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

/// Concrete HTTP LLM client supporting Anthropic Messages API and OpenAI-compatible Chat Completions,
/// plus custom Pi providers (`hub`, `kimi-coding`, etc.) defined in `~/.pi/agent/models.json` & `auth.json`.
#[derive(Clone)]
pub struct HttpLlmClient {
    http: reqwest::Client,
    anthropic_base_url: String,
    anthropic_api_key: Option<String>,
    openai_base_url: String,
    openai_api_key: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedProviderEndpoint {
    api: String,
    base_url: String,
    api_key: Option<String>,
    requires_reasoning_content: bool,
    supports_reasoning_effort: bool,
    thinking_level_map: Option<serde_json::Map<String, serde_json::Value>>,
}

fn resolve_pi_api_key_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("!cat ") {
        let path = rest.trim().trim_matches('\'').trim_matches('"');
        if let Ok(content) = std::fs::read_to_string(path) {
            let key = content.trim().to_owned();
            if !key.is_empty() {
                return Some(key);
            }
        }
    }
    if let Some(cmd) = trimmed.strip_prefix('!') {
        if let Ok(out) = std::process::Command::new("sh").arg("-c").arg(cmd).output()
            && out.status.success()
        {
            let key = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !key.is_empty() {
                return Some(key);
            }
        }
        return None;
    }
    Some(trimmed.to_owned())
}

fn resolve_pi_provider_endpoint(provider: &str, model: &str) -> Option<ResolvedProviderEndpoint> {
    let home = std::env::var("HOME").ok()?;
    let mut api = String::new();
    let mut base_url = String::new();
    let mut api_key: Option<String> = None;
    let mut requires_reasoning_content = provider == "hub" || model.contains("deepseek");
    let mut supports_reasoning_effort = true;
    let mut thinking_level_map = None;

    if let Ok(raw) = std::fs::read_to_string(format!("{home}/.pi/agent/models.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(p_obj) = val.pointer(&format!("/providers/{provider}"))
    {
        if let Some(a) = p_obj.get("api").and_then(|v| v.as_str()) {
            api = a.to_owned();
        }
        if let Some(b) = p_obj.get("baseUrl").and_then(|v| v.as_str()) {
            base_url = b.to_owned();
        }
        if let Some(k) = p_obj.get("apiKey").and_then(|v| v.as_str()) {
            api_key = resolve_pi_api_key_value(k);
        }
        if let Some(models) = p_obj.get("models").and_then(|v| v.as_array()) {
            for m in models {
                if m.get("id").and_then(|v| v.as_str()) == Some(model) {
                    if let Some(ma) = m.get("api").and_then(|v| v.as_str()) {
                        api = ma.to_owned();
                    }
                    if let Some(mb) = m.get("baseUrl").and_then(|v| v.as_str()) {
                        base_url = mb.to_owned();
                    }
                    if let Some(mk) = m.get("apiKey").and_then(|v| v.as_str())
                        && let Some(resolved_k) = resolve_pi_api_key_value(mk)
                    {
                        api_key = Some(resolved_k);
                    }
                    if let Some(compat) = m.get("compat") {
                        if let Some(req_rc) = compat
                            .get("requiresReasoningContentOnAssistantMessages")
                            .and_then(|v| v.as_bool())
                        {
                            requires_reasoning_content = req_rc;
                        }
                        supports_reasoning_effort = compat
                            .get("supportsReasoningEffort")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                    }
                    if let Some(tmap) = m.get("thinkingLevelMap").and_then(|v| v.as_object()) {
                        thinking_level_map = Some(tmap.clone());
                    }
                    break;
                }
            }
        }
    }

    if api_key.is_none()
        && let Ok(raw) = std::fs::read_to_string(format!("{home}/.pi/agent/auth.json"))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw)
        && let Some(a_obj) = val.get(provider)
    {
        if let Some(k) = a_obj
            .get("apiKey")
            .or_else(|| a_obj.get("access"))
            .or_else(|| a_obj.get("token"))
            .and_then(|v| v.as_str())
        {
            api_key = resolve_pi_api_key_value(k);
        }
    }

    if base_url.is_empty() && api_key.is_none() {
        return None;
    }

    Some(ResolvedProviderEndpoint {
        api,
        base_url,
        api_key,
        requires_reasoning_content,
        supports_reasoning_effort,
        thinking_level_map,
    })
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
        override_base_url: Option<&str>,
        override_api_key: Option<&str>,
    ) -> anyhow::Result<LlmCompletionResponse> {
        let api_key = override_api_key
            .or(self.anthropic_api_key.as_deref())
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

        let trimmed_base = override_base_url
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.anthropic_base_url)
            .trim_end_matches('/');
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
        override_base_url: Option<&str>,
        override_api_key: Option<&str>,
        resolved: Option<&ResolvedProviderEndpoint>,
    ) -> anyhow::Result<LlmCompletionResponse> {
        let api_key = override_api_key
            .or(self.openai_api_key.as_deref())
            .ok_or_else(|| anyhow::anyhow!("OPENAI_API_KEY is not configured"))?;
        let requires_reasoning_content = resolved
            .map(|r| r.requires_reasoning_content)
            .unwrap_or_else(|| req.provider == "hub" || req.model.contains("deepseek"));
        let supports_reasoning_effort = resolved
            .map(|r| r.supports_reasoning_effort)
            .unwrap_or(true);

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
                    if requires_reasoning_content {
                        obj["reasoning_content"] = serde_json::Value::String(msg.content.clone());
                    }
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
        if supports_reasoning_effort {
            let mapped_effort = if let Some(tmap) = resolved.and_then(|r| r.thinking_level_map.as_ref()) {
                tmap.get(req.think_effort.as_str())
                    .and_then(|v| v.as_str())
                    .map(str::to_owned)
            } else {
                openai_reasoning_effort(req.think_effort).map(str::to_owned)
            };
            if let Some(effort) = mapped_effort {
                body["reasoning_effort"] = serde_json::Value::String(effort);
            }
        }

        let base = override_base_url
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.openai_base_url)
            .trim_end_matches('/');
        let url = format!("{base}/chat/completions");
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

        let mut content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if content.trim().is_empty()
            && let Some(rc) = msg.get("reasoning_content").and_then(|v| v.as_str())
            && !rc.trim().is_empty()
        {
            content = rc.trim().to_owned();
        }
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
        let resolved = resolve_pi_provider_endpoint(&request.provider, &request.model);
        let override_base = resolved.as_ref().map(|r| r.base_url.as_str());
        let override_key = resolved.as_ref().and_then(|r| r.api_key.as_deref());
        let is_openai_api = resolved
            .as_ref()
            .is_some_and(|r| r.api.contains("openai") || r.api.contains("completions"));

        if !is_openai_api
            && (request.provider == "anthropic"
                || (request.provider != "hub" && request.model.starts_with("claude")))
        {
            self.complete_anthropic(request, override_base, override_key)
                .await
        } else {
            self.complete_openai(request, override_base, override_key, resolved.as_ref())
                .await
        }
    }
}
