//! Priority-aware Worker concurrency slots (`WorkerSlots`) and Worker prompt builder.

use crate::core::engine::EngineState;
use crate::core::model::{Assignment, ChannelAddress, OutboxStatus, Ticket, TicketInputKind};
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
        let target_chat_id = ticket.channels.iter().find_map(|ch| match ch {
            ChannelAddress::FeishuTopic { chat_id, .. } => Some(chat_id.as_str()),
            _ => None,
        });

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
            let same_chat = match (&tr.reply_channel, target_chat_id) {
                (Some(ChannelAddress::FeishuTopic { chat_id, .. }), Some(cid)) => chat_id == cid,
                _ => tr.source_id == ticket.mandate.source_id,
            };
            if same_chat && !tr.text.trim().is_empty() {
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
            let same_chat = match (&msg.channel, target_chat_id) {
                (ChannelAddress::FeishuTopic { chat_id, .. }, Some(cid)) => chat_id == cid,
                _ => true,
            };
            if same_chat && !msg.body.trim().is_empty() {
                timeline.push((
                    msg.created_at.clone(),
                    format!("- **[Assistant (奈克斯)]**: {}", msg.body.replace('\n', " ")),
                ));
            }
        }

        timeline.sort_by(|a, b| a.0.cmp(&b.0));
        let start = timeline.len().saturating_sub(16);
        let recent_lines: Vec<String> = timeline[start..].iter().map(|(_, line)| line.clone()).collect();
        if !recent_lines.is_empty() {
            sections.push(format!(
                "## Recent Conversation History in This Chat\n{}",
                recent_lines.join("\n")
            ));
        }
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
            "\n6. Maintain conversational continuity using `Recent Conversation History in This Chat` and `Persistent Long-Term Memory` above. When the user tells you personal details, preferences, or long-term project rules to remember, save or append them to `{}`.",
            p.display()
        ),
        None => String::new(),
    };

    sections.push(format!(
        "## Operating Rules & Shims\n\
         1. Use `gh-agent` for GitHub operations, `lark-bot` for Feishu operations, and `gws-agent` for Google Workspace.\n\
         2. When you need full spilled text for a truncated trigger or thread, run `nexus worker read <uri>` (or call `orchestrator_read`).\n\
         3. If a follow-up trigger in your inbox is unrelated to this Ticket, hand it back via `nexus worker handback <trigger_id> --reason \"...\"`.\n\
         4. Before running shell commands or inspecting/modifying files, first output a short 1-sentence Chinese thinking/progress note (<= 45 chars, e.g. \"我先查看一下仓库结构和最近提交记录。\") so Nexus can stream your live thinking process to the user in real time.\n\
         5. Your final response text is automatically captured and delivered to the user channel, or you may explicitly call `nexus worker report --status <ready|waiting|blocked|failed> --summary \"...\" [--pr-thread github:pr:owner/repo#N --pr-url https://...]` (or `worker_report`).\n\
         6. Keep any IM-facing summary or reply concise (strictly <= 90 words / characters).{}",
        memory_instruction
    ));

    sections.join("\n\n")
}
