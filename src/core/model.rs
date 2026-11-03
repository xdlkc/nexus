//! Core domain types and state invariants for Nexus.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Supported execution harnesses in the polymorphic worker pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessKind {
    PiNative,
    PiRpc,
    ClaudeCode,
    Codex,
    Opencode,
}

impl HarnessKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pi-native" => Some(Self::PiNative),
            "pi-rpc" => Some(Self::PiRpc),
            "claude-code" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::Opencode),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PiNative => "pi-native",
            Self::PiRpc => "pi-rpc",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
        }
    }
}

/// Reasoning effort level normalized across all worker harnesses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkEffort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

impl ThinkEffort {
    pub const SCORED_LEVELS: [Self; 5] = [
        Self::Minimal,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
        }
    }

    /// Escalates thinking effort by one tier for Watchdog auto-recovery.
    pub fn escalate(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Minimal => Self::Medium,
            Self::Low => Self::High,
            Self::Medium => Self::Xhigh,
            Self::High | Self::Xhigh => Self::Xhigh,
        }
    }
}

/// Target configuration binding a worker assignment to a specific harness, model, and thinking effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTarget {
    pub harness: HarnessKind,
    pub provider: String,
    pub model: String,
    pub think_effort: ThinkEffort,
}

/// Partial override parsed from an inline `[harness:provider/model:think]` directive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialExecutionTarget {
    pub harness: Option<HarnessKind>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub think_effort: Option<ThinkEffort>,
}

impl PartialExecutionTarget {
    pub fn is_empty(&self) -> bool {
        self.harness.is_none()
            && self.provider.is_none()
            && self.model.is_none()
            && self.think_effort.is_none()
    }

    pub fn apply_to(&self, base: &ExecutionTarget) -> ExecutionTarget {
        ExecutionTarget {
            harness: self.harness.unwrap_or(base.harness),
            provider: self.provider.clone().unwrap_or_else(|| base.provider.clone()),
            model: self.model.clone().unwrap_or_else(|| base.model.clone()),
            think_effort: self.think_effort.unwrap_or(base.think_effort),
        }
    }
}

/// Supported peripheral source kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKindId {
    Feishu,
    Dingtalk,
    Telegram,
    Slack,
    Wechat,
    Github,
    Gws,
}

impl SourceKindId {
    pub fn is_im(self) -> bool {
        matches!(
            self,
            Self::Feishu | Self::Dingtalk | Self::Telegram | Self::Slack | Self::Wechat
        )
    }
}

/// Admission strength determined by the Tier-0 Gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionStrength {
    Explicit,
    Inferred,
    NotAdmitted,
}

/// Proactive work acceptance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcceptMode {
    Off,
    Observe,
    Auto,
}

/// Destination address for outbound Channel replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelAddress {
    FeishuTopic {
        source_id: String,
        chat_id: String,
        root_message_id: String,
        reply_to_message_id: String,
    },
    GithubIssueComment {
        source_id: String,
        repo: String,
        number: u64,
    },
    GithubReviewReply {
        source_id: String,
        repo: String,
        pull_number: u64,
        comment_id: u64,
    },
    GwsGmailDraft {
        source_id: String,
        thread_id: String,
        to: String,
        subject: String,
    },
    GenericIm {
        source_id: String,
        source_kind: SourceKindId,
        conversation_id: String,
        thread_id: Option<String>,
    },
}

impl ChannelAddress {
    pub fn is_low_noise_im(&self) -> bool {
        matches!(
            self,
            Self::FeishuTopic { .. } | Self::GenericIm { .. }
        )
    }
}

/// Immutable environmental event emitted by a Source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub id: String,
    pub source_id: String,
    pub source_kind: SourceKindId,
    pub external_id: String,
    pub thread_key: String,
    pub actor_id: String,
    pub actor_is_bot: bool,
    pub addressed_agent: bool,
    pub text: String,
    pub reply_to_trigger_id: Option<String>,
    pub inline_target: Option<PartialExecutionTarget>,
    pub reply_channel: Option<ChannelAddress>,
    pub at: String,
    #[serde(default)]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// Ordered stream of Triggers belonging to one conversation or PR/Issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub thread_key: String,
    pub source_id: String,
    pub source_kind: SourceKindId,
    pub label: String,
    pub trigger_ids: Vec<String>,
    /// Tickets holding active ownership subscriptions on this Thread.
    pub subscribed_tickets: BTreeSet<String>,
    /// Tickets following this Thread for read-only context.
    pub context_only_tickets: BTreeSet<String>,
    /// Triggers waiting for the next Eval batch on this Thread.
    pub queued_trigger_ids: Vec<String>,
    pub decision_ids: Vec<String>,
    pub closed_outcome: Option<ThreadCloseOutcome>,
    #[serde(default)]
    pub meta: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCloseOutcome {
    pub outcome: String,
    pub at: String,
}

