export type AcceptMode = 'off' | 'observe' | 'auto';
export type HarnessKind = 'codex' | 'claude-code' | 'pi-native' | 'pi-rpc' | 'opencode';
export type ThinkEffort = 'off' | 'minimal' | 'low' | 'medium' | 'high' | 'xhigh';
export type TicketPhase = 'reproducing' | 'fixing' | 'awaiting_review' | 'awaiting_info' | 'blocked';
export type ReportStatus = 'progress' | 'ready' | 'waiting' | 'blocked' | 'failed' | 'dropped';

export interface ExecutionTarget {
  harness: HarnessKind;
  provider: string;
  model: string;
  think_effort: ThinkEffort;
}

export interface WorkerReport {
  status: ReportStatus;
  summary: string;
  question?: string | null;
  pull_request_thread_key?: string | null;
  pull_request_url?: string | null;
  at: string;
}

export interface Assignment {
  id: string;
  target: ExecutionTarget;
  worktree_dir: string;
  status: 'running' | 'ended';
  started_at: string;
  ended_at?: string | null;
  end_reason?: string | null;
  latest_report?: WorkerReport | null;
}

export interface FollowedThread {
  thread_key: string;
  own: boolean;
  closed?: string | null;
}

export interface Ticket {
  id: string;
  short_name: string;
  title: string;
  repo?: string | null;
  status: 'open' | 'closed';
  resolution?: string | null;
  phase?: TicketPhase | null;
  subscriptions: string[];
  follows: FollowedThread[];
  assignments: Assignment[];
  created_at: string;
  updated_at: string;
}

export interface Trigger {
  id: string;
  source_id: string;
  source_kind: string;
  external_id: string;
  thread_key: string;
  actor_id: string;
  actor_is_bot: boolean;
  addressed_agent: boolean;
  text: string;
  at: string;
}

export interface OutboxMessage {
  id: string;
  ticket_id?: string | null;
  body: string;
  status: 'pending' | 'sent' | 'failed';
  attempts: number;
  last_error?: string | null;
  created_at: string;
}

export interface DecisionRecord {
  id: string;
  trigger_ids: string[];
  outcome: string;
  ticket_id?: string | null;
  target?: ExecutionTarget | null;
  reason: string;
  would_accept_rule_ids: string[];
  at: string;
}

export interface AgentIdentityConfig {
  git_user_name: string;
  git_user_email: string;
  github_agent_token?: string | null;
  isolate_gh_config: boolean;
  gws_account?: string | null;
  dws_profile?: string | null;
}

export interface HttpJevConfig {
  protocol: string;
  base_url: string;
  model: string;
  api_key: string;
}

export interface RouterLlmConfig {
  provider: string;
  model: string;
  think_effort: ThinkEffort;
  max_turns: number;
}

export interface NexusConfig {
  state_dir: string;
  worktree_root: string;
  http_addr: string;
  accept_mode: AcceptMode;
  max_concurrent_workers: number;
  default_repo?: string | null;
  repositories: Record<string, string>;
  allowed_actors: string[];
  feishu: {
    source_id: string;
    chat_ids: string[];
    bot_open_id?: string | null;
    bot_name: string;
    profile?: string | null;
    reply_in_thread: boolean;
    wake_keywords: string[];
    ack_reaction?: string | null;
  };
  github: {
    source_id: string;
    repos: string[];
    bot_login: string;
    followed_pr_threads: string[];
  };
  agent_identity: AgentIdentityConfig;
  proactive_rules: Array<{
    id: string;
    name: string;
    description: string;
    match_criteria: string;
    enabled: boolean;
    default_target?: ExecutionTarget | null;
  }>;
  router_llm: RouterLlmConfig;
  jev?: HttpJevConfig | null;
}

export interface ToolProbe {
  binary: string;
  installed: boolean;
  version_or_note: string;
}

export interface NexusOverview {
  state: {
    accept_mode?: AcceptMode | null;
    triggers: Record<string, Trigger>;
    tickets: Record<string, Ticket>;
    decisions: Record<string, DecisionRecord>;
    outbox: Record<string, OutboxMessage>;
  };
  config: NexusConfig;
  memory: string;
  toolchain: ToolProbe[];
}

export interface WorkerTraceStep {
  seq: number;
  at: string;
  assignment_id: string;
  ticket_id: string;
  kind: 'launch' | 'thinking' | 'tool_call' | 'tool_result' | 'usage' | 'finish' | 'error' | 'message' | string;
  title: string;
  detail: string;
}

export interface WorkerTraceResponse {
  ok: boolean;
  ticket_id?: string;
  title?: string;
  short_name?: string;
  assignments?: Assignment[];
  steps: WorkerTraceStep[];
}

export interface IdentityStatusResponse {
  ok: boolean;
  workspace: {
    state_dir: string;
    agent_home: string;
    worker_bin: string;
    gitconfig_path: string;
    gh_config_dir: string;
    gws_config_dir: string;
    dws_config_dir: string;
  };
  git: {
    host_name: string;
    host_email: string;
    agent_name: string;
    agent_email: string;
    isolated: boolean;
  };
  github: {
    host_login: string;
    agent_login: string;
    configured_bot_login: string;
    agent_authenticated: boolean;
    agent_note: string;
    isolated: boolean;
    has_dedicated_token: boolean;
    has_isolated_hosts_yml: boolean;
    login_cmd: string;
  };
  feishu: {
    bot_name: string;
    bot_open_id?: string | null;
    profile?: string | null;
    isolated: boolean;
  };
  gws: {
    account?: string | null;
    config_dir: string;
    isolated: boolean;
  };
  dws: {
    profile?: string | null;
    config_dir: string;
    isolated: boolean;
  };
}

export interface PiModelRow {
  provider: string;
  model: string;
}
