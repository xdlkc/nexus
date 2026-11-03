//! Feishu (`lark-cli` / `lark-bot`) Source and Channel Adapter.
//!
//! Supports:
//! - Group chat & topic thread message polling via `lark-cli` (`lark-bot`)
//! - Parsing text & rich-text (`post`) bodies, `@bot` mentions, and inline `[harness:model:think]` directives
//! - Syncing Feishu group chat member `open_id`s into the Gate allowlist
//! - Delivering <= 100-word in-thread replies via `/open-apis/im/v1/messages/<message_id>/reply`

use crate::adapters::traits::{ChannelAdapter, SourceAdapter, SourcePollOutput};
use crate::core::gate::parse_inline_target_directive;
use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId, Trigger};
use crate::core::recall::validate_im_word_limit;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use tokio::process::Command;

fn default_wake_keywords() -> Vec<String> {
    vec!["奈克斯".to_owned(), "nexus".to_owned()]
}

fn default_ack_reaction() -> Option<String> {
    Some("OnIt".to_owned())
}

/// Configuration for a Feishu (`lark-cli`) Source & Channel adapter instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuAdapterConfig {
    pub source_id: String,
    pub chat_ids: Vec<String>,
    pub bot_open_id: Option<String>,
    pub bot_name: String,
    pub lark_bin: PathBuf,
    pub profile: Option<String>,
    /// Whether to reply in a collapsed topic thread (`true`) or directly as a group chat message (`false`, default).
    #[serde(default)]
    pub reply_in_thread: bool,
    /// Keywords or phrases that wake the agent in a group chat even when not `@`-mentioned.
    #[serde(default = "default_wake_keywords")]
    pub wake_keywords: Vec<String>,
    /// Emoji reaction (`OnIt`, `OK`, `THUMBSUP`, `EYES`, `Typing`) attached to a user message when processing starts.
    #[serde(default = "default_ack_reaction")]
    pub ack_reaction: Option<String>,
}

impl Default for FeishuAdapterConfig {
    fn default() -> Self {
        let default_bin = std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join(".local/bin/lark-cli"))
            .filter(|p| p.exists())
            .unwrap_or_else(|| PathBuf::from("lark-cli"));
        Self {
            source_id: "feishu-main".to_owned(),
            chat_ids: Vec::new(),
            bot_open_id: None,
            bot_name: "nexus".to_owned(),
            lark_bin: default_bin,
            profile: None,
            reply_in_thread: false,
            wake_keywords: default_wake_keywords(),
            ack_reaction: default_ack_reaction(),
        }
    }
}

/// Determines whether a Feishu message explicitly addresses or wakes the agent:
/// - Direct P2P messages (`chat_type == "p2p"`)
/// - Explicit `@bot` mention in Feishu metadata
/// - Message text contains `cfg.bot_name` (e.g. `"奈克斯，你来做xxx"`) or `"@nexus"`
/// - Message text matches any keyword in `cfg.wake_keywords`
pub fn is_feishu_message_addressed(
    cfg: &FeishuAdapterConfig,
    chat_type: &str,
    extracted_text: &str,
    mentioned_in_metadata: bool,
) -> bool {
    if chat_type == "p2p" || mentioned_in_metadata {
        return true;
    }
    let lower = extracted_text.to_lowercase();
    let bot_lower = cfg.bot_name.to_lowercase();
    if !bot_lower.is_empty() && lower.contains(&bot_lower) {
        return true;
    }
    if lower.contains("@nexus") || lower.contains("奈克斯") {
        return true;
    }
    cfg.wake_keywords
        .iter()
        .any(|kw| !kw.trim().is_empty() && lower.contains(&kw.to_lowercase()))
}

/// Feishu (`lark-cli` / `lark-bot`) adapter implementing both `SourceAdapter` and `ChannelAdapter`.
#[derive(Debug, Clone)]
pub struct FeishuAdapter {
    pub config: FeishuAdapterConfig,
}