/// Proactive team rule evaluated in parallel by Jev.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProactiveRule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub match_criteria: String,
    pub enabled: bool,
    pub default_target: Option<ExecutionTarget>,
}

/// High-level task category used by Jev to select an ExecutionTarget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    GwsWorkspace,
    QuickCode,
    DeepRefactor,
    HardDebug,
}

impl TaskKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gws_workspace" => Some(Self::GwsWorkspace),
            "quick_code" => Some(Self::QuickCode),
            "deep_refactor" => Some(Self::DeepRefactor),
            "hard_debug" => Some(Self::HardDebug),
            _ => None,
        }
    }

    pub fn default_target(self) -> ExecutionTarget {
        match self {
            Self::GwsWorkspace => ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "google".to_owned(),
                model: "gemini-3.1-flash".to_owned(),
                think_effort: ThinkEffort::Low,
            },
            Self::QuickCode => ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            Self::DeepRefactor => ExecutionTarget {
                harness: HarnessKind::ClaudeCode,
                provider: "anthropic".to_owned(),
                model: "claude-opus-4-6".to_owned(),
                think_effort: ThinkEffort::High,
            },
            Self::HardDebug => ExecutionTarget {
                harness: HarnessKind::Codex,
                provider: "openai-codex".to_owned(),
                model: "gpt-5.6-sol".to_owned(),
                think_effort: ThinkEffort::Xhigh,
            },
        }
    }
}

/// Route action chosen by the Dual-Engine Router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteAction {
    Ignore,
    NewTicket,
    AttachTicket,
    StopTicket,
    NeedsLlmRouter,
}

impl RouteAction {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ignore" => Some(Self::Ignore),
            "new_ticket" => Some(Self::NewTicket),
            "attach_ticket" => Some(Self::AttachTicket),
            "stop_ticket" => Some(Self::StopTicket),
            "needs_llm_router" => Some(Self::NeedsLlmRouter),
            _ => None,
        }
    }
}

/// Mutually exclusive lifecycle stages of an open Ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStage {
    PersonDelivery,
    PersonAnswer,
    PersonReview,
    PersonBlockedFailed,
    WorkingQueued,
    Idle,
    Working,
}

/// Open ticket summary passed into Jev and Router context cards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketSummary {
    pub id: String,
    pub title: String,
    pub thread_key: String,
    pub stage: TicketStage,
    pub current_target: ExecutionTarget,
}

/// Structured report status emitted by a Worker Assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportStatus {
    Progress,
    Waiting,
    Ready,
    Blocked,
    Failed,
    Dropped,
}

/// Immutable authorization record granted when a Trigger is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mandate {
    pub policy_version: String,
    pub strength: AdmissionStrength,
    pub source_id: String,
    pub sender_id: String,
    pub matched_rule_ids: Vec<String>,
    pub granted_at: String,
}

/// Status of one Worker Assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssignmentStatus {
    Pending,
    Running,
    Ended,
}

/// Latest report payload attached to an Assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentReport {
    pub status: ReportStatus,
    pub summary: String,
    pub question: Option<String>,
    pub pull_request_url: Option<String>,
    pub at: String,
}

/// One binding of a Ticket to an ExecutionTarget instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assignment {
    pub id: String,
    pub ticket_id: string::String,
    pub target: ExecutionTarget,
    pub status: AssignmentStatus,
    pub worktree_dir: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub latest_report: Option<AssignmentReport>,
}

use std::string;

/// Context input delivered to a Ticket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketInput {
    pub id: String,
    pub kind: TicketInputKind,
    pub from_person: bool,
    pub read_by_worker: bool,
    pub handed_back_reason: Option<String>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TicketInputKind {
    TriggerFanOut {
        trigger_id: String,
        thread_key: String,
        text_preview: String,
    },
    Handoff {
        previous_assignment_id: String,
        previous_target: ExecutionTarget,
        previous_summary: String,
        git_status_diff: String,
    },
    RouterNote {
        text: String,
    },
    PolicyChanged {
        old_version: String,
        new_version: String,
        detail: String,
    },
}

/// External Thread (such as a GitHub PR) bound to a Ticket for completion tracking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowedThread {
    pub thread_key: String,
    /// True when the Ticket itself opened this PR/Thread.
    pub own: bool,
    pub closed: Option<ThreadCloseOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketEnd {
    pub outcome: String,
    pub by_person_stop: bool,
    pub reason: String,
    pub at: String,
}

