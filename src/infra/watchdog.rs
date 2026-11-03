//! Dual-layer Watchdog: monitors Worker health and uses `Jev` (`is_looping`, `is_drifting`)
//! to trigger automatic `ThinkEffort` escalation and cross-harness recovery (`switch_execution_target`).

use crate::core::engine::Engine;
use crate::core::model::ExecutionTarget;
use crate::router::jev::{
    BoolCriteria, ClassifierAnswer, ClassifierContext, ClassifierQuestion, JevClassifier,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Probability threshold above which Jev flags a Worker as looping or drifting.
pub const WATCHDOG_ANOMALY_THRESHOLD: f64 = 0.85;

/// Health board entry for a peripheral Source or Worker component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub component_id: String,
    pub healthy: bool,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// Outcome of a Watchdog semantic inspection of a running Worker's trace.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchdogVerdict {
    Healthy {
        loop_probability: f64,
        drift_probability: f64,
    },
    EscalatedRecovery {
        ticket_id: String,
        new_assignment_id: String,
        new_target: ExecutionTarget,
        reason: String,
    },
}

/// Builds the System-One `ClassifierContext` used by the Watchdog to detect Worker infinite loops or goal drift.
pub fn build_watchdog_jev_context(
    ticket_title: &str,
    recent_worker_trace: &str,
) -> ClassifierContext {
    let mut questions = BTreeMap::new();
    questions.insert(
        "is_looping".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Is the worker repeating the same failing commands or edits without making forward progress?".to_owned(),
            criteria: BoolCriteria {
                when_true: "Repeated identical errors, oscillating edits, or stuck retry loop".to_owned(),
                when_false: "Making forward diagnostic or implementation progress".to_owned(),
            },
        },
    );
    questions.insert(
        "is_drifting".to_owned(),
        ClassifierQuestion::Bool {
            instructions: "Has the worker drifted away from the ticket's stated goal into unrelated files or tasks?".to_owned(),
            criteria: BoolCriteria {
                when_true: "Modifying or investigating areas unrelated to the ticket goal".to_owned(),
                when_false: "Focused on solving the ticket's stated goal".to_owned(),
            },
        },
    );

    ClassifierContext {
        state: serde_json::json!({
            "ticket_title": ticket_title,
            "recent_worker_trace": recent_worker_trace,
        }),
        questions,
    }
}

/// Evaluates a running Worker's trace with `Jev`; if looping or drifting is detected (`p >= 0.85`),
/// automatically escalates `ThinkEffort` by one tier and triggers `Engine::switch_execution_target`.
pub async fn inspect_and_recover_worker<J: JevClassifier>(
    engine: &mut Engine,
    jev: &J,
    ticket_id: &str,
    recent_worker_trace: &str,
    git_status_diff: &str,
    now: &str,
) -> anyhow::Result<WatchdogVerdict> {
    let ticket = engine
        .state
        .tickets
        .get(ticket_id)
        .ok_or_else(|| anyhow::anyhow!("Ticket {ticket_id} not found"))?;

    let title = ticket.title.clone();
    let current_target = ticket.current_target();

    let ctx = build_watchdog_jev_context(&title, recent_worker_trace);
    let res = jev.classify(&ctx).await?;

    let loop_p = match res.answers.get("is_looping") {
        Some(ClassifierAnswer::Bool { probability }) => *probability,
        _ => 0.0,
    };
    let drift_p = match res.answers.get("is_drifting") {
        Some(ClassifierAnswer::Bool { probability }) => *probability,
        _ => 0.0,
    };

    if loop_p >= WATCHDOG_ANOMALY_THRESHOLD || drift_p >= WATCHDOG_ANOMALY_THRESHOLD {
        let new_target = ExecutionTarget {
            harness: current_target.harness,
            provider: current_target.provider.clone(),
            model: current_target.model.clone(),
            think_effort: current_target.think_effort.escalate(),
        };
        let reason = if loop_p >= WATCHDOG_ANOMALY_THRESHOLD {
            format!(
                "Watchdog detected worker loop (p={loop_p:.2}); escalating ThinkEffort {} -> {}",
                current_target.think_effort.as_str(),
                new_target.think_effort.as_str()
            )
        } else {
            format!(
                "Watchdog detected goal drift (p={drift_p:.2}); escalating ThinkEffort {} -> {}",
                current_target.think_effort.as_str(),
                new_target.think_effort.as_str()
            )
        };

        let new_assignment_id = engine
            .switch_execution_target(
                ticket_id,
                new_target.clone(),
                &reason,
                git_status_diff,
                now,
            )
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(WatchdogVerdict::EscalatedRecovery {
            ticket_id: ticket_id.to_owned(),
            new_assignment_id,
            new_target,
            reason,
        })
    } else {
        Ok(WatchdogVerdict::Healthy {
            loop_probability: loop_p,
            drift_probability: drift_p,
        })
    }
}