impl FeishuAdapter {
    pub fn new(config: FeishuAdapterConfig) -> Self {
        Self { config }
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.config.lark_bin);
        if let Some(profile) = &self.config.profile {
            cmd.arg("--profile").arg(profile);
        }
        cmd
    }

    /// Attaches a Feishu emoji reaction (e.g. `"OnIt"`, `"OK"`, `"THUMBSUP"`) to `message_id` as the bot.
    pub async fn add_reaction(&self, message_id: &str, emoji_type: &str) -> anyhow::Result<()> {
        let endpoint = format!("/open-apis/im/v1/messages/{message_id}/reactions");
        let payload = serde_json::json!({
            "reaction_type": {
                "emoji_type": emoji_type
            }
        })
        .to_string();

        let output = self
            .build_command()
            .args(["api", "POST", &endpoint, "--data", &payload, "--as", "bot"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("lark-cli add_reaction failed: {stderr}");
        }
        Ok(())
    }

    /// Queries `/open-apis/im/v1/chats/<chat_id>/members` via `lark-cli` to sync group member `open_id`s.
    pub async fn sync_chat_members(&self, chat_id: &str) -> anyhow::Result<HashSet<String>> {
        let endpoint = format!("/open-apis/im/v1/chats/{chat_id}/members");
        let output = self
            .build_command()
            .args(["api", "GET", &endpoint])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("lark-cli chat members failed: {stderr}");
        }

        let raw: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        Ok(parse_feishu_chat_members_json(&raw))
    }

    /// Queries `/open-apis/im/v1/chats` to auto-discover all group chats the bot has joined.
    pub async fn discover_bot_chats(&self) -> anyhow::Result<Vec<String>> {
        let output = self
            .build_command()
            .args(["api", "GET", "/open-apis/im/v1/chats"])
            .output()
            .await?;

        if !output.status.success() {
            return Ok(Vec::new());
        }

        let raw: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let mut chats = Vec::new();
        if let Some(arr) = raw
            .pointer("/data/items")
            .or_else(|| raw.pointer("/data/chats"))
            .and_then(|v| v.as_array())
        {
            for item in arr {
                if let Some(cid) = item.get("chat_id").and_then(|v| v.as_str()) {
                    chats.push(cid.to_owned());
                }
            }
        }
        Ok(chats)
    }
}

/// Parses Feishu `/open-apis/im/v1/chats/<chat_id>/members` JSON response into a set of `open_id`s.
pub fn parse_feishu_chat_members_json(raw: &serde_json::Value) -> HashSet<String> {
    let mut members = HashSet::new();
    let items = raw
        .pointer("/data/items")
        .or_else(|| raw.get("items"))
        .and_then(|v| v.as_array());

    if let Some(arr) = items {
        for item in arr {
            if let Some(id) = item
                .get("member_id")
                .or_else(|| item.get("open_id"))
                .and_then(|v| v.as_str())
            {
                members.insert(id.to_owned());
            }
        }
    }
    members
}

