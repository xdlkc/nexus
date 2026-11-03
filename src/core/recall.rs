//! Context-as-Cache (`Inline & Spill`) card builders and `Chain of Recall` resource URI resolver.

use crate::core::model::{Decision, ExecutionTarget, Thread, Ticket, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const INLINE_TRIGGER_CHARS: usize = 1_200;
pub const INLINE_NEIGHBOR_CHARS: usize = 400;
pub const INLINE_REPORT_CHARS: usize = 600;
pub const DEFAULT_PAGE_CHARS: usize = 4_000;
pub const IM_MAX_WORDS: usize = 100;

/// Caller identity used to enforce permission boundaries on `Chain of Recall` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallCaller {
    MainRouter,
    Worker { ticket_id: String },
}

/// Truncated text payload with optional Spill metadata (`total_chars` and `read_uri`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpilledText {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_uri: Option<String>,
}

/// Inline & Spill helper: keeps up to `max_chars` inline and attaches `read_uri` when spilled.
pub fn inline_or_spill(text: &str, max_chars: usize, read_uri: &str) -> SpilledText {
    let total = text.chars().count();
    if total <= max_chars {
        SpilledText {
            text: text.to_owned(),
            total_chars: None,
            read_uri: None,
        }
    } else {
        let head: String = text.chars().take(max_chars).collect();
        SpilledText {
            text: format!("{head}…"),
            total_chars: Some(total),
            read_uri: Some(read_uri.to_owned()),
        }
    }
}

/// Counts words across Latin whitespace-delimited tokens and individual CJK ideographs.
pub fn count_words(text: &str) -> usize {
    let mut count = 0usize;
    let mut in_ascii_word = false;

    for ch in text.chars() {
        if is_cjk(ch) {
            if in_ascii_word {
                count += 1;
                in_ascii_word = false;
            }
            count += 1;
        } else if ch.is_whitespace() {
            if in_ascii_word {
                count += 1;
                in_ascii_word = false;
            }
        } else {
            in_ascii_word = true;
        }
    }
    if in_ascii_word {
        count += 1;
    }
    count
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x3040..=0x30FF
            | 0xAC00..=0xD7AF
    )
}

/// Validates that an IM message respects the 100-word low-noise limit.
pub fn validate_im_word_limit(text: &str) -> Result<(), String> {
    let words = count_words(text);
    if words > IM_MAX_WORDS {
        Err(format!(
            "IM message exceeds {IM_MAX_WORDS}-word limit ({words} words)"
        ))
    } else {
        Ok(())
    }
}

/// Truncates text so that `count_words(result) <= max_words` (accounting for both CJK ideographs and ASCII words).
pub fn truncate_to_im_word_limit(text: &str, max_words: usize) -> String {
    if count_words(text) <= max_words {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if count_words(&candidate) > max_words {
            break;
        }
        out.push(ch);
    }
    format!("{}…", out.trim_end())
}

/// Compact inline card for a Trigger, embedding 1-hop Chain of Recall URIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerCard {
    pub uri: String,
    pub thread_uri: String,
    pub from: String,
    pub at: String,
    pub addressed_agent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to_uri: Option<String>,
    pub content: SpilledText,
}

pub fn build_trigger_card(trigger: &Trigger, max_chars: usize) -> TriggerCard {
    let uri = format!("trigger/{}", trigger.id);
    TriggerCard {
        content: inline_or_spill(&trigger.text, max_chars, &uri),
        uri,
        thread_uri: format!("thread/{}", trigger.thread_key),
        from: trigger.actor_id.clone(),
        at: trigger.at.clone(),
        addressed_agent: trigger.addressed_agent,
        reply_to_uri: trigger
            .reply_to_trigger_id
            .as_ref()
            .map(|id| format!("trigger/{id}")),
    }
}

/// Compact inline card for a Ticket, embedding 1-hop Chain of Recall URIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketCard {
    pub uri: String,
    pub id: String,
    pub short_name: String,
    pub title: String,
    pub stage: String,
    pub origin_trigger_uri: String,
    pub subscribed_thread_uris: Vec<String>,
    pub current_target: ExecutionTarget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_report_preview: Option<SpilledText>,
}

