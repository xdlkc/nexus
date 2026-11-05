//! Priority-aware Worker concurrency slots (`WorkerSlots`) and Worker prompt builder.

use super::history::HistoryScope;
use crate::core::engine::EngineState;
use crate::core::model::{Assignment, OutboxStatus, Ticket, TicketInputKind};
use std::collections::BTreeSet;
use std::path::Path;

/// Priority level of a Worker launch request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchPriority {
    /// New background or proactive ticket launch.
    General,
    /// Human follow-up message waking an existing subscribed ticket (may use reserved slot).
    HumanWakeup,
}

/// Manages concurrent Worker slots, reserving 1 slot for `HumanWakeup` when `max_slots >= 2`.
#[derive(Debug, Clone)]
pub struct WorkerSlots {
    max_slots: usize,
    active_assignments: BTreeSet<String>,
}

impl WorkerSlots {
    pub fn new(max_slots: usize) -> Self {
        Self {
            max_slots: max_slots.max(1),
            active_assignments: BTreeSet::new(),
        }
    }

    pub fn active_count(&self) -> usize {
        self.active_assignments.len()
    }

    pub fn max_slots(&self) -> usize {
        self.max_slots
    }

    pub fn is_running(&self, assignment_id: &str) -> bool {
        self.active_assignments.contains(assignment_id)
    }

    /// Checks whether a launch with the given priority can acquire a slot right now.
    pub fn can_acquire(&self, priority: LaunchPriority) -> bool {
        let used = self.active_assignments.len();
        match priority {
            LaunchPriority::HumanWakeup => used < self.max_slots,
            LaunchPriority::General => {
                let general_cap = if self.max_slots >= 2 {
                    self.max_slots - 1
                } else {
                    1
                };
                used < general_cap
            }
        }
    }

    /// Attempts to acquire a slot for `assignment_id`.
    pub fn try_acquire(&mut self, assignment_id: &str, priority: LaunchPriority) -> bool {
        if self.active_assignments.contains(assignment_id) {
            return true;
        }
        if !self.can_acquire(priority) {
            return false;
        }
        self.active_assignments.insert(assignment_id.to_owned());
        true
    }

    /// Releases a slot when a Worker assignment finishes.
    pub fn release(&mut self, assignment_id: &str) {
        self.active_assignments.remove(assignment_id);
    }
}

/// Determines the `LaunchPriority` of a Ticket's pending launch.
pub fn infer_launch_priority(ticket: &Ticket) -> LaunchPriority {
    if ticket.assignments.len() > 1
        && ticket
            .inputs
            .iter()
            .any(|inp| !inp.read_by_worker && inp.from_person)
    {
        LaunchPriority::HumanWakeup
    } else {
        LaunchPriority::General
    }
}

/// Builds the structured system & task prompt injected into any Worker Harness (`pi-native`,
/// `pi-rpc`, `claude-code`, `codex`, or `opencode`) when launching or resuming an Assignment.
pub fn build_worker_prompt(ticket: &Ticket, assignment: &Assignment) -> String {
    build_worker_prompt_with_memory(ticket, assignment, None, None)
}

