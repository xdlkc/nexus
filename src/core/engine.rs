//! Core deterministic domain state machine (`Engine`).

use crate::core::model::{
    AcceptMode, AdmissionStrength, Assignment, AssignmentReport, AssignmentStatus, ChannelAddress,
    Decision, DecisionOutcome, Effect, EffectKind, ExecutionTarget, FollowedThread, Mandate,
    OutboxMessage, OutboxPurpose, OutboxStatus, ReportStatus, Thread, ThreadCloseOutcome, Ticket,
    TicketEnd, TicketInput, TicketInputKind, Trigger,
};
use crate::core::recall::{
    RecallCaller, RecallSnapshot, read_resource, truncate_to_im_word_limit, validate_im_word_limit,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Result of receiving a Trigger into the Engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeDisposition {
    Duplicate,
    FilteredOut,
    FannedOutToTickets { ticket_ids: Vec<String> },
    QueuedForEval { thread_key: String, batch_size: usize },
}

/// Complete serializable state of the Engine (persisted in SQLite or memory).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineState {
    pub policy_version: String,
    pub accept_mode: Option<AcceptMode>,
    pub worktree_root: String,
    pub next_seq: u64,
    pub triggers: BTreeMap<String, Trigger>,
    pub external_index: BTreeMap<String, String>,
    pub threads: BTreeMap<String, Thread>,
    pub tickets: BTreeMap<String, Ticket>,
    pub decisions: BTreeMap<String, Decision>,
    pub outbox: BTreeMap<String, OutboxMessage>,
    pub effects: Vec<Effect>,
}

/// Pure, deterministic state machine owning all domain aggregates.
pub struct Engine {
    pub state: EngineState,
}

impl Engine {
    pub fn new(worktree_root: impl Into<String>) -> Self {
        Self {
            state: EngineState {
                policy_version: "v1".to_owned(),
                accept_mode: Some(AcceptMode::Observe),
                worktree_root: worktree_root.into(),
                next_seq: 1,
                ..EngineState::default()
            },
        }
    }