pub fn build_ticket_card(ticket: &Ticket) -> TicketCard {
    let uri = format!("ticket/{}", ticket.id);
    let latest_report_preview = ticket.latest_assignment().and_then(|a| {
        a.latest_report.as_ref().map(|r| {
            let report_uri = format!("ticket/{}/report/{}", ticket.id, a.id);
            inline_or_spill(&r.summary, INLINE_REPORT_CHARS, &report_uri)
        })
    });

    TicketCard {
        uri,
        id: ticket.id.clone(),
        short_name: ticket.short_name.clone(),
        title: ticket.title.clone(),
        stage: serde_json::to_value(ticket.stage())
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "working".to_owned()),
        origin_trigger_uri: format!("trigger/{}", ticket.origin_trigger_id),
        subscribed_thread_uris: ticket
            .subscriptions
            .iter()
            .map(|k| format!("thread/{k}"))
            .collect(),
        current_target: ticket.current_target(),
        latest_report_preview,
    }
}

/// Structured handoff card injected when switching a Ticket across Worker Harnesses/Models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffCard {
    pub ticket_uri: String,
    pub title: String,
    pub previous_assignment_id: String,
    pub previous_target: ExecutionTarget,
    pub new_target: ExecutionTarget,
    pub previous_summary: String,
    pub git_status_diff: String,
    pub unread_input_uris: Vec<String>,
}

/// In-memory view of domain entities used to resolve `orchestrator_read` URIs.
pub struct RecallSnapshot<'a> {
    pub triggers: &'a BTreeMap<String, Trigger>,
    pub threads: &'a BTreeMap<String, Thread>,
    pub tickets: &'a BTreeMap<String, Ticket>,
    pub decisions: &'a BTreeMap<String, Decision>,
}