/// Authorized unit of work, decoupled from any single Agent Session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub short_name: String,
    pub title: String,
    pub repo: Option<String>,
    pub origin_trigger_id: String,
    pub origin_thread_key: String,
    pub mandate: Mandate,
    pub channels: Vec<ChannelAddress>,
    pub subscriptions: BTreeSet<String>,
    pub context_subscriptions: BTreeSet<String>,
    pub follows: Vec<FollowedThread>,
    pub inputs: Vec<TicketInput>,
    pub assignments: Vec<Assignment>,
    pub delivery_error: Option<String>,
    pub created_at: String,
    pub ended: Option<TicketEnd>,
}

impl Ticket {
    pub fn is_open(&self) -> bool {
        self.ended.is_none()
    }

    pub fn latest_assignment(&self) -> Option<&Assignment> {
        self.assignments.last()
    }

    pub fn latest_assignment_mut(&mut self) -> Option<&mut Assignment> {
        self.assignments.last_mut()
    }

    pub fn current_target(&self) -> ExecutionTarget {
        self.latest_assignment()
            .map(|a| a.target.clone())
            .unwrap_or_else(|| TaskKind::QuickCode.default_target())
    }

    /// Computes the Ticket's current lifecycle stage according to 001-domain.md priority rules.
    pub fn stage(&self) -> TicketStage {
        if self.delivery_error.is_some() {
            return TicketStage::PersonDelivery;
        }

        let latest_report = self
            .latest_assignment()
            .and_then(|a| a.latest_report.as_ref());

        if let Some(report) = latest_report {
            if report.question.is_some() {
                return TicketStage::PersonAnswer;
            }
            if report.status == ReportStatus::Ready {
                return TicketStage::PersonReview;
            }
            if matches!(report.status, ReportStatus::Blocked | ReportStatus::Failed) {
                let human_arrived_since = self
                    .inputs
                    .iter()
                    .any(|inp| inp.from_person && inp.at > report.at);
                if !human_arrived_since {
                    return TicketStage::PersonBlockedFailed;
                }
            }
        }

        match self.latest_assignment() {
            None => TicketStage::WorkingQueued,
            Some(a) if a.status == AssignmentStatus::Pending => TicketStage::WorkingQueued,
            Some(a) if a.status == AssignmentStatus::Ended => TicketStage::Idle,
            Some(a) => {
                if matches!(
                    a.latest_report.as_ref().map(|r| r.status),
                    Some(ReportStatus::Waiting)
                ) {
                    TicketStage::Idle
                } else {
                    TicketStage::Working
                }
            }
        }
    }

    /// Returns true if all PRs/Threads followed by this Ticket have closed.
    pub fn follows_done(&self) -> bool {
        if self.follows.is_empty() {
            return false;
        }
        let own_follows: Vec<&FollowedThread> = self.follows.iter().filter(|f| f.own).collect();
        if !own_follows.is_empty() {
            own_follows.iter().all(|f| f.closed.is_some())
        } else {
            self.follows.iter().all(|f| f.closed.is_some())
        }
    }

    pub fn summary(&self) -> TicketSummary {
        TicketSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            thread_key: self.origin_thread_key.clone(),
            stage: self.stage(),
            current_target: self.current_target(),
        }
    }
}

/// Recorded routing decision on a Thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub thread_key: String,
    pub trigger_ids: Vec<String>,
    pub outcome: DecisionOutcome,
    pub reason: String,
    pub ticket_id: Option<String>,
    pub would_accept_rule_ids: Vec<String>,
    pub target: Option<ExecutionTarget>,
    pub at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Accept,
    Attach,
    Stop,
    Ignore,
    DropInjection,
    Unavailable,
}

/// Outbound message stored in the transactional Outbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxMessage {
    pub id: String,
    pub ticket_id: Option<String>,
    pub channel: ChannelAddress,
    pub purpose: OutboxPurpose,
    pub body: String,
    pub status: OutboxStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxPurpose {
    Acknowledgement,
    Answer,
    Result,
    Attention,
    PullEndNotice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboxStatus {
    Pending,
    Sent,
    Dropped,
}

/// Durable side-effect item queued inside an Engine transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub key: String,
    pub kind: EffectKind,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EffectKind {
    RunEval {
        thread_key: String,
        trigger_ids: Vec<String>,
    },
    LaunchAssignment {
        ticket_id: String,
        assignment_id: String,
    },
    DeliverInputs {
        ticket_id: String,
        assignment_id: String,
        input_ids: Vec<String>,
    },
    SendOutbox {
        outbox_id: String,
    },
}
