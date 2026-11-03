//! Concrete HTTP and fallback implementations of the System-1 `JevClassifier` trait.
//!
//! Supports:
//! - TypeSafe System-One (`POST /v1/systemone`)
//! - OpenRouter System-One (`POST /api/v1/systemone`)
//! - OpenAI Decisions (`POST /v1/decisions`)
//! - Deterministic local heuristic fallback when no remote System-1 API key is configured.

use crate::router::jev::{
    ClassifierAnswer, ClassifierContext, ClassifierQuestion, ClassifierResult, JevClassifier,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Supported System-1 wire protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JevWireProtocol {
    TypesafeSystemOne,
    OpenrouterSystemOne,
    OpenaiDecisions,
}

/// Configuration for the HTTP System-1 Jev client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpJevConfig {
    pub protocol: JevWireProtocol,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

/// HTTP client implementing `JevClassifier` across System-One and OpenAI Decisions endpoints.
#[derive(Clone)]
pub struct HttpJevClient {
    config: HttpJevConfig,
    http: reqwest::Client,
}

impl HttpJevClient {
    pub fn new(config: HttpJevConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }

    pub fn build_endpoint_url(&self) -> String {
        let trimmed = self.config.base_url.trim_end_matches('/');
        match self.config.protocol {
            JevWireProtocol::TypesafeSystemOne | JevWireProtocol::OpenrouterSystemOne => {
                if trimmed.ends_with("/systemone") {
                    trimmed.to_owned()
                } else {
                    format!("{trimmed}/systemone")
                }
            }
            JevWireProtocol::OpenaiDecisions => {
                if trimmed.ends_with("/decisions") {
                    trimmed.to_owned()
                } else {
                    format!("{trimmed}/decisions")
                }
            }
        }
    }

    pub fn build_request_payload(&self, context: &ClassifierContext) -> serde_json::Value {
        match self.config.protocol {
            JevWireProtocol::TypesafeSystemOne | JevWireProtocol::OpenrouterSystemOne => {
                let mut wire_questions = serde_json::Map::new();
                for (name, q) in &context.questions {
                    match q {
                        ClassifierQuestion::Bool {
                            instructions,
                            criteria,
                        } => {
                            wire_questions.insert(
                                name.clone(),
                                serde_json::json!({
                                    "type": "noul",
                                    "instructions": instructions,
                                    "criteria": {
                                        "true": criteria.when_true,
                                        "false": criteria.when_false,
                                    }
                                }),
                            );
                        }
                        ClassifierQuestion::Choice {
                            instructions,
                            criteria,
                        } => {
                            wire_questions.insert(
                                name.clone(),
                                serde_json::json!({
                                    "type": "choice",
                                    "instructions": instructions,
                                    "criteria": criteria,
                                }),
                            );
                        }
                        ClassifierQuestion::Score {
                            instructions,
                            criteria,
                        } => {
                            wire_questions.insert(
                                name.clone(),
                                serde_json::json!({
                                    "type": "score",
                                    "instructions": instructions,
                                    "criteria": criteria,
                                }),
                            );
                        }
                    }
                }

                serde_json::json!({
                    "model": self.config.model,
                    "state": context.state,
                    "questions": wire_questions,
                })
            }
            JevWireProtocol::OpenaiDecisions => {
                serde_json::json!({
                    "model": self.config.model,
                    "state": context.state,
                    "questions": context.questions,
                })
            }
        }
    }

    pub fn parse_response_payload(
        &self,
        raw: serde_json::Value,
    ) -> anyhow::Result<ClassifierResult> {
        let model = raw
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.config.model)
            .to_owned();
        let answers_obj = raw
            .get("answers")
            .and_then(|v| v.as_object())
            .ok_or_else(|| anyhow::anyhow!("Missing `answers` object in Jev response"))?;

        let mut answers = BTreeMap::new();
        for (name, val) in answers_obj {
            let kind = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "noul" | "bool" => {
                    let prob = val
                        .get("noul")
                        .or_else(|| val.get("probability"))
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    answers.insert(name.clone(), ClassifierAnswer::Bool { probability: prob });
                }
                "choice" => {
                    let choice = val
                        .get("choice")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let confidence = val
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let mut probabilities = BTreeMap::new();
                    if let Some(probs) = val.get("probabilities").and_then(|v| v.as_object()) {
                        for (k, p) in probs {
                            if let Some(pf) = p.as_f64() {
                                probabilities.insert(k.clone(), pf);
                            }
                        }
                    }
                    answers.insert(
                        name.clone(),
                        ClassifierAnswer::Choice {
                            choice,
                            probabilities,
                            confidence,
                        },
                    );
                }
                "score" => {
                    let score = val.get("score").and_then(|v| v.as_f64()).unwrap_or(2.0);
                    let confidence = val
                        .get("confidence")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    answers.insert(
                        name.clone(),
                        ClassifierAnswer::Score { score, confidence },
                    );
                }
                other => {
                    anyhow::bail!("Unsupported Jev answer type `{other}` for question `{name}`");
                }
            }
        }

        Ok(ClassifierResult { model, answers })
    }
}

