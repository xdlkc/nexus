//! Tier-2 System-2 Resident `RouterOrchestrator` and full Dual-Engine evaluation pipeline.
//!
//! Combines Tier-1 (`Jev`) fast path with Tier-2 (`Nexus-Router` LLM) escalation,
//! supporting Auto-Batching, Context-as-Cache (`Inline & Spill`), and `Chain of Recall` (`orchestrator_read`).

use crate::core::engine::Engine;
use crate::core::model::{
    AcceptMode, AdmissionStrength, ExecutionTarget, HarnessKind, OutboxPurpose, ProactiveRule,
    RouteAction, ThinkEffort, TicketSummary, Trigger,
};
use crate::core::recall::{
     INLINE_TRIGGER_CHARS, RecallCaller, build_ticket_card, build_trigger_card,
};
use crate::router::jev::{
    JevClassifier, JevTriageInput, JevTriageOutcome, ThreadMessageStub, run_jev_triage,
};
use crate::router::llm::{
    LlmClient, LlmCompletionRequest, LlmMessage, LlmToolSpec,
};
use serde::{Deserialize, Serialize};

/// Configuration for the Tier-2 System-2 LLM Router Orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterLlmConfig {
    pub provider: String,
    pub model: String,
    pub think_effort: ThinkEffort,
    pub max_turns: usize,
}

impl Default for RouterLlmConfig {
    fn default() -> Self {
        Self {
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            think_effort: ThinkEffort::Medium,
            max_turns: 4,
        }
    }
}

/// Summary of how a candidate Thread batch was handled by the Dual-Engine Router.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalSummary {
    DroppedInjection {
        decision_id: String,
        probability: f64,
    },
    IgnoredByJev {
        decision_id: String,
        would_accept_rule_ids: Vec<String>,
    },
    FastDispatchedByJev {
        action: RouteAction,
        ticket_id: Option<String>,
    },
    HandledByTier2Llm {
        turns: usize,
        routed_tickets: Vec<String>,
    },
}

/// Builds the JSON Schema tool specifications exposed to the Tier-2 Router LLM.
pub fn router_tool_specs() -> Vec<LlmToolSpec> {
    vec![
        LlmToolSpec {
            name: "orchestrator_read".to_owned(),
            description: "Read a Chain of Recall resource URI (`trigger/<id>`, `thread/<key>`, `ticket/<id>`, `decision/<id>`, or `tickets`) to inspect spilled text or historical context.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "uri": { "type": "string", "description": "Resource URI to read" }
                },
                "required": ["uri"]
            }),
        },
        LlmToolSpec {
            name: "orchestrator_route".to_owned(),
            description: "Commit a routing decision for a candidate trigger (`accept`, `attach`, `stop`, or `ignore`).".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["accept", "attach", "stop", "ignore"]
                    },
                    "trigger_id": { "type": "string" },
                    "title": { "type": "string" },
                    "repo": { "type": "string" },
                    "ticket_id": { "type": "string" },
                    "harness": {
                        "type": "string",
                        "enum": ["pi-native", "pi-rpc", "claude-code", "codex", "opencode"]
                    },
                    "provider": { "type": "string" },
                    "model": { "type": "string" },
                    "think_effort": {
                        "type": "string",
                        "enum": ["off", "minimal", "low", "medium", "high", "xhigh"]
                    },
                    "acknowledgement": {
                        "type": "string",
                        "description": "Optional short acknowledgement reply (<= 100 words on IM)"
                    },
                    "reason": { "type": "string" }
                },
                "required": ["action", "trigger_id"]
            }),
        },
        LlmToolSpec {
            name: "orchestrator_switch_target".to_owned(),
            description: "Switch an open Ticket's Worker execution target (`harness`, `provider`, `model`, `think_effort`) mid-flight.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "ticket_id": { "type": "string" },
                    "harness": {
                        "type": "string",
                        "enum": ["pi-native", "pi-rpc", "claude-code", "codex", "opencode"]
                    },
                    "provider": { "type": "string" },
                    "model": { "type": "string" },
                    "think_effort": {
                        "type": "string",
                        "enum": ["off", "minimal", "low", "medium", "high", "xhigh"]
                    },
                    "reason": { "type": "string" }
                },
                "required": ["ticket_id", "harness", "provider", "model", "think_effort", "reason"]
            }),
        },
        LlmToolSpec {
            name: "orchestrator_reply".to_owned(),
            description: "Send a short direct reply (<= 100 words on IM) to the trigger's channel without opening a Ticket.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "trigger_id": { "type": "string" },
                    "body": { "type": "string" }
                },
                "required": ["trigger_id", "body"]
            }),
        },
    ]
}

