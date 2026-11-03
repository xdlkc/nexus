//! Priority-aware Worker concurrency slots (`WorkerSlots`) and Worker prompt builder.

use crate::core::model::{Assignment, Ticket, TicketInputKind};
use std::collections::BTreeSet;

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

    sections.push(
        "## Operating Rules & Shims\n\
         1. Use `gh-agent` for GitHub operations, `lark-bot` for Feishu operations, and `gws-agent` for Google Workspace.\n\
         2. When you need full spilled text for a truncated trigger or thread, run `nexus worker read <uri>` (or call `orchestrator_read`).\n\
         3. If a follow-up trigger in your inbox is unrelated to this Ticket, hand it back via `nexus worker handback <trigger_id> --reason \"...\"`.\n\
         4. Report your outcome via `nexus worker report --status <ready|waiting|blocked|failed> --summary \"...\" [--pr-thread github:pr:owner/repo#N --pr-url https://...]` (or call `worker_report`).\n\
         5. Keep any IM-facing summary or question strictly <= 100 words."
            .to_owned(),
    );

    sections.join("\n\n")
}
