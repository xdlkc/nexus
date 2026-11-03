//! Google Workspace (`gws` / `gws-agent`) Source and Channel Adapter.

use crate::adapters::traits::{ChannelAdapter, SourceAdapter, SourcePollOutput};
use crate::core::gate::parse_inline_target_directive;
use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GwsAdapterConfig {
    pub source_id: String,
    pub gws_bin: PathBuf,
    pub account: Option<String>,
}

impl Default for GwsAdapterConfig {
    fn default() -> Self {
        Self {
            source_id: "gws-main".to_owned(),
            gws_bin: PathBuf::from("gws"),
            account: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GwsAdapter {
    pub config: GwsAdapterConfig,
}

impl GwsAdapter {
    pub fn new(config: GwsAdapterConfig) -> Self {
        Self { config }
    }
}

/// Parses `gws gmail messages list --format json` output into normalized `Trigger`s.
pub fn parse_gws_gmail_json(cfg: &GwsAdapterConfig, raw: &serde_json::Value) -> Vec<Trigger> {
    let mut triggers = Vec::new();
    let Some(items) = raw
        .get("messages")
        .or_else(|| raw.get("items"))
        .and_then(|v| v.as_array())
    else {
        return triggers;
    };

    for item in items {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned();
        if id.is_empty() {
            continue;
        }
        let thread_id = item
            .get("threadId")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_owned();
        let from = item
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown@example.com")
            .to_owned();
        let subject = item
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("(no subject)")
            .to_owned();
        let snippet = item
            .get("snippet")
            .or_else(|| item.get("body"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let (clean_text, inline_target) =
            parse_inline_target_directive(&format!("{subject}\n{snippet}"));

        triggers.push(Trigger {
            id: format!("gws-mail-{id}"),
            source_id: cfg.source_id.clone(),
            source_kind: SourceKindId::Gws,
            external_id: id,
            thread_key: format!("gws:gmail:{thread_id}"),
            actor_id: from.clone(),
            actor_is_bot: false,
            addressed_agent: true,
            text: clean_text,
            reply_to_trigger_id: None,
            inline_target,
            reply_channel: Some(ChannelAddress::GwsGmailDraft {
                source_id: cfg.source_id.clone(),
                thread_id,
                to: from,
                subject: format!("Re: {subject}"),
            }),
            at: item
                .get("date")
                .and_then(|v| v.as_str())
                .unwrap_or("2026-11-03T00:00:00Z")
                .to_owned(),
            meta: BTreeMap::new(),
        });
    }
    triggers
}

impl SourceAdapter for GwsAdapter {
    fn source_id(&self) -> &str {
        &self.config.source_id
    }

    fn source_kind(&self) -> SourceKindId {
        SourceKindId::Gws
    }

    async fn poll(&self, _cursor: Option<&str>) -> anyhow::Result<SourcePollOutput> {
        let out = Command::new(&self.config.gws_bin)
            .args([
                "gmail",
                "users",
                "messages",
                "list",
                "--params",
                "{\"userId\":\"me\",\"q\":\"is:unread\"}",
            ])
            .output()
            .await?;

        if !out.status.success() {
            return Ok(SourcePollOutput::default());
        }
        let raw: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        Ok(SourcePollOutput {
            triggers: parse_gws_gmail_json(&self.config, &raw),
            closed_threads: Vec::new(),
            updated_cursors: Vec::new(),
        })
    }
}

impl ChannelAdapter for GwsAdapter {
    fn can_deliver(&self, address: &ChannelAddress) -> bool {
        matches!(
            address,
            ChannelAddress::GwsGmailDraft { source_id, .. } if source_id == &self.config.source_id
        )
    }

    async fn send_message(&self, message: &OutboxMessage) -> anyhow::Result<()> {
        let ChannelAddress::GwsGmailDraft {
            thread_id,
            to,
            subject,
            ..
        } = &message.channel
        else {
            anyhow::bail!("GwsAdapter requires GwsGmailDraft address");
        };

        let payload = serde_json::json!({
            "threadId": thread_id,
            "to": to,
            "subject": subject,
            "body": message.body,
        })
        .to_string();

        let out = Command::new(&self.config.gws_bin)
            .args(["gmail", "users", "drafts", "create", "--json", &payload])
            .output()
            .await?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("gws draft creation failed: {stderr}");
        }
        Ok(())
    }
}