    pub fn from_state(state: EngineState) -> Self {
        Self { state }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}-{}", self.state.next_seq);
        self.state.next_seq += 1;
        id
    }

    fn enqueue_effect(&mut self, key: impl Into<String>, kind: EffectKind, now: &str) {
        let key_str = key.into();
        if let Some(existing) = self.state.effects.iter_mut().find(|e| e.key == key_str) {
            existing.kind = kind;
            return;
        }
        let id = self.next_id("eff");
        self.state.effects.push(Effect {
            id,
            key: key_str,
            kind,
            attempts: 0,
            last_error: None,
            created_at: now.to_owned(),
        });
    }

    /// Receives a Trigger that already passed Tier-0 Gate admission check.
    pub fn receive_trigger(
        &mut self,
        trigger: Trigger,
        strength: AdmissionStrength,
    ) -> IntakeDisposition {
        if strength == AdmissionStrength::NotAdmitted {
            return IntakeDisposition::FilteredOut;
        }

        let ext_key = format!("{}:{}", trigger.source_id, trigger.external_id);
        if self.state.external_index.contains_key(&ext_key) {
            return IntakeDisposition::Duplicate;
        }

        let trigger_id = trigger.id.clone();
        let thread_key = trigger.thread_key.clone();
        let from_person = !trigger.actor_is_bot;
        let now = trigger.at.clone();
        let text_preview: String = trigger.text.chars().take(600).collect();

        self.state
            .external_index
            .insert(ext_key, trigger_id.clone());
        self.state
            .triggers
            .insert(trigger_id.clone(), trigger.clone());

        let thread = self
            .state
            .threads
            .entry(thread_key.clone())
            .or_insert_with(|| Thread {
                thread_key: thread_key.clone(),
                source_id: trigger.source_id.clone(),
                source_kind: trigger.source_kind,
                label: thread_key.clone(),
                trigger_ids: Vec::new(),
                subscribed_tickets: BTreeSet::new(),
                context_only_tickets: BTreeSet::new(),
                queued_trigger_ids: Vec::new(),
                decision_ids: Vec::new(),
                closed_outcome: None,
                meta: BTreeMap::new(),
            });
        thread.trigger_ids.push(trigger_id.clone());

        let subscribed_ids: Vec<String> = thread.subscribed_tickets.iter().cloned().collect();
        let context_ids: Vec<String> = thread.context_only_tickets.iter().cloned().collect();

        let mut owned_by_tickets = Vec::new();

        // 1. Fan out to active/resumable ownership subscriptions.
        for ticket_id in subscribed_ids {
            let can_deliver = match self.state.tickets.get(&ticket_id) {
                Some(t) if t.is_open() => true,
                Some(t) => {
                    // Ended ticket: only a human message resumes it, provided a person did not manually stop it.
                    from_person
                        && !t
                            .ended
                            .as_ref()
                            .map(|e| e.by_person_stop)
                            .unwrap_or(false)
                }
                None => false,
            };

            if !can_deliver {
                continue;
            }

            // If user embedded an inline target override (e.g. `[codex:gpt-5.6-sol:xhigh]`) on a follow-up,
            // switch the Ticket's execution target automatically!
            if from_person
                && let Some(partial) = trigger.inline_target.clone()
                    && !partial.is_empty() {
                        let base = self.state.tickets[&ticket_id].current_target();
                        let new_target = partial.apply_to(&base);
                        let _ = self.switch_execution_target(
                            &ticket_id,
                            new_target,
                            "Inline target directive on follow-up message",
                            "",
                            &now,
                        );
                    }

            let input_id = self.next_id("inp");
            let mut resumed_assignment_to_launch: Option<String> = None;
            let mut running_assignment_id: Option<String> = None;

            if let Some(ticket) = self.state.tickets.get_mut(&ticket_id) {
                if ticket.ended.is_some() && from_person {
                    ticket.ended = None;
                }
                if let Some(ch) = &trigger.reply_channel
                    && !ticket.channels.contains(ch) {
                        ticket.channels.push(ch.clone());
                    }
                ticket.inputs.push(TicketInput {
                    id: input_id.clone(),
                    kind: TicketInputKind::TriggerFanOut {
                        trigger_id: trigger_id.clone(),
                        thread_key: thread_key.clone(),
                        text_preview: text_preview.clone(),
                    },
                    from_person,
                    read_by_worker: false,
                    handed_back_reason: None,
                    at: now.clone(),
                });

                let need_new_assignment = match ticket.latest_assignment() {
                    None => true,
                    Some(a) => a.status == AssignmentStatus::Ended,
                };
                if need_new_assignment {
                    let target = ticket.current_target();
                    let worktree_dir = ticket
                        .latest_assignment()
                        .map(|a| a.worktree_dir.clone())
                        .unwrap_or_else(|| format!("{}/{}", self.state.worktree_root, ticket.short_name));
                    let assign_id = format!("asg-{}-{}", ticket.id, ticket.assignments.len() + 1);
                    ticket.assignments.push(Assignment {
                        id: assign_id.clone(),
                        ticket_id: ticket.id.clone(),
                        target,
                        status: AssignmentStatus::Pending,
                        worktree_dir,
                        started_at: now.clone(),
                        ended_at: None,
                        latest_report: None,
                    });
                    resumed_assignment_to_launch = Some(assign_id);
                } else if let Some(a) = ticket.latest_assignment() {
                    running_assignment_id = Some(a.id.clone());
                }
            }

            if let Some(assign_id) = resumed_assignment_to_launch {
                self.enqueue_effect(
                    format!("launch:{ticket_id}"),
                    EffectKind::LaunchAssignment {
                        ticket_id: ticket_id.clone(),
                        assignment_id: assign_id,
                    },
                    &now,
                );
            } else if let Some(assign_id) = running_assignment_id {
                self.enqueue_effect(
                    format!("deliver:{ticket_id}"),
                    EffectKind::DeliverInputs {
                        ticket_id: ticket_id.clone(),
                        assignment_id: assign_id,
                        input_ids: vec![input_id],
                    },
                    &now,
                );
            }

            if from_person {
                owned_by_tickets.push(ticket_id);
            }
        }

        // 2. Fan out to context-only subscriptions (grants no ownership and does not resume ended tickets).
        for ticket_id in context_ids {
            let is_open = self
                .state
                .tickets
                .get(&ticket_id)
                .map(Ticket::is_open)
                .unwrap_or(false);
            if !is_open {
                continue;
            }
            let input_id = self.next_id("inp");
            let mut running_assign_id = None;
            if let Some(ticket) = self.state.tickets.get_mut(&ticket_id) {
                ticket.inputs.push(TicketInput {
                    id: input_id.clone(),
                    kind: TicketInputKind::TriggerFanOut {
                        trigger_id: trigger_id.clone(),
                        thread_key: thread_key.clone(),
                        text_preview: text_preview.clone(),
                    },
                    from_person,
                    read_by_worker: false,
                    handed_back_reason: None,
                    at: now.clone(),
                });
                running_assign_id = ticket.latest_assignment().map(|a| a.id.clone());
            }
            if let Some(assign_id) = running_assign_id {
                self.enqueue_effect(
                    format!("deliver:{ticket_id}"),
                    EffectKind::DeliverInputs {
                        ticket_id: ticket_id.clone(),
                        assignment_id: assign_id,
                        input_ids: vec![input_id],
                    },
                    &now,
                );
            }
        }

        // A human message that reached an owning Ticket stays with that Ticket (bypasses Eval).
        if !owned_by_tickets.is_empty() {
            return IntakeDisposition::FannedOutToTickets {
                ticket_ids: owned_by_tickets,
            };
        }

        // Otherwise queue for Eval batch on the Thread.
        let batch_ids = {
            let thread = self.state.threads.get_mut(&thread_key).unwrap();
            thread.queued_trigger_ids.push(trigger_id);
            thread.queued_trigger_ids.clone()
        };
        let batch_size = batch_ids.len();
        self.enqueue_effect(
            format!("eval:{thread_key}"),
            EffectKind::RunEval {
                thread_key: thread_key.clone(),
                trigger_ids: batch_ids,
            },
            &now,
        );

        IntakeDisposition::QueuedForEval {
            thread_key,
            batch_size,
        }
    }

    /// Accepts a candidate batch and atomically creates a Ticket, Mandate, Subscription, Assignment, and optional Acknowledgement.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_ticket(
        &mut self,
        origin_trigger_id: &str,
        title: impl Into<String>,
        repo: Option<String>,
        strength: AdmissionStrength,
        matched_rule_ids: Vec<String>,
        target: ExecutionTarget,
        acknowledgement: Option<&str>,
        now: &str,
    ) -> Result<String, String> {
        let trigger = self
            .state
            .triggers
            .get(origin_trigger_id)
            .cloned()
            .ok_or_else(|| format!("Origin trigger {origin_trigger_id} not found"))?;

        if let Some(ack_text) = acknowledgement
            && trigger
                .reply_channel
                .as_ref()
                .map(ChannelAddress::is_low_noise_im)
                .unwrap_or(false)
            {
                validate_im_word_limit(ack_text)?;
            }

        let ticket_id = self.next_id("tkt");
        let short_name = format!("work-{}", &ticket_id["tkt-".len()..]);
        let worktree_dir = format!("{}/{}", self.state.worktree_root, short_name);
        let assign_id = format!("asg-{ticket_id}-1");
        let thread_key = trigger.thread_key.clone();

        let mandate = Mandate {
            policy_version: self.state.policy_version.clone(),
            strength,
            source_id: trigger.source_id.clone(),
            sender_id: trigger.actor_id.clone(),
            matched_rule_ids,
            granted_at: now.to_owned(),
        };

        let mut subscriptions = BTreeSet::new();
        subscriptions.insert(thread_key.clone());

        let mut channels = Vec::new();
        if let Some(ch) = trigger.reply_channel.clone() {
            channels.push(ch);
        }

        let assignment = Assignment {
            id: assign_id.clone(),
            ticket_id: ticket_id.clone(),
            target: target.clone(),
            status: AssignmentStatus::Pending,
            worktree_dir,
            started_at: now.to_owned(),
            ended_at: None,
            latest_report: None,
        };

        let input_id = self.next_id("inp");
        let initial_input = TicketInput {
            id: input_id,
            kind: TicketInputKind::TriggerFanOut {
                trigger_id: trigger.id.clone(),
                thread_key: thread_key.clone(),
                text_preview: trigger.text.chars().take(1_200).collect(),
            },
            from_person: !trigger.actor_is_bot,
            read_by_worker: false,
            handed_back_reason: None,
            at: now.to_owned(),
        };

        let ticket = Ticket {
            id: ticket_id.clone(),
            short_name,
            title: title.into(),
            repo,
            origin_trigger_id: trigger.id.clone(),
            origin_thread_key: thread_key.clone(),
            mandate,
            channels,
            subscriptions,
            context_subscriptions: BTreeSet::new(),
            follows: Vec::new(),
            inputs: vec![initial_input],
            assignments: vec![assignment],
            delivery_error: None,
            created_at: now.to_owned(),
            ended: None,
        };

        self.state.tickets.insert(ticket_id.clone(), ticket);

        let dec_id = self.next_id("dec");
        let queued = if let Some(thread) = self.state.threads.get_mut(&thread_key) {
            thread.subscribed_tickets.insert(ticket_id.clone());
            thread.decision_ids.push(dec_id.clone());
            std::mem::take(&mut thread.queued_trigger_ids)
        } else {
            vec![origin_trigger_id.to_owned()]
        };

        self.state.decisions.insert(
            dec_id.clone(),
            Decision {
                id: dec_id,
                thread_key,
                trigger_ids: if queued.is_empty() {
                    vec![origin_trigger_id.to_owned()]
                } else {
                    queued
                },
                outcome: DecisionOutcome::Accept,
                reason: "Accepted work item".to_owned(),
                ticket_id: Some(ticket_id.clone()),
                would_accept_rule_ids: Vec::new(),
                target: Some(target),
                at: now.to_owned(),
            },
        );

        if let (Some(ack_text), Some(ch)) = (acknowledgement, trigger.reply_channel) {
            self.queue_outbox_message(
                Some(ticket_id.clone()),
                ch,
                OutboxPurpose::Acknowledgement,
                ack_text,
                now,
            )?;
        }

        self.enqueue_effect(
            format!("launch:{ticket_id}"),
            EffectKind::LaunchAssignment {
                ticket_id: ticket_id.clone(),
                assignment_id: assign_id,
            },
            now,
        );

        Ok(ticket_id)
    }

    /// Switches a Ticket's execution target (`Harness × Model × ThinkEffort`) mid-flight,
    /// preserving its `git worktree` and injecting a structured Handoff Card.
    pub fn switch_execution_target(
        &mut self,
        ticket_id: &str,
        new_target: ExecutionTarget,
        reason: &str,
        git_status_diff: &str,
        now: &str,
    ) -> Result<String, String> {
        let handoff_input_id = self.next_id("inp");
        let ticket = self
            .state
            .tickets
            .get_mut(ticket_id)
            .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;

        if !ticket.is_open() {
            return Err(format!("Cannot switch target on ended ticket {ticket_id}"));
        }

        let (prev_id, prev_target, prev_summary, worktree_dir) =
            if let Some(prev) = ticket.latest_assignment_mut() {
                prev.status = AssignmentStatus::Ended;
                prev.ended_at = Some(now.to_owned());
                let summary = prev
                    .latest_report
                    .as_ref()
                    .map(|r| r.summary.clone())
                    .unwrap_or_else(|| format!("Switched target: {reason}"));
                (
                    prev.id.clone(),
                    prev.target.clone(),
                    summary,
                    prev.worktree_dir.clone(),
                )
            } else {
                (
                    "none".to_owned(),
                    ticket.current_target(),
                    reason.to_owned(),
                    format!("{}/{}", self.state.worktree_root, ticket.short_name),
                )
            };

        ticket.inputs.push(TicketInput {
            id: handoff_input_id,
            kind: TicketInputKind::Handoff {
                previous_assignment_id: prev_id,
                previous_target: prev_target,
                previous_summary: prev_summary,
                git_status_diff: git_status_diff.to_owned(),
            },
            from_person: false,
            read_by_worker: false,
            handed_back_reason: None,
            at: now.to_owned(),
        });

        let new_assign_id = format!("asg-{ticket_id}-{}", ticket.assignments.len() + 1);
        ticket.assignments.push(Assignment {
            id: new_assign_id.clone(),
            ticket_id: ticket_id.to_owned(),
            target: new_target,
            status: AssignmentStatus::Pending,
            worktree_dir,
            started_at: now.to_owned(),
            ended_at: None,
            latest_report: None,
        });
        ticket.delivery_error = None;

        self.enqueue_effect(
            format!("launch:{ticket_id}"),
            EffectKind::LaunchAssignment {
                ticket_id: ticket_id.to_owned(),
                assignment_id: new_assign_id.clone(),
            },
            now,
        );

        Ok(new_assign_id)
    }

    /// Stops an open Ticket on human request and withdraws its Mandate.
    pub fn stop_ticket(&mut self, ticket_id: &str, reason: &str, now: &str) -> Result<(), String> {
        let ticket = self
            .state
            .tickets
            .get_mut(ticket_id)
            .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;

        if let Some(assign) = ticket.latest_assignment_mut() {
            assign.status = AssignmentStatus::Ended;
            assign.ended_at = Some(now.to_owned());
        }
        ticket.ended = Some(TicketEnd {
            outcome: "stopped".to_owned(),
            by_person_stop: true,
            reason: reason.to_owned(),
            at: now.to_owned(),
        });
        Ok(())
    }

    /// Records a Worker's structured Report, updating Ticket stage, binding PRs, and queuing outbound messages.
    #[allow(clippy::too_many_arguments)]
    pub fn record_worker_report(
        &mut self,
        ticket_id: &str,
        status: ReportStatus,
        summary: impl Into<String>,
        question: Option<String>,
        pull_request_thread_key: Option<String>,
        pull_request_url: Option<String>,
        now: &str,
    ) -> Result<(), String> {
        let summary_str = summary.into();

        if let Some(pr_thread) = &pull_request_thread_key {
            self.bind_pull_request(ticket_id, pr_thread, true);
        }

        let mut should_send_message: Option<(ChannelAddress, OutboxPurpose, String)> = None;

        {
            let ticket = self
                .state
                .tickets
                .get_mut(ticket_id)
                .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;

            for inp in &mut ticket.inputs {
                inp.read_by_worker = true;
            }

            if let Some(assign) = ticket.latest_assignment_mut() {
                if matches!(
                    status,
                    ReportStatus::Waiting
                        | ReportStatus::Ready
                        | ReportStatus::Blocked
                        | ReportStatus::Failed
                        | ReportStatus::Dropped
                ) {
                    assign.status = AssignmentStatus::Ended;
                    assign.ended_at = Some(now.to_owned());
                } else {
                    assign.status = AssignmentStatus::Running;
                }
                assign.latest_report = Some(AssignmentReport {
                    status,
                    summary: summary_str.clone(),
                    question: question.clone(),
                    pull_request_url,
                    at: now.to_owned(),
                });
            }

            if status == ReportStatus::Dropped {
                ticket.ended = Some(TicketEnd {
                    outcome: "dropped".to_owned(),
                    by_person_stop: false,
                    reason: summary_str,
                    at: now.to_owned(),
                });
                return Ok(());
            }

            if status == ReportStatus::Ready && (ticket.follows.is_empty() || ticket.follows_done())
            {
                ticket.ended = Some(TicketEnd {
                    outcome: "completed".to_owned(),
                    by_person_stop: false,
                    reason: "Work completed".to_owned(),
                    at: now.to_owned(),
                });
            }

            if let Some(ch) = ticket.channels.first().cloned() {
                if let Some(q) = question.filter(|s| !s.trim().is_empty()) {
                    should_send_message = Some((ch, OutboxPurpose::Attention, q));
                } else if status == ReportStatus::Ready {
                    should_send_message = Some((ch, OutboxPurpose::Result, summary_str));
                } else if matches!(
                    status,
                    ReportStatus::Waiting | ReportStatus::Blocked | ReportStatus::Failed
                ) && !summary_str.trim().is_empty()
                {
                    should_send_message = Some((ch, OutboxPurpose::Attention, summary_str));
                }
            }
        }

        if let Some((ch, purpose, body)) = should_send_message {
            let safe_body = if ch.is_low_noise_im() {
                truncate_to_im_word_limit(&body, 95)
            } else {
                body
            };
            let _ =
                self.queue_outbox_message(Some(ticket_id.to_owned()), ch, purpose, &safe_body, now);
        }

        Ok(())
    }

    /// Hands back an out-of-scope follow-up Trigger from a Worker, re-queueing it for Eval.
    pub fn handback_trigger(
        &mut self,
        ticket_id: &str,
        trigger_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<bool, String> {
        let trigger = self
            .state
            .triggers
            .get(trigger_id)
            .cloned()
            .ok_or_else(|| format!("Trigger {trigger_id} not found"))?;

        let ticket = self
            .state
            .tickets
            .get_mut(ticket_id)
            .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;

        let mut found = false;
        for inp in &mut ticket.inputs {
            if let TicketInputKind::TriggerFanOut {
                trigger_id: tid, ..
            } = &inp.kind
                && tid == trigger_id && inp.handed_back_reason.is_none() {
                    inp.handed_back_reason = Some(reason.to_owned());
                    found = true;
                }
        }
        if !found {
            return Ok(false);
        }

        if let Some(thread) = self.state.threads.get_mut(&trigger.thread_key) {
            if !thread.queued_trigger_ids.contains(&trigger.id) {
                thread.queued_trigger_ids.push(trigger.id.clone());
            }
            let batch = thread.queued_trigger_ids.clone();
            self.enqueue_effect(
                format!("eval:{}", trigger.thread_key),
                EffectKind::RunEval {
                    thread_key: trigger.thread_key.clone(),
                    trigger_ids: batch,
                },
                now,
            );
        }

        Ok(true)
    }

    /// Binds a pull request Thread to a Ticket so closing/merging the PR completes the Ticket.
    pub fn bind_pull_request(&mut self, ticket_id: &str, pr_thread_key: &str, own: bool) {
        if self
            .state
            .threads
            .get(pr_thread_key)
            .is_some_and(|t| t.closed_outcome.is_some())
        {
            return;
        }
        if let Some(ticket) = self.state.tickets.get_mut(ticket_id) {
            ticket.subscriptions.insert(pr_thread_key.to_owned());
            if let Some(existing) = ticket
                .follows
                .iter_mut()
                .find(|f| f.thread_key == pr_thread_key)
            {
                existing.own |= own;
                existing.closed = None;
            } else {
                ticket.follows.push(FollowedThread {
                    thread_key: pr_thread_key.to_owned(),
                    own,
                    closed: None,
                });
            }
        }

        let thread = self
            .state
            .threads
            .entry(pr_thread_key.to_owned())
            .or_insert_with(|| Thread {
                thread_key: pr_thread_key.to_owned(),
                source_id: "github".to_owned(),
                source_kind: crate::core::model::SourceKindId::Github,
                label: pr_thread_key.to_owned(),
                trigger_ids: Vec::new(),
                subscribed_tickets: BTreeSet::new(),
                context_only_tickets: BTreeSet::new(),
                queued_trigger_ids: Vec::new(),
                decision_ids: Vec::new(),
                closed_outcome: None,
                meta: BTreeMap::new(),
            });
        thread.subscribed_tickets.insert(ticket_id.to_owned());
    }

    /// Marks an external Thread (such as a GitHub PR) closed or merged, completing any Tickets waiting on it.
    pub fn thread_closed(&mut self, thread_key: &str, outcome: &str, now: &str) {
        let close_record = ThreadCloseOutcome {
            outcome: outcome.to_owned(),
            at: now.to_owned(),
        };

        if let Some(thread) = self.state.threads.get_mut(thread_key) {
            thread.closed_outcome = Some(close_record.clone());
        }

        let ticket_ids: Vec<String> = self.state.tickets.keys().cloned().collect();
        for tid in ticket_ids {
            let mut completed_notice: Option<(ChannelAddress, String)> = None;
            let mut resumed_assignment_to_launch: Option<String> = None;
            let inp_id = self.next_id("inp");
            if let Some(ticket) = self.state.tickets.get_mut(&tid) {
                if !ticket.is_open() {
                    continue;
                }
                let mut touched = false;
                for f in &mut ticket.follows {
                    if f.thread_key == thread_key && f.closed.is_none() {
                        f.closed = Some(close_record.clone());
                        touched = true;
                    }
                }
                if touched && ticket.follows_done() {
                    let was_waiting_for_post_merge = ticket
                        .latest_assignment()
                        .and_then(|a| a.latest_report.as_ref())
                        .is_some_and(|r| r.status == ReportStatus::Waiting);

                    if was_waiting_for_post_merge {
                        ticket.inputs.push(TicketInput {
                            id: inp_id,
                            kind: TicketInputKind::RouterNote {
                                text: format!(
                                    "Followed PR `{thread_key}` is now `{outcome}`. Please proceed with the remaining post-merge steps on `origin/main` (e.g. fetch `origin/main`, create & push the release tag, wait for or publish the formal GitHub Release) and call `worker_report` with `status=\"ready\"` when finished."
                                ),
                            },
                            from_person: true,
                            read_by_worker: false,
                            handed_back_reason: None,
                            at: now.to_owned(),
                        });
                        let target = ticket.current_target();
                        let worktree_dir = ticket
                            .latest_assignment()
                            .map(|a| a.worktree_dir.clone())
                            .unwrap_or_else(|| {
                                format!("{}/{}", self.state.worktree_root, ticket.short_name)
                            });
                        let assign_id =
                            format!("asg-{}-{}", ticket.id, ticket.assignments.len() + 1);
                        ticket.assignments.push(Assignment {
                            id: assign_id.clone(),
                            ticket_id: ticket.id.clone(),
                            target,
                            status: AssignmentStatus::Pending,
                            worktree_dir,
                            started_at: now.to_owned(),
                            ended_at: None,
                            latest_report: None,
                        });
                        resumed_assignment_to_launch = Some(assign_id);
                    } else {
                        ticket.ended = Some(TicketEnd {
                            outcome: outcome.to_owned(),
                            by_person_stop: false,
                            reason: format!("Followed thread {thread_key} {outcome}"),
                            at: now.to_owned(),
                        });
                        if let Some(reply_ch) = ticket
                            .channels
                            .iter()
                            .find(|c| c.is_low_noise_im())
                            .or_else(|| ticket.channels.first())
                            .cloned()
                        {
                            completed_notice =
                                Some((reply_ch, format!("PR {thread_key} is now {outcome}.")));
                        }
                    }
                }
            }
            if let Some(assign_id) = resumed_assignment_to_launch {
                self.enqueue_effect(
                    format!("launch:{tid}"),
                    EffectKind::LaunchAssignment {
                        ticket_id: tid.clone(),
                        assignment_id: assign_id,
                    },
                    now,
                );
            }
            if let Some((ch, msg)) = completed_notice {
                let _ = self.queue_outbox_message(
                    Some(tid),
                    ch,
                    OutboxPurpose::PullEndNotice,
                    &msg,
                    now,
                );
            }
        }
    }

    /// Queues an outbound message in the transactional Outbox.
    pub fn queue_outbox_message(
        &mut self,
        ticket_id: Option<String>,
        channel: ChannelAddress,
        purpose: OutboxPurpose,
        body: &str,
        now: &str,
    ) -> Result<String, String> {
        if channel.is_low_noise_im() {
            validate_im_word_limit(body)?;
        }
        let outbox_id = self.next_id("out");
        self.state.outbox.insert(
            outbox_id.clone(),
            OutboxMessage {
                id: outbox_id.clone(),
                ticket_id,
                channel,
                purpose,
                body: body.to_owned(),
                status: OutboxStatus::Pending,
                attempts: 0,
                last_error: None,
                created_at: now.to_owned(),
            },
        );
        self.enqueue_effect(
            format!("outbox:{outbox_id}"),
            EffectKind::SendOutbox {
                outbox_id: outbox_id.clone(),
            },
            now,
        );
        Ok(outbox_id)
    }

    /// Resolves a Chain of Recall resource URI against the current Engine state.
    pub fn read_recall_resource(
        &self,
        caller: &RecallCaller,
        uri: &str,
    ) -> Result<serde_json::Value, String> {
        let snapshot = RecallSnapshot {
            triggers: &self.state.triggers,
            threads: &self.state.threads,
            tickets: &self.state.tickets,
            decisions: &self.state.decisions,
        };
        read_resource(&snapshot, caller, uri)
    }

    /// Records an Ignore (or Observe-mode shadow) decision for queued triggers on a Thread.
    pub fn ignore_thread_triggers(
        &mut self,
        thread_key: &str,
        reason: impl Into<String>,
        would_accept_rule_ids: Vec<String>,
        recommended_target: Option<ExecutionTarget>,
        now: &str,
    ) -> String {
        let dec_id = self.next_id("dec");
        let trigger_ids = if let Some(thread) = self.state.threads.get_mut(thread_key) {
            thread.decision_ids.push(dec_id.clone());
            std::mem::take(&mut thread.queued_trigger_ids)
        } else {
            Vec::new()
        };
        self.state.decisions.insert(
            dec_id.clone(),
            Decision {
                id: dec_id.clone(),
                thread_key: thread_key.to_owned(),
                trigger_ids,
                outcome: DecisionOutcome::Ignore,
                reason: reason.into(),
                ticket_id: None,
                would_accept_rule_ids,
                target: recommended_target,
                at: now.to_owned(),
            },
        );
        dec_id
    }

    /// Attaches a queued Trigger to an existing open Ticket and subscribes the Ticket to the Trigger's Thread.
    pub fn attach_trigger_to_ticket(
        &mut self,
        trigger_id: &str,
        ticket_id: &str,
        now: &str,
    ) -> Result<(), String> {
        let trigger = self
            .state
            .triggers
            .get(trigger_id)
            .cloned()
            .ok_or_else(|| format!("Trigger {trigger_id} not found"))?;

        let input_id = self.next_id("inp");
        let dec_id = self.next_id("dec");

        let ticket = self
            .state
            .tickets
            .get_mut(ticket_id)
            .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;
        ticket.subscriptions.insert(trigger.thread_key.clone());
        if let Some(ch) = &trigger.reply_channel
            && !ticket.channels.contains(ch) {
                ticket.channels.push(ch.clone());
            }
        ticket.inputs.push(TicketInput {
            id: input_id.clone(),
            kind: TicketInputKind::TriggerFanOut {
                trigger_id: trigger.id.clone(),
                thread_key: trigger.thread_key.clone(),
                text_preview: trigger.text.chars().take(600).collect(),
            },
            from_person: !trigger.actor_is_bot,
            read_by_worker: false,
            handed_back_reason: None,
            at: now.to_owned(),
        });

        let running_assign = ticket.latest_assignment().map(|a| a.id.clone());

        if let Some(thread) = self.state.threads.get_mut(&trigger.thread_key) {
            thread.subscribed_tickets.insert(ticket_id.to_owned());
            thread.decision_ids.push(dec_id.clone());
            thread.queued_trigger_ids.retain(|id| id != trigger_id);
        }

        self.state.decisions.insert(
            dec_id.clone(),
            Decision {
                id: dec_id,
                thread_key: trigger.thread_key,
                trigger_ids: vec![trigger.id],
                outcome: DecisionOutcome::Accept,
                reason: format!("Attached to existing ticket {ticket_id}"),
                ticket_id: Some(ticket_id.to_owned()),
                would_accept_rule_ids: Vec::new(),
                target: None,
                at: now.to_owned(),
            },
        );

        if let Some(assign_id) = running_assign {
            self.enqueue_effect(
                format!("deliver:{ticket_id}"),
                EffectKind::DeliverInputs {
                    ticket_id: ticket_id.to_owned(),
                    assignment_id: assign_id,
                    input_ids: vec![input_id],
                },
                now,
            );
        }

        Ok(())
    }
}
