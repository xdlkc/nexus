//! DingTalk (`dws`) and Additional IM Ecosystem Adapters (`Telegram`, `Slack`, `Wechat`).
//!
//! `DingtalkAdapter` supports:
//! - Group chat message polling via `dws chat message list --conversation-id <cid> --limit 20 --format json`
//! - Auto-discovering joined groups via `dws chat +my-groups --format json`
//! - Syncing group chat members via `dws chat +chat-members-list --group <cid> --format json`
//! - Attaching emoji reactions (`OK`, `赞`, etc.) via `dws chat +messages-add-emoji`
//! - Replying in group chats as the dedicated DingTalk Bot (`奈克斯`, `--robot-code`) with message quote (`--reply --ref-sender`)

use crate::adapters::traits::{ChannelAdapter, SourceAdapter, SourcePollOutput};
use crate::core::gate::parse_inline_target_directive;
use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId, Trigger};
use crate::core::recall::validate_im_word_limit;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use tokio::process::Command;

fn default_dingtalk_source_id() -> String {
    "dingtalk-main".to_owned()
}

fn default_dingtalk_bot_name() -> String {
    "奈克斯".to_owned()
}

fn default_dingtalk_wake_keywords() -> Vec<String> {
    vec!["奈克斯".to_owned(), "nexus".to_owned()]
}

fn default_dingtalk_ack_reaction() -> Option<String> {
    Some("OK".to_owned())
}

fn default_dws_bin() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".local/bin/dws"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("dws"))
}

/// Configuration for a DingTalk (`dws`) Source & Channel adapter instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingtalkAdapterConfig {
    #[serde(default = "default_dingtalk_source_id")]
    pub source_id: String,
    #[serde(default)]
    pub chat_ids: Vec<String>,
    #[serde(default)]
    pub robot_code: Option<String>,
    #[serde(default)]
    pub bot_open_id: Option<String>,
    #[serde(default = "default_dingtalk_bot_name")]
    pub bot_name: String,
    #[serde(default = "default_dws_bin")]
    pub dws_bin: PathBuf,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default = "default_dingtalk_wake_keywords")]
    pub wake_keywords: Vec<String>,
    #[serde(default = "default_dingtalk_ack_reaction")]
    pub ack_reaction: Option<String>,
}

impl Default for DingtalkAdapterConfig {
    fn default() -> Self {
        Self {
            source_id: default_dingtalk_source_id(),
            chat_ids: Vec::new(),
            robot_code: Some("dingztmnesaskuiyqfup".to_owned()),
            bot_open_id: Some("DDUZYCHVgX4atvniSiiiiAiSmBnTVm6sbSnY8".to_owned()),
            bot_name: default_dingtalk_bot_name(),
            dws_bin: default_dws_bin(),
            profile: None,
            wake_keywords: default_dingtalk_wake_keywords(),
            ack_reaction: default_dingtalk_ack_reaction(),
        }
    }
}

