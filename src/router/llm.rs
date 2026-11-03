//! Multi-provider LLM client (`LlmClient`) supporting OpenAI-compatible (OpenAI, DeepSeek,
//! OpenRouter) and Anthropic Messages APIs with `ThinkEffort` reasoning budget mapping and tool calling.

use crate::core::model::ThinkEffort;
use serde::{Deserialize, Serialize};

/// Role of a conversation message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Tool call emitted by an assistant turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Single conversation message passed to an LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<LlmToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::System,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>, tool_calls: Vec<LlmToolCall>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// JSON Schema definition of a tool exposed to the LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Request parameters for a single LLM completion turn.
#[derive(Debug, Clone)]
pub struct LlmCompletionRequest {
    pub provider: String,
    pub model: String,
    pub think_effort: ThinkEffort,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolSpec>,
    pub max_tokens: u32,
}

/// Response from a single LLM completion turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmCompletionResponse {
    pub content: String,
    pub tool_calls: Vec<LlmToolCall>,
}

/// Trait abstracting an LLM completion backend (used by Tier-2 Router Orchestrator and `pi-native` Worker).
pub trait LlmClient: Send + Sync {
    fn complete(
        &self,
        request: &LlmCompletionRequest,
    ) -> impl std::future::Future<Output = anyhow::Result<LlmCompletionResponse>> + Send;
}

/// Maps `ThinkEffort` to an Anthropic extended-thinking token budget.
pub fn anthropic_thinking_budget(effort: ThinkEffort) -> Option<u32> {
    match effort {
        ThinkEffort::Off => None,
        ThinkEffort::Minimal => Some(1_024),
        ThinkEffort::Low => Some(2_048),
        ThinkEffort::Medium => Some(4_096),
        ThinkEffort::High => Some(10_240),
        ThinkEffort::Xhigh => Some(24_576),
    }
}

/// Maps `ThinkEffort` to an OpenAI `reasoning_effort` string.
pub fn openai_reasoning_effort(effort: ThinkEffort) -> Option<&'static str> {
    match effort {
        ThinkEffort::Off => None,
        ThinkEffort::Minimal | ThinkEffort::Low => Some("low"),
        ThinkEffort::Medium => Some("medium"),
        ThinkEffort::High | ThinkEffort::Xhigh => Some("high"),
    }
}

/// Concrete HTTP LLM client supporting Anthropic Messages API and OpenAI-compatible Chat Completions.
#[derive(Clone)]
pub struct HttpLlmClient {
    http: reqwest::Client,
    anthropic_base_url: String,
    anthropic_api_key: Option<String>,
    openai_base_url: String,
    openai_api_key: Option<String>,
}

impl HttpLlmClient {
    pub fn from_env() -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            anthropic_base_url: std::env::var("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_owned()),
            anthropic_api_key: std::env::var("ANTHROPIC_API_KEY").ok(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned()),
            openai_api_key: std::env::var("OPENAI_API_KEY")
                .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
                .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
                .ok(),
        }
    }

    async fn complete_anthropic(
        &self,
        req: &LlmCompletionRequest,
    ) -> anyhow::Result<LlmCompletionResponse> {
        let api_key = self
            .anthropic_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("ANTHROPIC_API_KEY is not configured"))?;

        let mut system_prompts = Vec::new();
        let mut messages = Vec::new();

        for msg in &req.messages {
            match msg.role {
                LlmRole::System => system_prompts.push(msg.content.clone()),
                LlmRole::User => messages.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                })),
                LlmRole::Assistant => {
                    let mut blocks = Vec::new();
                    if !msg.content.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": msg.content,
                        }));
                    }
                    for tc in &msg.tool_calls {
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": tc.arguments,
                        }));
                    }
                    messages.push(serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    }));
                }
                LlmRole::Tool => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": msg.tool_call_id.clone().unwrap_or_default(),
                            "content": msg.content,
                        }],
                    }));
                }
            }
        }

        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "messages": messages,
        });
        if !system_prompts.is_empty() {
            body["system"] = serde_json::Value::String(system_prompts.join("\n\n"));
        }
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }
        if let Some(budget) = anthropic_thinking_budget(req.think_effort)
            && req.max_tokens > budget {
                body["thinking"] = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": budget,
                });
            }

        let trimmed_base = self.anthropic_base_url.trim_end_matches('/');
        let url = if trimmed_base.ends_with("/v1") {
            format!("{trimmed_base}/messages")
        } else {
            format!("{trimmed_base}/v1/messages")
        };
        let resp = self
            .http
            .post(&url)
            .header("x-api-key", api_key)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error ({status}): {err_text}");
        }

        let raw: serde_json::Value = resp.json().await?;
        let mut content = String::new();
        let mut tool_calls = Vec::new();

        if let Some(blocks) = raw.get("content").and_then(|v| v.as_array()) {
            for block in blocks {
                match block.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                            if !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(t);
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("call-1")
                            .to_owned();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned();
                        let arguments = block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        tool_calls.push(LlmToolCall {
                            id,
                            name,
                            arguments,
                        });
                    }
                    _ => {}
                }
            }
        }

        Ok(LlmCompletionResponse {
            content,
            tool_calls,
        })
    }

    async fn complete_openai(
        &self,
        req: &LlmCompletionRequest,
    ) -> anyhow::Result<LlmCompletionResponse> {
        let api_key = self
            .openai_api_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY is not configured"))?;

        let mut messages = Vec::new();
        for msg in &req.messages {
            match msg.role {
                LlmRole::System => messages.push(serde_json::json!({
                    "role": "system",
                    "content": msg.content,
                })),
                LlmRole::User => messages.push(serde_json::json!({
                    "role": "user",
                    "content": msg.content,
                })),
                LlmRole::Assistant => {
                    let mut obj = serde_json::json!({
                        "role": "assistant",
                        "content": msg.content,
                    });
                    if !msg.tool_calls.is_empty() {
                        let tcs: Vec<serde_json::Value> = msg
                            .tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments.to_string(),
                                    }
                                })
                            })
                            .collect();
                        obj["tool_calls"] = serde_json::Value::Array(tcs);
                    }
                    messages.push(obj);
                }
                LlmRole::Tool => messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": msg.tool_call_id.clone().unwrap_or_default(),
                    "content": msg.content,
                })),
            }
        }

        let tools: Vec<serde_json::Value> = req
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools);
        }
        if let Some(effort) = openai_reasoning_effort(req.think_effort) {
            body["reasoning_effort"] = serde_json::Value::String(effort.to_owned());
        }

        let url = format!(
            "{}/chat/completions",
            self.openai_base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error ({status}): {err_text}");
        }

        let raw: serde_json::Value = resp.json().await?;
        let msg = raw
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .ok_or_else(|| anyhow::anyhow!("Missing choices[0].message in OpenAI response"))?;

        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        let mut tool_calls = Vec::new();
        if let Some(tcs) = msg.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tcs {
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("call-1")
                    .to_owned();
                if let Some(func) = tc.get("function") {
                    let name = func
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned();
                    let args_raw = func
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("{}");
                    let arguments = serde_json::from_str(args_raw)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    tool_calls.push(LlmToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }

        Ok(LlmCompletionResponse {
            content,
            tool_calls,
        })
    }
}

impl LlmClient for HttpLlmClient {
    async fn complete(
        &self,
        request: &LlmCompletionRequest,
    ) -> anyhow::Result<LlmCompletionResponse> {
        if request.provider.contains("anthropic") || request.model.starts_with("claude") {
            self.complete_anthropic(request).await
        } else {
            self.complete_openai(request).await
        }
    }
}