impl JevClassifier for HttpJevClient {
    async fn classify(&self, context: &ClassifierContext) -> anyhow::Result<ClassifierResult> {
        let url = self.build_endpoint_url();
        let payload = self.build_request_payload(context);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jev HTTP error ({status}): {body}");
        }

        let raw: serde_json::Value = resp.json().await?;
        self.parse_response_payload(raw)
    }
}

/// Fast local heuristic classifier used when no external System-1 API key is configured
/// or as a zero-network fallback during offline development.
#[derive(Debug, Clone, Default)]
pub struct HeuristicJevClassifier;

impl JevClassifier for HeuristicJevClassifier {
    async fn classify(&self, context: &ClassifierContext) -> anyhow::Result<ClassifierResult> {
        let state_text = serde_json::to_string(&context.state)
            .unwrap_or_default()
            .to_lowercase();
        let addressed = context
            .state
            .get("triggers")
            .or_else(|| context.state.get("candidate_triggers"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|t| {
                    t.get("addressed")
                        .or_else(|| t.get("addressed_agent"))
                        .and_then(|b| b.as_bool())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let is_injection = state_text.contains("ignore previous instructions")
            || state_text.contains("exfiltrate")
            || state_text.contains("system prompt");

        let is_stop = state_text.contains("stop working")
            || state_text.contains("cancel ticket")
            || state_text.contains("abort task");

        let is_hard_debug = state_text.contains("deadlock")
            || state_text.contains("race condition")
            || state_text.contains("memory leak")
            || state_text.contains("flaky");

        let is_refactor = state_text.contains("refactor")
            || state_text.contains("architecture")
            || state_text.contains("redesign");

        let is_gws = state_text.contains("calendar")
            || state_text.contains("gmail")
            || state_text.contains("google doc")
            || state_text.contains("spreadsheet");

        let mut answers = BTreeMap::new();
        for (name, q) in &context.questions {
            match q {
                ClassifierQuestion::Bool { .. } => {
                    let prob = if name == "injection_risk" {
                        if is_injection { 0.98 } else { 0.01 }
                    } else if name == "is_looping" || name == "is_drifting" {
                        0.05
                    } else if addressed {
                        // Proactive rule match (`rule_*`) when addressed or woken by keyword
                        0.88
                    } else {
                        0.05
                    };
                    answers.insert(name.clone(), ClassifierAnswer::Bool { probability: prob });
                }
                ClassifierQuestion::Choice { .. } => {
                    if name == "route_action" {
                        let choice = if !addressed {
                            "ignore"
                        } else if is_stop {
                            "stop_ticket"
                        } else if !state_text.is_empty() {
                            "new_ticket"
                        } else {
                            "ignore"
                        };
                        answers.insert(
                            name.clone(),
                            ClassifierAnswer::Choice {
                                choice: choice.to_owned(),
                                probabilities: BTreeMap::from([(choice.to_owned(), 0.90)]),
                                confidence: 0.90,
                            },
                        );
                    } else if name == "task_kind" {
                        let choice = if is_hard_debug {
                            "hard_debug"
                        } else if is_refactor {
                            "deep_refactor"
                        } else if is_gws {
                            "gws_workspace"
                        } else {
                            "quick_code"
                        };
                        answers.insert(
                            name.clone(),
                            ClassifierAnswer::Choice {
                                choice: choice.to_owned(),
                                probabilities: BTreeMap::from([(choice.to_owned(), 0.88)]),
                                confidence: 0.88,
                            },
                        );
                    }
                }
                ClassifierQuestion::Score { .. } => {
                    let score = if is_hard_debug {
                        4.0
                    } else if is_refactor {
                        3.0
                    } else if is_gws {
                        1.0
                    } else {
                        2.0
                    };
                    answers.insert(
                        name.clone(),
                        ClassifierAnswer::Score {
                            score,
                            confidence: 0.85,
                        },
                    );
                }
            }
        }

        Ok(ClassifierResult {
            model: "heuristic-jev-v1".to_owned(),
            answers,
        })
    }
}
