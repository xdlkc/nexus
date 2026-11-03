//! Unified `SourceAdapter` and `ChannelAdapter` traits decoupling external I/O from the Nexus `Engine`.

use crate::core::model::{ChannelAddress, OutboxMessage, SourceKindId, Trigger};

/// Output returned by a single `SourceAdapter::poll` cycle.
#[derive(Debug, Clone, Default)]
pub struct SourcePollOutput {
    /// Newly discovered environmental events normalized into `Trigger`s.
    pub triggers: Vec<Trigger>,
    /// External threads (such as GitHub PRs) that transitioned to closed or merged: `(thread_key, outcome)`.
    pub closed_threads: Vec<(String, String)>,
    /// Updated polling cursor `(cursor_key, cursor_value)` to persist in `SqliteStore`.
    pub updated_cursors: Vec<(String, String)>,
}

/// Trait implemented by every peripheral event source (`Feishu`, `Github`, `Gws`, `Dingtalk`, `Telegram`, `Slack`, `Wechat`).
pub trait SourceAdapter: Send + Sync {
    fn source_id(&self) -> &str;
    fn source_kind(&self) -> SourceKindId;

    fn poll(
        &self,
        cursor: Option<&str>,
    ) -> impl std::future::Future<Output = anyhow::Result<SourcePollOutput>> + Send;
}

/// Trait implemented by every outbound reply channel.
pub trait ChannelAdapter: Send + Sync {
    fn can_deliver(&self, address: &ChannelAddress) -> bool;

    fn send_message(
        &self,
        message: &OutboxMessage,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + Send;
}