/// Evaluates a candidate Thread batch through the Dual-Engine Router (Tier-1 `Jev` -> Tier-2 `LLM` with Auto-Batching).
pub async fn evaluate_thread_batch<J, L>(
    engine: &mut Engine,
    jev: &J,
    llm: &L,
    llm_config: &RouterLlmConfig,
    thread_key: &str,
    rules: &[ProactiveRule],
    now: &str,
) -> anyhow::Result<EvalSummary>
where
    J: JevClassifier,
    L: LlmClient,
{
    // Auto-batching: snapshot all currently queued triggers on this Thread.
    let queued_ids: Vec<String> = engine
        .state
        .threads
        .get(thread_key)
        .map(|t| t.queued_trigger_ids.clone())
        .unwrap_or_default();

    let triggers: Vec<Trigger> = queued_ids
        .iter()
        .filter_map(|id| engine.state.triggers.get(id).cloned())
        .collect();

    if triggers.is_empty() {
        let dec_id = engine.ignore_thread_triggers(thread_key, "Empty eval batch", vec![], None, now);
        return Ok(EvalSummary::IgnoredByJev {
            decision_id: dec_id,
            would_accept_rule_ids: vec![],
        });
    }

    let strength = if triggers.iter().any(|t| t.addressed_agent) {
        AdmissionStrength::Explicit
    } else {
        AdmissionStrength::Inferred
    };
    let accept_mode = engine.state.accept_mode.unwrap_or(AcceptMode::Observe);

    let thread_messages: Vec<ThreadMessageStub> = engine
        .state
        .threads
        .get(thread_key)
        .map(|th| {
            th.trigger_ids
                .iter()
                .rev()
                .take(8)
                .rev()
                .filter_map(|tid| engine.state.triggers.get(tid))
                .map(|tr| ThreadMessageStub {
                    id: tr.id.clone(),
                    actor_id: tr.actor_id.clone(),
                    at: tr.at.clone(),
                    text: tr.text.chars().take(400).collect(),
                })
                .collect()
        })
        .unwrap_or_default();

    let open_tickets: Vec<TicketSummary> = engine
        .state
        .tickets
        .values()
        .filter(|t| t.is_open())
        .map(|t| TicketSummary {
            id: t.id.clone(),
            title: t.title.clone(),
            thread_key: t.origin_thread_key.clone(),
            stage: t.stage(),
            current_target: t.current_target(),
        })
        .collect();

    let triage_outcome = run_jev_triage(
        jev,
        &JevTriageInput {
            triggers: &triggers,
            strength,
            accept_mode,
            thread_messages: &thread_messages,
            open_tickets: &open_tickets,
            rules,
        },
    )
    .await;

    let last_trigger = triggers.last().unwrap();

    match triage_outcome {
        JevTriageOutcome::DropInjection {
            injection_probability,
        } => {
            let dec_id = engine.ignore_thread_triggers(
                thread_key,
                format!("Blocked by Jev prompt-injection air-gap (p={injection_probability:.2})"),
                vec![],
                None,
                now,
            );
            Ok(EvalSummary::DroppedInjection {
                decision_id: dec_id,
                probability: injection_probability,
            })
        }
        JevTriageOutcome::Ignore {
            would_accept_rule_ids,
            recommended_target,
            ..
        } => {
            let dec_id = engine.ignore_thread_triggers(
                thread_key,
                "Ignored by Tier-1 Jev classifier",
                would_accept_rule_ids.clone(),
                Some(recommended_target),
                now,
            );
            Ok(EvalSummary::IgnoredByJev {
                decision_id: dec_id,
                would_accept_rule_ids,
            })
        }
        JevTriageOutcome::FastDispatch {
            action,
            target_ticket_id,
            matched_rule_ids,
            execution_target,
            ..
        } => match action {
            RouteAction::NewTicket => {
                let title: String = last_trigger
                    .text
                    .lines()
                    .next()
                    .unwrap_or("Work item")
                    .chars()
                    .take(72)
                    .collect();
                let ticket_id = engine
                    .accept_ticket(
                        &last_trigger.id,
                        title,
                        None,
                        strength,
                        matched_rule_ids,
                        execution_target,
                        None,
                        now,
                    )
                    .map_err(|e| anyhow::anyhow!(e))?;
                Ok(EvalSummary::FastDispatchedByJev {
                    action: RouteAction::NewTicket,
                    ticket_id: Some(ticket_id),
                })
            }
            RouteAction::AttachTicket => {
                let tid = target_ticket_id
                    .or_else(|| open_tickets.first().map(|t| t.id.clone()))
                    .ok_or_else(|| anyhow::anyhow!("No open ticket available for AttachTicket"))?;
                engine
                    .attach_trigger_to_ticket(&last_trigger.id, &tid, now)
                    .map_err(|e| anyhow::anyhow!(e))?;
                Ok(EvalSummary::FastDispatchedByJev {
                    action: RouteAction::AttachTicket,
                    ticket_id: Some(tid),
                })
            }
            RouteAction::StopTicket => {
                let tid = target_ticket_id
                    .or_else(|| open_tickets.first().map(|t| t.id.clone()))
                    .ok_or_else(|| anyhow::anyhow!("No open ticket available for StopTicket"))?;
                engine
                    .stop_ticket(&tid, "Stopped via Jev fast-dispatch", now)
                    .map_err(|e| anyhow::anyhow!(e))?;
                let _ = engine.ignore_thread_triggers(
                    thread_key,
                    format!("Stopped ticket {tid}"),
                    vec![],
                    None,
                    now,
                );
                Ok(EvalSummary::FastDispatchedByJev {
                    action: RouteAction::StopTicket,
                    ticket_id: Some(tid),
                })
            }
            _ => {
                run_tier2_orchestrator(
                    engine,
                    llm,
                    llm_config,
                    thread_key,
                    &triggers,
                    strength,
                    &matched_rule_ids,
                    &execution_target,
                    now,
                )
                .await
            }
        },
        JevTriageOutcome::EscalateToLlmRouter {
            matched_rule_ids,
            recommended_target,
            ..
        } => {
            run_tier2_orchestrator(
                engine,
                llm,
                llm_config,
                thread_key,
                &triggers,
                strength,
                &matched_rule_ids,
                &recommended_target,
                now,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_tier2_orchestrator<L: LlmClient>(
    engine: &mut Engine,
    llm: &L,
    llm_config: &RouterLlmConfig,
    thread_key: &str,
    triggers: &[Trigger],
    strength: AdmissionStrength,
    matched_rule_ids: &[String],
    recommended_target: &ExecutionTarget,
    now: &str,
) -> anyhow::Result<EvalSummary> {
    let trigger_cards: Vec<_> = triggers
        .iter()
        .map(|t| build_trigger_card(t, INLINE_TRIGGER_CHARS))
        .collect();
    let ticket_cards: Vec<_> = engine
        .state
        .tickets
        .values()
        .filter(|t| t.is_open())
        .map(build_ticket_card)
        .collect();

    let system_prompt = "\
You are the Nexus Tier-2 Resident Router Orchestrator.\n\
Your responsibility is to route candidate Trigger events without executing code yourself.\n\
- Use `orchestrator_read` to inspect any spilled URI (`trigger/<id>`, `thread/<key>`, `ticket/<id>`) when needed.\n\
- Use `orchestrator_route` to accept a new ticket, attach to an existing ticket, stop an open ticket, or ignore.\n\
- Keep any IM acknowledgement or reply strictly <= 100 words.";

    let user_payload = serde_json::json!({
        "thread_key": thread_key,
        "admission_strength": strength,
        "matched_rule_ids": matched_rule_ids,
        "jev_recommended_target": recommended_target,
        "candidate_triggers": trigger_cards,
        "open_tickets": ticket_cards,
    });

    let mut messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(serde_json::to_string_pretty(&user_payload)?),
    ];
    let tools = router_tool_specs();
    let mut routed_tickets = Vec::new();
    let mut decided = false;
    let mut turns = 0;

    for _ in 0..llm_config.max_turns.max(1) {
        turns += 1;
        let resp = llm
            .complete(&LlmCompletionRequest {
                provider: llm_config.provider.clone(),
                model: llm_config.model.clone(),
                think_effort: llm_config.think_effort,
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: 4_096,
            })
            .await?;

        if resp.tool_calls.is_empty() {
            break;
        }

        messages.push(LlmMessage::assistant(
            resp.content.clone(),
            resp.tool_calls.clone(),
        ));

        for tc in resp.tool_calls {
            let tool_output = match tc.name.as_str() {
                "orchestrator_read" => {
                    let uri = tc
                        .arguments
                        .get("uri")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match engine.read_recall_resource(&RecallCaller::MainRouter, uri) {
                        Ok(val) => serde_json::to_string(&val).unwrap_or_default(),
                        Err(err) => serde_json::json!({ "error": err }).to_string(),
                    }
                }
                "orchestrator_route" => {
                    let action = tc
                        .arguments
                        .get("action")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ignore");
                    let trig_id = tc
                        .arguments
                        .get("trigger_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| &triggers.last().unwrap().id);
                    match action {
                        "accept" => {
                            let title = tc
                                .arguments
                                .get("title")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Accepted task");
                            let repo = tc
                                .arguments
                                .get("repo")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned);
                            let ack = tc.arguments.get("acknowledgement").and_then(|v| v.as_str());
                            let target = ExecutionTarget {
                                harness: tc
                                    .arguments
                                    .get("harness")
                                    .and_then(|v| v.as_str())
                                    .and_then(HarnessKind::parse)
                                    .unwrap_or(recommended_target.harness),
                                provider: tc
                                    .arguments
                                    .get("provider")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| recommended_target.provider.clone()),
                                model: tc
                                    .arguments
                                    .get("model")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| recommended_target.model.clone()),
                                think_effort: tc
                                    .arguments
                                    .get("think_effort")
                                    .and_then(|v| v.as_str())
                                    .and_then(ThinkEffort::parse)
                                    .unwrap_or(recommended_target.think_effort),
                            };
                            match engine.accept_ticket(
                                trig_id,
                                title,
                                repo,
                                strength,
                                matched_rule_ids.to_vec(),
                                target,
                                ack,
                                now,
                            ) {
                                Ok(tid) => {
                                    decided = true;
                                    routed_tickets.push(tid.clone());
                                    serde_json::json!({ "ok": true, "ticket_id": tid }).to_string()
                                }
                                Err(e) => serde_json::json!({ "error": e }).to_string(),
                            }
                        }
                        "attach" => {
                            let tid = tc
                                .arguments
                                .get("ticket_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            match engine.attach_trigger_to_ticket(trig_id, tid, now) {
                                Ok(()) => {
                                    decided = true;
                                    routed_tickets.push(tid.to_owned());
                                    serde_json::json!({ "ok": true, "attached_to": tid })
                                        .to_string()
                                }
                                Err(e) => serde_json::json!({ "error": e }).to_string(),
                            }
                        }
                        "stop" => {
                            let tid = tc
                                .arguments
                                .get("ticket_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let reason = tc
                                .arguments
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Stopped by Router");
                            match engine.stop_ticket(tid, reason, now) {
                                Ok(()) => {
                                    decided = true;
                                    let _ = engine.ignore_thread_triggers(
                                        thread_key,
                                        reason,
                                        vec![],
                                        None,
                                        now,
                                    );
                                    serde_json::json!({ "ok": true, "stopped": tid }).to_string()
                                }
                                Err(e) => serde_json::json!({ "error": e }).to_string(),
                            }
                        }
                        _ => {
                            let reason = tc
                                .arguments
                                .get("reason")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Ignored by Tier-2 Router");
                            let dec_id = engine.ignore_thread_triggers(
                                thread_key,
                                reason,
                                matched_rule_ids.to_vec(),
                                Some(recommended_target.clone()),
                                now,
                            );
                            decided = true;
                            serde_json::json!({ "ok": true, "decision_id": dec_id }).to_string()
                        }
                    }
                }
                "orchestrator_switch_target" => {
                    let tid = tc
                        .arguments
                        .get("ticket_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let reason = tc
                        .arguments
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Router switched target");
                    let target = ExecutionTarget {
                        harness: tc
                            .arguments
                            .get("harness")
                            .and_then(|v| v.as_str())
                            .and_then(HarnessKind::parse)
                            .unwrap_or(recommended_target.harness),
                        provider: tc
                            .arguments
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&recommended_target.provider)
                            .to_owned(),
                        model: tc
                            .arguments
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&recommended_target.model)
                            .to_owned(),
                        think_effort: tc
                            .arguments
                            .get("think_effort")
                            .and_then(|v| v.as_str())
                            .and_then(ThinkEffort::parse)
                            .unwrap_or(recommended_target.think_effort),
                    };
                    match engine.switch_execution_target(tid, target, reason, "", now) {
                        Ok(asg_id) => {
                            serde_json::json!({ "ok": true, "assignment_id": asg_id }).to_string()
                        }
                        Err(e) => serde_json::json!({ "error": e }).to_string(),
                    }
                }
                "orchestrator_reply" => {
                    let trig_id = tc
                        .arguments
                        .get("trigger_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| &triggers.last().unwrap().id);
                    let body = tc
                        .arguments
                        .get("body")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if let Some(ch) = engine
                        .state
                        .triggers
                        .get(trig_id)
                        .and_then(|t| t.reply_channel.clone())
                    {
                        match engine.queue_outbox_message(
                            None,
                            ch,
                            OutboxPurpose::Answer,
                            body,
                            now,
                        ) {
                            Ok(out_id) => {
                                serde_json::json!({ "ok": true, "outbox_id": out_id }).to_string()
                            }
                            Err(e) => serde_json::json!({ "error": e }).to_string(),
                        }
                    } else {
                        serde_json::json!({ "error": "Trigger has no reply_channel" }).to_string()
                    }
                }
                other => serde_json::json!({ "error": format!("Unknown tool {other}") }).to_string(),
            };

            messages.push(LlmMessage::tool_result(tc.id, tool_output));
        }

        if decided {
            break;
        }
    }

    if !decided {
        let _ = engine.ignore_thread_triggers(
            thread_key,
            "Tier-2 Router completed without explicit accept",
            matched_rule_ids.to_vec(),
            Some(recommended_target.clone()),
            now,
        );
    }

    Ok(EvalSummary::HandledByTier2Llm {
        turns,
        routed_tickets,
    })
}