/// Parses Feishu `/open-apis/im/v1/messages` JSON response into normalized `Trigger`s and the latest timestamp cursor.
pub fn parse_feishu_messages_json(
    cfg: &FeishuAdapterConfig,
    chat_id: &str,
    raw: &serde_json::Value,
    min_create_time_ms: Option<u64>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut max_ts: Option<u64> = min_create_time_ms;

    let items = raw
        .pointer("/data/items")
        .or_else(|| raw.get("items"))
        .and_then(|v| v.as_array());

    let Some(arr) = items else {
        return (triggers, max_ts.map(|v| v.to_string()));
    };

    for item in arr {
        let message_id = item
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if message_id.is_empty() {
            continue;
        }

        let create_time_str = item
            .get("create_time")
            .and_then(|v| v.as_str())
            .unwrap_or("0");
        let create_time_ms = create_time_str.parse::<u64>().unwrap_or(0);
        if let Some(min_ts) = min_create_time_ms
            && create_time_ms <= min_ts {
                continue;
            }
        max_ts = Some(max_ts.map_or(create_time_ms, |cur| cur.max(create_time_ms)));

        let msg_type = item.get("msg_type").and_then(|v| v.as_str()).unwrap_or("text");
        if msg_type == "system" {
            continue;
        }

        let sender_id = item
            .pointer("/sender/id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if sender_id.is_empty() {
            continue;
        }
        let sender_type = item
            .pointer("/sender/sender_type")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let actor_is_bot = sender_type == "app"
            || cfg
                .bot_open_id
                .as_deref()
                .map(|b| b == sender_id)
                .unwrap_or(false);

        // Skip the bot's own outbound messages to prevent self-loops.
        if cfg
            .bot_open_id
            .as_deref()
            .map(|b| b == sender_id)
            .unwrap_or(false)
        {
            continue;
        }

        let root_id = item
            .get("root_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                item.get("parent_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(&message_id)
            .to_owned();

        let parent_id = item
            .get("parent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| format!("feishu-msg-{s}"));

        let raw_content = item
            .pointer("/body/content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut extracted_text = extract_feishu_message_text(raw_content);

        let mut mentioned_bot = false;
        if let Some(mentions) = item.get("mentions").and_then(|v| v.as_array()) {
            for m in mentions {
                let mid = m.pointer("/id").and_then(|v| v.as_str()).unwrap_or("");
                let mname = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let mkey = m.get("key").and_then(|v| v.as_str()).unwrap_or("");
                if cfg.bot_open_id.as_deref() == Some(mid)
                    || mname.eq_ignore_ascii_case(&cfg.bot_name)
                {
                    mentioned_bot = true;
                    if !mkey.is_empty() {
                        extracted_text = extracted_text.replace(mkey, "").trim().to_owned();
                    }
                }
            }
        }
        let chat_type = item
            .get("chat_type")
            .and_then(|v| v.as_str())
            .unwrap_or("group");
        let mentioned_bot =
            is_feishu_message_addressed(cfg, chat_type, &extracted_text, mentioned_bot);

        let (cleaned_text, inline_target) = parse_inline_target_directive(&extracted_text);
        let thread_key = format!("feishu:topic:{chat_id}:{root_id}");

        let at_iso = chrono::DateTime::from_timestamp_millis(create_time_ms as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| create_time_str.to_owned());

        triggers.push(Trigger {
            id: format!("feishu-msg-{message_id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Feishu,
            external_id: message_id.clone(),
            thread_key,
            actor_id: sender_id,
            actor_is_bot,
            addressed_agent: mentioned_bot,
            text: cleaned_text,
            reply_to_trigger_id: parent_id,
            inline_target,
            reply_channel: Some(ChannelAddress::FeishuTopic {
                source_id: cfg.source_id.clone(),
                chat_id: chat_id.to_owned(),
                root_message_id: root_id,
                reply_to_message_id: message_id,
            }),
            at: at_iso,
            meta: BTreeMap::new(),
        });
    }

    (triggers, max_ts.map(|v| v.to_string()))
}

/// Parses NDJSON lines emitted by `lark-cli event consume im.message.receive_v1 --as bot` into normalized `Trigger`s.
pub fn parse_feishu_event_ndjson(
    cfg: &FeishuAdapterConfig,
    ndjson_stdout: &str,
    min_create_time_ms: Option<u64>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut max_ts: Option<u64> = min_create_time_ms;

    for line in ndjson_stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(item) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        let message_id = item
            .get("message_id")
            .or_else(|| item.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if message_id.is_empty() {
            continue;
        }

        let chat_id = item
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or("oc_unknown")
            .to_owned();
        let chat_type = item
            .get("chat_type")
            .and_then(|v| v.as_str())
            .unwrap_or("group");

        let create_time_str = item
            .get("create_time")
            .or_else(|| item.get("timestamp"))
            .and_then(|v| v.as_str())
            .unwrap_or("0");
        let create_time_ms = create_time_str.parse::<u64>().unwrap_or(0);
        if let Some(min_ts) = min_create_time_ms
            && create_time_ms > 0
            && create_time_ms <= min_ts
        {
            continue;
        }
        if create_time_ms > 0 {
            max_ts = Some(max_ts.map_or(create_time_ms, |cur| cur.max(create_time_ms)));
        }

        let sender_id = item
            .get("sender_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let sender_type = item
            .get("sender_type")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let actor_is_bot = sender_type == "bot"
            || sender_type == "app"
            || cfg
                .bot_open_id
                .as_deref()
                .is_some_and(|b| b == sender_id);

        if cfg
            .bot_open_id
            .as_deref()
            .is_some_and(|b| b == sender_id)
        {
            continue;
        }

        let root_id = item
            .get("root_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                item.get("reply_to")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or(&message_id)
            .to_owned();

        let parent_id = item
            .get("reply_to")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| format!("feishu-msg-{s}"));

        let raw_content = item.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let extracted_text = extract_feishu_message_text(raw_content);

        let mut mentioned_bot = false;
        if let Some(mentions) = item.get("mentions").and_then(|v| v.as_array()) {
            for m in mentions {
                let mid = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let mname = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if cfg.bot_open_id.as_deref() == Some(mid)
                    || mname.eq_ignore_ascii_case(&cfg.bot_name)
                {
                    mentioned_bot = true;
                }
            }
        }
        let mentioned_bot =
            is_feishu_message_addressed(cfg, chat_type, &extracted_text, mentioned_bot);

        let (cleaned_text, inline_target) = parse_inline_target_directive(&extracted_text);
        let thread_key = format!("feishu:topic:{chat_id}:{root_id}");

        let at_iso = chrono::DateTime::from_timestamp_millis(create_time_ms as i64)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| create_time_str.to_owned());

        triggers.push(Trigger {
            id: format!("feishu-msg-{message_id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Feishu,
            external_id: message_id.clone(),
            thread_key,
            actor_id: sender_id,
            actor_is_bot,
            addressed_agent: mentioned_bot,
            text: cleaned_text,
            reply_to_trigger_id: parent_id,
            inline_target,
            reply_channel: Some(ChannelAddress::FeishuTopic {
                source_id: cfg.source_id.clone(),
                chat_id,
                root_message_id: root_id,
                reply_to_message_id: message_id,
            }),
            at: at_iso,
            meta: BTreeMap::new(),
        });
    }

    (triggers, max_ts.map(|v| v.to_string()))
}

fn extract_feishu_message_text(raw_content: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw_content) {
        if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
            return t.trim().to_owned();
        }
        // Rich-text `post` payload: collect all text elements
        let mut parts = Vec::new();
        if let Some(title) = val.get("title").and_then(|v| v.as_str())
            && !title.is_empty() {
                parts.push(title.to_owned());
            }
        if let Some(paragraphs) = val.get("content").and_then(|v| v.as_array()) {
            for para in paragraphs {
                if let Some(elems) = para.as_array() {
                    let mut line = String::new();
                    for el in elems {
                        if let Some(t) = el.get("text").and_then(|v| v.as_str()) {
                            line.push_str(t);
                        }
                    }
                    if !line.is_empty() {
                        parts.push(line);
                    }
                }
            }
        }
        if !parts.is_empty() {
            return parts.join("\n");
        }
    }
    raw_content.trim().to_owned()
}

impl SourceAdapter for FeishuAdapter {
    fn source_id(&self) -> &str {
        &self.config.source_id
    }

    fn source_kind(&self) -> SourceKindId {
        SourceKindId::Feishu
    }

    async fn poll(&self, cursor: Option<&str>) -> anyhow::Result<SourcePollOutput> {
        let min_ts = cursor.and_then(|c| c.parse::<u64>().ok());
        let mut out = SourcePollOutput::default();
        let mut latest_ts = min_ts;

        // 1. Consume real-time WebSocket events from `lark-cli event consume im.message.receive_v1 --as bot --timeout 2s`
        if let Ok(ev_out) = self
            .build_command()
            .args([
                "event",
                "consume",
                "im.message.receive_v1",
                "--as",
                "bot",
                "--timeout",
                "2s",
            ])
            .output()
            .await
            && ev_out.status.success()
        {
            let stdout_str = String::from_utf8_lossy(&ev_out.stdout);
            let (mut ev_triggers, ev_max) =
                parse_feishu_event_ndjson(&self.config, &stdout_str, min_ts);
            out.triggers.append(&mut ev_triggers);
            if let Some(ts) = ev_max.and_then(|s| s.parse::<u64>().ok()) {
                latest_ts = Some(latest_ts.map_or(ts, |cur| cur.max(ts)));
            }
        }

        // 2. Also poll REST `/open-apis/im/v1/messages` for configured or auto-discovered chats
        let target_chats = if self.config.chat_ids.is_empty() {
            self.discover_bot_chats().await.unwrap_or_default()
        } else {
            self.config.chat_ids.clone()
        };

        for chat_id in &target_chats {
            let params = serde_json::json!({
                "container_id_type": "chat",
                "container_id": chat_id,
                "page_size": 20,
            })
            .to_string();
            let mut output = self
                .build_command()
                .args([
                    "api",
                    "GET",
                    "/open-apis/im/v1/messages",
                    "--params",
                    &params,
                ])
                .output()
                .await;

            // If bot lacks `im:message.group_msg` (230027), fall back to `--as user` (`im:message.group_msg:get_as_user`).
            if output.as_ref().map(|o| !o.status.success()).unwrap_or(true) {
                output = self
                    .build_command()
                    .args([
                        "api",
                        "GET",
                        "/open-apis/im/v1/messages",
                        "--params",
                        &params,
                        "--as",
                        "user",
                    ])
                    .output()
                    .await;
            }

            let Ok(output) = output else {
                continue;
            };

            if !output.status.success() {
                continue;
            }

            let Ok(raw) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
                continue;
            };
            let (mut triggers, chat_max) =
                parse_feishu_messages_json(&self.config, chat_id, &raw, min_ts);
            out.triggers.append(&mut triggers);

            if let Some(chat_ts) = chat_max.and_then(|s| s.parse::<u64>().ok()) {
                latest_ts = Some(latest_ts.map_or(chat_ts, |cur| cur.max(chat_ts)));
            }
        }

        if let Some(ts) = latest_ts {
            out.updated_cursors
                .push(("latest_create_time_ms".to_owned(), ts.to_string()));
        }

        Ok(out)
    }
}

impl ChannelAdapter for FeishuAdapter {
    fn can_deliver(&self, address: &ChannelAddress) -> bool {
        matches!(
            address,
            ChannelAddress::FeishuTopic { source_id, .. } if source_id == &self.config.source_id
        )
    }

    async fn send_message(&self, message: &OutboxMessage) -> anyhow::Result<()> {
        validate_im_word_limit(&message.body).map_err(|e| anyhow::anyhow!(e))?;

        let ChannelAddress::FeishuTopic {
            reply_to_message_id,
            ..
        } = &message.channel
        else {
            anyhow::bail!("FeishuAdapter cannot deliver non-FeishuTopic address");
        };

        let endpoint = format!("/open-apis/im/v1/messages/{reply_to_message_id}/reply");
        let content_json = serde_json::json!({ "text": message.body }).to_string();
        let payload = serde_json::json!({
            "msg_type": "text",
            "content": content_json,
            "reply_in_thread": self.config.reply_in_thread,
        })
        .to_string();

        let output = self
            .build_command()
            .args(["api", "POST", &endpoint, "--data", &payload, "--as", "bot"])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("lark-cli reply failed: {stderr}");
        }

        Ok(())
    }
}