/// Determines whether a DingTalk message explicitly addresses or wakes the agent.
pub fn is_dingtalk_message_addressed(cfg: &DingtalkAdapterConfig, extracted_text: &str) -> bool {
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

/// DingTalk (`dws`) adapter implementing both `SourceAdapter` and `ChannelAdapter`.
#[derive(Debug, Clone)]
pub struct DingtalkAdapter {
    pub config: DingtalkAdapterConfig,
}

impl DingtalkAdapter {
    pub fn new(config: DingtalkAdapterConfig) -> Self {
        Self { config }
    }

    fn build_command(&self) -> Command {
        let bin = if self.config.dws_bin.exists() {
            self.config.dws_bin.clone()
        } else if let Some(home) = std::env::var_os("HOME") {
            let local_dws = PathBuf::from(home).join(".local/bin/dws");
            if local_dws.exists() {
                local_dws
            } else {
                PathBuf::from("dws")
            }
        } else {
            PathBuf::from("dws")
        };
        let mut cmd = Command::new(bin);
        if let Some(profile) = &self.config.profile
            && !profile.trim().is_empty()
        {
            cmd.arg("--profile").arg(profile);
        }
        cmd
    }

    /// Attaches an emoji reaction (e.g. `"OK"`, `"赞"`) to a DingTalk message.
    pub async fn add_reaction(
        &self,
        conversation_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> anyhow::Result<()> {
        let out = self
            .build_command()
            .args([
                "chat",
                "+messages-add-emoji",
                "--conversation-id",
                conversation_id,
                "--msg-id",
                message_id,
                "--emoji",
                emoji,
                "--yes",
                "--format",
                "json",
            ])
            .output()
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("dws +messages-add-emoji failed: {stderr}");
        }
        Ok(())
    }

    /// Queries `dws chat +my-groups --format json` to auto-discover internal group chats.
    pub async fn discover_groups(&self) -> anyhow::Result<Vec<String>> {
        let out = self
            .build_command()
            .args(["chat", "+my-groups", "--format", "json"])
            .output()
            .await?;
        if !out.status.success() {
            return Ok(Vec::new());
        }
        let raw: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        let mut groups = Vec::new();
        if let Some(arr) = raw.get("groups").and_then(|v| v.as_array()) {
            for g in arr {
                let gtype = g.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(cid) = g.get("conversationId").and_then(|v| v.as_str())
                    && !cid.is_empty()
                    && (gtype == "INTERNAL_GROUP" || arr.len() == 1)
                {
                    groups.push(cid.to_owned());
                }
            }
        }
        Ok(groups)
    }

    /// Queries `dws chat +chat-members-list --group <chat_id> --format json` to sync human member `openDingtalkId`s.
    pub async fn sync_chat_members(&self, chat_id: &str) -> anyhow::Result<HashSet<String>> {
        let out = self
            .build_command()
            .args([
                "chat",
                "+chat-members-list",
                "--group",
                chat_id,
                "--format",
                "json",
            ])
            .output()
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("dws +chat-members-list failed: {stderr}");
        }
        let raw: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        let mut members = HashSet::new();
        if let Some(users) = raw.get("users").and_then(|v| v.as_array()) {
            for u in users {
                if let Some(oid) = u.get("openDingtalkId").and_then(|v| v.as_str())
                    && !oid.is_empty()
                {
                    members.insert(oid.to_owned());
                }
            }
        }
        Ok(members)
    }
}