/// Resolves a Chain of Recall resource URI (`trigger/<id>`, `thread/<key>`, `ticket/<id>`, etc.)
/// while enforcing Worker context isolation boundaries.
pub fn read_resource(
    snapshot: &RecallSnapshot<'_>,
    caller: &RecallCaller,
    resource_uri: &str,
) -> Result<serde_json::Value, String> {
    let normalized = resource_uri.trim().trim_start_matches('/');

    if normalized == "tickets" {
        return match caller {
            RecallCaller::MainRouter => {
                let items: Vec<TicketCard> =
                    snapshot.tickets.values().map(build_ticket_card).collect();
                Ok(serde_json::json!({ "items": items }))
            }
            RecallCaller::Worker { ticket_id } => {
                let ticket = snapshot
                    .tickets
                    .get(ticket_id)
                    .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;
                Ok(serde_json::json!({ "items": [build_ticket_card(ticket)] }))
            }
        };
    }

    if let Some(trig_id) = normalized.strip_prefix("trigger/") {
        let trigger = snapshot
            .triggers
            .get(trig_id)
            .ok_or_else(|| format!("Trigger {trig_id} not found"))?;
        check_thread_permission(snapshot, caller, &trigger.thread_key)?;

        let mut neighbors = Vec::new();
        if let Some(thread) = snapshot.threads.get(&trigger.thread_key)
            && let Some(pos) = thread.trigger_ids.iter().position(|id| id == trig_id) {
                let start = pos.saturating_sub(3);
                let end = (pos + 4).min(thread.trigger_ids.len());
                for nid in &thread.trigger_ids[start..end] {
                    if nid != trig_id
                        && let Some(nt) = snapshot.triggers.get(nid) {
                            neighbors.push(build_trigger_card(nt, INLINE_NEIGHBOR_CHARS));
                        }
                }
            }

        return Ok(serde_json::json!({
            "uri": format!("trigger/{}", trigger.id),
            "thread_uri": format!("thread/{}", trigger.thread_key),
            "from": trigger.actor_id,
            "at": trigger.at,
            "text": trigger.text,
            "reply_to_uri": trigger.reply_to_trigger_id.as_ref().map(|id| format!("trigger/{id}")),
            "neighbors": neighbors,
        }));
    }

    if let Some(thread_key) = normalized.strip_prefix("thread/") {
        let thread = snapshot
            .threads
            .get(thread_key)
            .ok_or_else(|| format!("Thread {thread_key} not found"))?;
        check_thread_permission(snapshot, caller, thread_key)?;

        let triggers: Vec<TriggerCard> = thread
            .trigger_ids
            .iter()
            .rev()
            .take(20)
            .rev()
            .filter_map(|id| snapshot.triggers.get(id))
            .map(|t| build_trigger_card(t, INLINE_NEIGHBOR_CHARS))
            .collect();

        let ticket_uris: Vec<String> = match caller {
            RecallCaller::MainRouter => thread
                .subscribed_tickets
                .iter()
                .map(|id| format!("ticket/{id}"))
                .collect(),
            RecallCaller::Worker { ticket_id } => {
                if thread.subscribed_tickets.contains(ticket_id) {
                    vec![format!("ticket/{ticket_id}")]
                } else {
                    Vec::new()
                }
            }
        };

        return Ok(serde_json::json!({
            "uri": format!("thread/{}", thread.thread_key),
            "label": thread.label,
            "source_id": thread.source_id,
            "ticket_uris": ticket_uris,
            "recent_triggers": triggers,
        }));
    }

    if let Some(rest) = normalized.strip_prefix("ticket/") {
        if let Some((ticket_id, assignment_id)) = rest.split_once("/report/") {
            check_ticket_permission(caller, ticket_id)?;
            let ticket = snapshot
                .tickets
                .get(ticket_id)
                .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;
            let assignment = ticket
                .assignments
                .iter()
                .find(|a| a.id == assignment_id)
                .ok_or_else(|| format!("Assignment {assignment_id} not found"))?;
            return Ok(serde_json::json!({
                "uri": format!("ticket/{ticket_id}/report/{assignment_id}"),
                "ticket_uri": format!("ticket/{ticket_id}"),
                "target": assignment.target,
                "status": assignment.status,
                "report": assignment.latest_report,
            }));
        }

        let ticket_id = rest;
        check_ticket_permission(caller, ticket_id)?;
        let ticket = snapshot
            .tickets
            .get(ticket_id)
            .ok_or_else(|| format!("Ticket {ticket_id} not found"))?;

        return Ok(serde_json::json!({
            "card": build_ticket_card(ticket),
            "mandate": ticket.mandate,
            "context_thread_uris": ticket.context_subscriptions.iter().map(|k| format!("thread/{k}")).collect::<Vec<_>>(),
            "assignments": ticket.assignments,
            "inputs": ticket.inputs,
        }));
    }

    if let Some(decision_id) = normalized.strip_prefix("decision/") {
        if matches!(caller, RecallCaller::Worker { .. }) {
            return Err("Worker sessions are not permitted to read global routing decisions".to_owned());
        }
        let decision = snapshot
            .decisions
            .get(decision_id)
            .ok_or_else(|| format!("Decision {decision_id} not found"))?;
        return Ok(serde_json::json!({
            "uri": format!("decision/{}", decision.id),
            "thread_uri": format!("thread/{}", decision.thread_key),
            "trigger_uris": decision.trigger_ids.iter().map(|id| format!("trigger/{id}")).collect::<Vec<_>>(),
            "ticket_uri": decision.ticket_id.as_ref().map(|id| format!("ticket/{id}")),
            "outcome": decision.outcome,
            "reason": decision.reason,
            "would_accept_rule_ids": decision.would_accept_rule_ids,
            "target": decision.target,
            "at": decision.at,
        }));
    }

    Err(format!("Unsupported resource URI: {resource_uri}"))
}

fn check_ticket_permission(caller: &RecallCaller, target_ticket_id: &str) -> Result<(), String> {
    match caller {
        RecallCaller::MainRouter => Ok(()),
        RecallCaller::Worker { ticket_id } if ticket_id == target_ticket_id => Ok(()),
        RecallCaller::Worker { .. } => Err(format!(
            "Permission denied: worker cannot read ticket/{target_ticket_id}"
        )),
    }
}

fn check_thread_permission(
    snapshot: &RecallSnapshot<'_>,
    caller: &RecallCaller,
    thread_key: &str,
) -> Result<(), String> {
    match caller {
        RecallCaller::MainRouter => Ok(()),
        RecallCaller::Worker { ticket_id } => {
            let ticket = snapshot
                .tickets
                .get(ticket_id)
                .ok_or_else(|| format!("Worker ticket {ticket_id} not found"))?;
            if ticket.subscriptions.contains(thread_key)
                || ticket.context_subscriptions.contains(thread_key)
            {
                Ok(())
            } else {
                Err(format!(
                    "Permission denied: worker for {ticket_id} is not subscribed to thread/{thread_key}"
                ))
            }
        }
    }
}
