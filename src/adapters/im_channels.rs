//! Additional IM Ecosystem Adapters (`Dingtalk` via `dws`, `Telegram`, `Slack`, `Wechat`).

use crate::adapters::traits::{ChannelAdapter, SourceAdapter, SourcePollOutput};
use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId};
use crate::core::recall::validate_im_word_limit;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

/// Generic IM adapter supporting DingTalk (`dws`), Telegram, Slack, and WeChat.
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