/// Parses `dws chat message list --conversation-id <cid> --format json` output into normalized `Trigger`s.
pub fn parse_dingtalk_messages_json(
    cfg: &DingtalkAdapterConfig,
    chat_id: &str,
    raw: &serde_json::Value,
    min_create_time_str: Option<&str>,
) -> (Vec<Trigger>, Option<String>) {
    let mut triggers = Vec::new();
    let mut max_time: Option<String> = min_create_time_str.map(|s| s.to_owned());

    let items = raw
        .get("messages")
        .or_else(|| raw.pointer("/result/messages"))
        .and_then(|v| v.as_array());

    let Some(arr) = items else {
        return (triggers, max_time);
    };

    for item in arr {
        let message_id = item
            .get("openMessageId")
            .or_else(|| item.get("messageId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if message_id.is_empty() {
            continue;
        }

        let create_time = item
            .get("createTime")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();

        // Filter out messages strictly older than the saved cursor.
        // Same-second messages are allowed through because `Engine::receive_trigger` deduplicates by `trig.id`.
        if let Some(min_t) = min_create_time_str
            && !create_time.is_empty()
            && create_time.as_str() < min_t
        {
            continue;
        }
        if !create_time.is_empty() {
            max_time = Some(match max_time {
                Some(cur) if cur >= create_time => cur,
                _ => create_time.clone(),
            });
        }

        let send_type = item
            .get("sendType")
            .and_then(|v| v.as_str())
            .unwrap_or("user");
        let sender_name = item.get("sender").and_then(|v| v.as_str()).unwrap_or("");
        let sender_id = item
            .get("senderOpenDingTalkId")
            .or_else(|| item.get("senderId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if sender_id.is_empty() {
            continue;
        }

        let is_self_bot = send_type == "robot"
            || sender_name == cfg.bot_name
            || sender_name == "AI小钉"
            || cfg.bot_open_id.as_deref() == Some(sender_id.as_str());

        // Prevent self-loops from the bot's own messages (including its welcome message on joining).
        if is_self_bot {
            continue;
        }

        let raw_text = item
            .get("text")
            .or_else(|| item.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if raw_text.is_empty() {
            continue;
        }

        let addressed = is_dingtalk_message_addressed(cfg, raw_text);
        let (cleaned_text, inline_target) = parse_inline_target_directive(raw_text);

        let ref_msg_id = item
            .get("referenceOpenMessageId")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let root_id = ref_msg_id.unwrap_or(&message_id);
        let thread_key = format!("dingtalk:topic:{chat_id}:{root_id}");

        let at_iso = if create_time.contains(' ') && !create_time.contains('T') {
            format!("{}+08:00", create_time.replace(' ', "T"))
        } else if create_time.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            create_time.clone()
        };

        let mut meta = BTreeMap::new();
        meta.insert(
            "conversation_id".to_owned(),
            serde_json::Value::String(chat_id.to_owned()),
        );
        meta.insert(
            "sender_name".to_owned(),
            serde_json::Value::String(sender_name.to_owned()),
        );

        triggers.push(Trigger {
            id: format!("dingtalk-msg-{message_id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Dingtalk,
            external_id: message_id.clone(),
            thread_key,
            actor_id: sender_id.clone(),
            actor_is_bot: false,
            addressed_agent: addressed,
            text: cleaned_text,
            reply_to_trigger_id: ref_msg_id.map(|r| format!("dingtalk-msg-{r}")),
            inline_target,
            reply_channel: Some(ChannelAddress::GenericIm {
                source_id: cfg.source_id.clone(),
                source_kind: SourceKindId::Dingtalk,
                conversation_id: chat_id.to_owned(),
                thread_id: Some(format!("{message_id}|{sender_id}")),
            }),
            at: at_iso,
            meta,
        });
    }

    triggers.sort_by(|a, b| a.at.cmp(&b.at));
    (triggers, max_time)
}

impl SourceAdapter for DingtalkAdapter {
    fn source_id(&self) -> &str {
        &self.config.source_id
    }

    fn source_kind(&self) -> SourceKindId {
        SourceKindId::Dingtalk
    }

    async fn poll(&self, cursor: Option<&str>) -> anyhow::Result<SourcePollOutput> {
        let mut out = SourcePollOutput::default();
        let mut latest_cursor: Option<String> = cursor.map(|s| s.to_owned());

        let target_chats = if self.config.chat_ids.is_empty() {
            self.discover_groups().await.unwrap_or_default()
        } else {
            self.config.chat_ids.clone()
        };

        for chat_id in &target_chats {
            let output = self
                .build_command()
                .args([
                    "chat",
                    "message",
                    "list",
                    "--conversation-id",
                    chat_id,
                    "--limit",
                    "20",
                    "--format",
                    "json",
                ])
                .output()
                .await;

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
                parse_dingtalk_messages_json(&self.config, chat_id, &raw, cursor);
            out.triggers.append(&mut triggers);

            if let Some(ct) = chat_max {
                latest_cursor = Some(match latest_cursor {
                    Some(cur) if cur >= ct => cur,
                    _ => ct,
                });
            }
        }

        if let Some(c) = latest_cursor {
            out.updated_cursors.push(("latest_create_time".to_owned(), c));
        }

        Ok(out)
    }
}

impl ChannelAdapter for DingtalkAdapter {
    fn can_deliver(&self, address: &ChannelAddress) -> bool {
        matches!(
            address,
            ChannelAddress::GenericIm {
                source_id,
                source_kind: SourceKindId::Dingtalk,
                ..
            } if source_id == &self.config.source_id
        )
    }

    async fn send_message(&self, message: &OutboxMessage) -> anyhow::Result<()> {
        validate_im_word_limit(&message.body).map_err(|e| anyhow::anyhow!(e))?;
        let ChannelAddress::GenericIm {
            conversation_id,
            thread_id,
            ..
        } = &message.channel
        else {
            anyhow::bail!("DingtalkAdapter requires GenericIm channel address");
        };

        if let Some(robot_code) = self
            .config
            .robot_code
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            // Try quoted reply via `send-by-bot --reply <msg_id> --ref-sender <sender_id>` first if thread_id carries `msg_id|sender_id`
            if let Some((reply_msg_id, ref_sender)) = thread_id
                .as_deref()
                .and_then(|t| t.split_once('|'))
                && !reply_msg_id.is_empty()
                && !ref_sender.is_empty()
            {
                let quote_out = self
                    .build_command()
                    .args([
                        "chat",
                        "message",
                        "send-by-bot",
                        "--robot-code",
                        robot_code,
                        "--conversation-id",
                        conversation_id,
                        "--reply",
                        reply_msg_id,
                        "--ref-sender",
                        ref_sender,
                        "--text",
                        &message.body,
                        "--yes",
                        "--format",
                        "json",
                    ])
                    .output()
                    .await;
                if let Ok(o) = quote_out
                    && o.status.success()
                {
                    return Ok(());
                }
            }

            // Fallback to bot group Markdown message
            let bot_out = self
                .build_command()
                .args([
                    "chat",
                    "+messages-send",
                    "--as",
                    "bot",
                    "--robot-code",
                    robot_code,
                    "--group",
                    conversation_id,
                    "--title",
                    &self.config.bot_name,
                    "--text",
                    &message.body,
                    "--yes",
                    "--format",
                    "json",
                ])
                .output()
                .await?;
            if bot_out.status.success() {
                return Ok(());
            }
        }

        // Fallback to user group message via `+send-to-group`
        let out = self
            .build_command()
            .args([
                "chat",
                "+send-to-group",
                "--group",
                conversation_id,
                "--content",
                &message.body,
                "--yes",
                "--format",
                "json",
            ])
            .output()
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("DingTalk delivery failed: {stderr}");
        }
        Ok(())
    }
}

/// Generic IM adapter supporting Telegram, Slack, WeChat, and test shims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericImAdapter {
    pub source_id: String,
    pub kind: SourceKindId,
    pub cli_bin: PathBuf,
}