/// Builds the Worker prompt enriched with chronological Chat Conversation History and Persistent Memory (`MEMORY.md`).
pub fn build_worker_prompt_with_memory(
    ticket: &Ticket,
    assignment: &Assignment,
    engine_state: Option<&EngineState>,
    state_dir: Option<&Path>,
) -> String {
    let mut sections = Vec::new();

    sections.push(format!(
        "# Nexus Worker Assignment `{}`\n\
         - **Ticket**: `{}` ({})\n\
         - **Repository**: `{}`\n\
         - **Worktree**: `{}` (branch `agent/{}`)\n\
         - **Target**: `{}:{}/{}:{}`",
        assignment.id,
        ticket.id,
        ticket.title,
        ticket.repo.as_deref().unwrap_or("default"),
        assignment.worktree_dir,
        ticket.short_name,
        assignment.target.harness.as_str(),
        assignment.target.provider,
        assignment.target.model,
        assignment.target.think_effort.as_str(),
    ));

    let abs_memory_path = state_dir.map(|d| {
        let base = std::fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf());
        base.join("MEMORY.md")
    });

    if let Some(mem_path) = &abs_memory_path
        && let Ok(mem_content) = std::fs::read_to_string(mem_path)
        && !mem_content.trim().is_empty()
    {
        let preview: String = mem_content.trim().chars().take(4_000).collect();
        sections.push(format!(
            "## Persistent Long-Term Memory (`{}`)\n{}",
            mem_path.display(),
            preview
        ));
    }

    if let Some(state) = engine_state {
        let scope = HistoryScope::new(ticket, state);

        let unread_trigger_ids: BTreeSet<&str> = ticket
            .inputs
            .iter()
            .filter(|i| !i.read_by_worker && i.handed_back_reason.is_none())
            .filter_map(|i| match &i.kind {
                TicketInputKind::TriggerFanOut { trigger_id, .. } => Some(trigger_id.as_str()),
                _ => None,
            })
            .collect();

        let mut timeline: Vec<(String, String)> = Vec::new();

        for tr in state.triggers.values() {
            if unread_trigger_ids.contains(tr.id.as_str()) {
                continue;
            }
            if scope.contains_trigger(tr) && !tr.text.trim().is_empty() {
                timeline.push((
                    tr.at.clone(),
                    format!(
                        "- **[User `{}`]**: {}",
                        tr.actor_id,
                        tr.text.replace('\n', " ")
                    ),
                ));
            }
        }

        for msg in state.outbox.values() {
            if msg.status != OutboxStatus::Sent {
                continue;
            }
            if scope.contains_address(&msg.channel) && !msg.body.trim().is_empty() {
                timeline.push((
                    msg.created_at.clone(),
                    format!(
                        "- **[Assistant (奈克斯)]**: {}",
                        msg.body.replace('\n', " ")
                    ),
                ));
            }
        }

        timeline.sort_by(|a, b| a.0.cmp(&b.0));
        let start = timeline.len().saturating_sub(16);
        let recent_lines: Vec<String> = timeline[start..]
            .iter()
            .map(|(_, line)| line.clone())
            .collect();
        if !recent_lines.is_empty() {
            sections.push(format!(
                "## Recent Conversation History in This Chat\n{}",
                recent_lines.join("\n")
            ));
        }
    }

    // Include prior ticket inputs and previous assignment reports when resuming a ticket across multiple turns.
    let prior_inputs: Vec<_> = ticket
        .inputs
        .iter()
        .filter(|i| i.read_by_worker && i.handed_back_reason.is_none())
        .collect();
    let prior_assignments: Vec<_> = ticket
        .assignments
        .iter()
        .filter(|a| a.id != assignment.id && a.latest_report.is_some())
        .collect();
    if !prior_inputs.is_empty() || !prior_assignments.is_empty() {
        let mut prior_lines = vec!["## Prior Ticket History & Completed Assignments".to_owned()];
        for inp in prior_inputs {
            if let TicketInputKind::TriggerFanOut {
                trigger_id,
                thread_key,
                text_preview,
            } = &inp.kind
            {
                prior_lines.push(format!(
                    "- **[Prior Input `{}` | Trigger `trigger/{}` | Thread `{}`]**:\n  {}",
                    inp.id, trigger_id, thread_key, text_preview
                ));
            }
        }
        for prev_asg in prior_assignments {
            if let Some(rep) = &prev_asg.latest_report {
                prior_lines.push(format!(
                    "- **[Prior Assignment `{}` (`{:?}`)]**: {}",
                    prev_asg.id, rep.status, rep.summary
                ));
            }
        }
        sections.push(prior_lines.join("\n"));
    }

    // Include any unread inputs (including cross-harness Handoff cards and human follow-ups).
    let unread: Vec<_> = ticket
        .inputs
        .iter()
        .filter(|i| !i.read_by_worker && i.handed_back_reason.is_none())
        .collect();

    if !unread.is_empty() {
        let mut input_lines = vec!["## Unread Ticket Inputs".to_owned()];
        for inp in unread {
            match &inp.kind {
                TicketInputKind::TriggerFanOut {
                    trigger_id,
                    thread_key,
                    text_preview,
                } => {
                    input_lines.push(format!(
                        "- **[Input `{}` | Trigger `trigger/{}` | Thread `{}`]**:\n  {}",
                        inp.id, trigger_id, thread_key, text_preview
                    ));
                }
                TicketInputKind::Handoff {
                    previous_assignment_id,
                    previous_target,
                    previous_summary,
                    git_status_diff,
                } => {
                    input_lines.push(format!(
                        "### Cross-Harness Handoff Card (from `{}` running `{}:{}/{}:{}`)\n\
                         - **Prior Summary**: {}\n\
                         - **Worktree Git Status / Diff**:\n```\n{}\n```",
                        previous_assignment_id,
                        previous_target.harness.as_str(),
                        previous_target.provider,
                        previous_target.model,
                        previous_target.think_effort.as_str(),
                        previous_summary,
                        git_status_diff,
                    ));
                }
                TicketInputKind::RouterNote { text } => {
                    input_lines.push(format!("- **[Router Note]**: {text}"));
                }
                TicketInputKind::PolicyChanged {
                    old_version,
                    new_version,
                    detail,
                } => {
                    input_lines.push(format!(
                        "- **[Policy Changed `{old_version}` -> `{new_version}`]**: {detail}"
                    ));
                }
            }
        }
        sections.push(input_lines.join("\n"));
    }

    let memory_instruction = match abs_memory_path {
        Some(p) => format!(
            "\n7. Maintain conversational continuity using `Recent Conversation History in This Chat` and `Persistent Long-Term Memory` above. When the user tells you personal details, preferences, long-term project rules, or when you reach agreed conclusions, decisions, or ideas during discussions, proactively save or append them to `{}`.",
            p.display()
        ),
        None => String::new(),
    };

    sections.push(format!(
        "## Operating Rules & Shims\n\
         1. Use `gh-agent` for GitHub operations, `lark-bot` for Feishu operations, `dws-agent` for DingTalk operations, and `gws-agent` for Google Workspace:\n\
            - **Rich Text (Markdown)**: Both DingTalk (`dws-agent`) and Feishu (`lark-bot`) render Markdown rich text (`**bold**`, `### headings`, lists, code blocks, links, `![alt](https://...)` web images) directly in your final reply.\n\
            - **Images & Files in DingTalk (`dws-agent`)**:\n\
              - Web image as bot: `dws-agent chat message send-by-bot --robot-code dingztmnesaskuiyqfup --conversation-id <cid> --msg-type image --image-url \"https://...\" -y`\n\
              - Local file as bot: `dws-agent chat message send-by-bot --robot-code dingztmnesaskuiyqfup --conversation-id <cid> --msg-type file --file-path ./relative/path -y`\n\
              - Rich text with inline local image: include `![说明](./relative/image.png)` in your reply or run `dws-agent chat +messages-send --as user --chat-id <cid> --markdown \"![说明](./relative/image.png)\" --idempotency-key <key> -y`\n\
            - **Images & Files in Feishu (`lark-bot`)**:\n\
              - Local or web image as bot: `lark-bot im +messages-send --as bot --chat-id <oc_id> --image ./relative/image.png`\n\
              - Rich text (`post`) as bot: `lark-bot im +messages-send --as bot --chat-id <oc_id> --markdown \"### Title\\n- item\"`\n\
         2. When you need full spilled text for a truncated trigger or thread, run `nexus worker read <uri>` (or call `orchestrator_read`).\n\
         3. If a follow-up trigger in your inbox is unrelated to this Ticket, hand it back via `nexus worker handback <trigger_id> --reason \"...\"`.\n\
         4. Before running shell commands or inspecting/modifying files, first output a short 1-sentence Chinese thinking/progress note (<= 45 chars, e.g. \"我先查看一下仓库结构和最近提交记录。\") so Nexus can stream your live thinking process to the user in real time.\n\
         5. When solving a GitHub Issue or implementing a feature/bugfix in repository `{}`:\n\
            - Implement the change cleanly in your current worktree branch (`agent/{}`).\n\
            - Commit your changes (`git add -A && git commit -m \"...\"`), push the branch (`git push -u origin HEAD`), and open a Pull Request via `gh-agent pr create --repo {} --head agent/{} --base main --title \"...\" --body \"Closes #<issue_number>\\n\\n...\"`.\n\
            - Always include the full PR URL (`https://github.com/.../pull/N`) in your final summary.\n\
         6. Your final response text is automatically captured and delivered to the user channel, or you may explicitly call `nexus worker report --status <ready|waiting|blocked|failed> --summary \"...\" [--pr-thread github:pr:owner/repo#N --pr-url https://...]` (or `worker_report`).\n\
         7. Keep any IM-facing summary or reply concise (strictly <= 90 words / characters).{}",
        ticket.repo.as_deref().unwrap_or("xdlkc/nexus"),
        ticket.short_name,
        ticket.repo.as_deref().unwrap_or("xdlkc/nexus"),
        ticket.short_name,
        memory_instruction
    ));

    sections.join("\n\n")
}