impl GenericImAdapter {
    pub fn new_dingtalk(source_id: impl Into<String>, dws_bin: PathBuf) -> Self {
        Self {
            source_id: source_id.into(),
            kind: SourceKindId::Dingtalk,
            cli_bin: dws_bin,
        }
    }

    pub fn new(source_id: impl Into<String>, kind: SourceKindId, cli_bin: PathBuf) -> Self {
        Self {
            source_id: source_id.into(),
            kind,
            cli_bin,
        }
    }
}

impl SourceAdapter for GenericImAdapter {
    fn source_id(&self) -> &str {
        &self.source_id
    }

    fn source_kind(&self) -> SourceKindId {
        self.kind
    }

    async fn poll(&self, _cursor: Option<&str>) -> anyhow::Result<SourcePollOutput> {
        Ok(SourcePollOutput::default())
    }
}

impl ChannelAdapter for GenericImAdapter {
    fn can_deliver(&self, address: &ChannelAddress) -> bool {
        matches!(
            address,
            ChannelAddress::GenericIm {
                source_id,
                source_kind,
                ..
            } if source_id == &self.source_id && *source_kind == self.kind
        )
    }

    async fn send_message(&self, message: &OutboxMessage) -> anyhow::Result<()> {
        validate_im_word_limit(&message.body).map_err(|e| anyhow::anyhow!(e))?;
        let ChannelAddress::GenericIm {
            conversation_id, ..
        } = &message.channel
        else {
            anyhow::bail!("GenericImAdapter requires GenericIm channel address");
        };

        let out = Command::new(&self.cli_bin)
            .args(["send", "--to", conversation_id, "--text", &message.body])
            .output()
            .await?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("IM delivery failed: {stderr}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dingtalk_messages_parser_filters_bot_and_parses_user_messages() {
        let cfg = DingtalkAdapterConfig {
            source_id: "dingtalk-main".to_owned(),
            chat_ids: vec!["cidrlrbHu3h3bSaJWi6oDzOKA==".to_owned()],
            bot_open_id: Some("DDUZYCHVgX4atvniSiiiiAiSmBnTVm6sbSnY8".to_owned()),
            bot_name: "奈克斯".to_owned(),
            robot_code: Some("dingztmnesaskuiyqfup".to_owned()),
            dws_bin: PathBuf::from("dws"),
            profile: None,
            wake_keywords: vec!["奈克斯".to_owned(), "nexus".to_owned()],
            ack_reaction: Some("OK".to_owned()),
        };

        let sample = serde_json::json!({
            "messages": [
                {
                    "openMessageId": "msg_bot_welcome",
                    "openConversationId": "cidrlrbHu3h3bSaJWi6oDzOKA==",
                    "createTime": "2026-11-04 01:36:14",
                    "sendType": "user",
                    "sender": "奈克斯",
                    "senderId": "DDUZYCHVgX4atvniSiiiiAiSmBnTVm6sbSnY8",
                    "text": "大家好！我是 奈克斯 机器人，很高兴为你们服务。"
                },
                {
                    "openMessageId": "msg_user_1",
                    "openConversationId": "cidrlrbHu3h3bSaJWi6oDzOKA==",
                    "createTime": "2026-11-04 01:40:00",
                    "sendType": "user",
                    "sender": "Lkc",
                    "senderId": "DDUZYCHVgX4aG7xTu5Kn0Bjl6QlYuNbng",
                    "text": "奈克斯，检查一下状态"
                }
            ]
        });

        let (triggers, max_time) =
            parse_dingtalk_messages_json(&cfg, "cidrlrbHu3h3bSaJWi6oDzOKA==", &sample, None);
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].external_id, "msg_user_1");
        assert!(triggers[0].addressed_agent);
        assert_eq!(
            triggers[0].actor_id,
            "DDUZYCHVgX4aG7xTu5Kn0Bjl6QlYuNbng"
        );
        assert_eq!(max_time.as_deref(), Some("2026-11-04 01:40:00"));
    }
}
