import React, { useEffect, useMemo, useState } from 'react';
import {
  Activity,
  Bot,
  BookOpen,
  CheckCircle2,
  Cpu,
  ExternalLink,
  Filter,
  GitBranch,
  GitPullRequest,
  Layers,
  MessageSquare,
  Play,
  RefreshCw,
  Search,
  Send,
  Settings,
  Shield,
  Sparkles,
  Square,
  Terminal,
  Wrench,
  X,
  Zap,
} from 'lucide-react';
import {
  dispatchSimulatedChat,
  getIdentityStatus,
  getOverview,
  getWorkerModels,
  getWorkerTrace,
  saveMemoryContent,
  setAcceptMode,
  stopTicket,
  switchWorkerTarget,
  testJevTriage,
  updateSystemConfig,
} from './api/client';
import type {
  AcceptMode,
  HarnessCatalog,
  HarnessKind,
  IdentityStatusResponse,
  NexusOverview,
  ThinkEffort,
  Ticket,
  WorkerTraceResponse,
} from './types/nexus';
import { StepTimelineItem } from './components/StepBadge';
import { IdentityMatrix } from './components/IdentityMatrix';

type ActiveTab = 'tickets' | 'trajectory' | 'conversation' | 'decisions' | 'memory' | 'settings' | 'toolchain';

export default function App() {
  const [activeTab, setActiveTab] = useState<ActiveTab>('tickets');
  const [overview, setOverview] = useState<NexusOverview | null>(null);
  const [identityStatus, setIdentityStatus] = useState<IdentityStatusResponse | null>(null);
  const [identityLoading, setIdentityLoading] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [toast, setToast] = useState<string | null>(null);

  // Quick command dispatch bar
  const [quickPrompt, setQuickPrompt] = useState('');
  const [dispatching, setDispatching] = useState(false);

  // Ticket filters & Slide-over Trajectory Drawer
  const [ticketSearch, setTicketSearch] = useState('');
  const [repoFilter, setRepoFilter] = useState<string>('all');
  const [drawerTicketId, setDrawerTicketId] = useState<string | null>(null);
  const [drawerTrace, setDrawerTrace] = useState<WorkerTraceResponse | null>(null);
  const [drawerLoading, setDrawerLoading] = useState(false);

  // Full-screen Trajectory Studio
  const [selectedTraceTicketId, setSelectedTraceTicketId] = useState<string>('');
  const [fullTrace, setFullTrace] = useState<WorkerTraceResponse | null>(null);
  const [traceKindFilter, setTraceKindFilter] = useState<string>('all');

  // Mid-Flight Switch Modal
  const [switchModalTicket, setSwitchModalTicket] = useState<Ticket | null>(null);
  const [switchHarness, setSwitchHarness] = useState<HarnessKind>('codex');
  const [switchProvider, setSwitchProvider] = useState('openai');
  const [switchModel, setSwitchModel] = useState('gpt-6-astra');
  const [switchThink, setSwitchThink] = useState<ThinkEffort>('high');
  const [switchReason, setSwitchReason] = useState('管理控制台手动热切换执行引擎');
  const [harnessCatalogs, setHarnessCatalogs] = useState<Record<string, HarnessCatalog>>({});

  // Memory Editor
  const [memoryDraft, setMemoryDraft] = useState('');
  const [memoryDirty, setMemoryDirty] = useState(false);

  // System Config Form
  const [cfgDirty, setCfgDirty] = useState(false);
  const [botName, setBotName] = useState('奈克斯');
  const [feishuProfile, setFeishuProfile] = useState('cli_aa62798efef8dcec');
  const [replyInThread, setReplyInThread] = useState(false);
  const [ackReaction, setAckReaction] = useState('OnIt');
  const [wakeKeywords, setWakeKeywords] = useState('奈克斯, nexus');
  const [chatIds, setChatIds] = useState('');
  const [githubRepos, setGithubRepos] = useState('xdlkc/nexus, xdlkc/clipboard-history');
  const [defaultRepo, setDefaultRepo] = useState('xdlkc/nexus');
  const [allowedActors, setAllowedActors] = useState('');
  const [gitUserName, setGitUserName] = useState('Nexus Agent (奈克斯)');
  const [gitUserEmail, setGitUserEmail] = useState('246123951+lkc12138-cloud@users.noreply.github.com');
  const [githubBotLogin, setGithubBotLogin] = useState('lkc12138-cloud');
  const [githubAgentToken, setGithubAgentToken] = useState('');
  const [isolateGhConfig, setIsolateGhConfig] = useState(true);
  const [gwsAccount, setGwsAccount] = useState('');
  const [dwsProfile, setDwsProfile] = useState('');
  const [defHarness, setDefHarness] = useState<HarnessKind>('codex');
  const [defProvider, setDefProvider] = useState('openai');
  const [defModel, setDefModel] = useState('gpt-6-astra');
  const [defThink, setDefThink] = useState<ThinkEffort>('medium');
  const [jevMode, setJevMode] = useState('typesafe-system-one');
  const [jevBaseUrl, setJevBaseUrl] = useState('https://api.typesafe.ai/v1');
  const [jevModel, setJevModel] = useState('jev-1');
  const [jevApiKey, setJevApiKey] = useState('');
  const [routerProvider, setRouterProvider] = useState('anthropic');
  const [routerModel, setRouterModel] = useState('claude-sonnet-4-6');
  const [routerThink, setRouterThink] = useState<ThinkEffort>('medium');

  // Jev Live Tester
  const [jevTestInput, setJevTestInput] = useState('奈克斯，帮我排查一下高并发死锁 race condition 问题');
  const [jevTestOutput, setJevTestOutput] = useState<string | null>(null);

  const notify = (msg: string) => {
    setToast(msg);
    setTimeout(() => setToast(null), 2800);
  };

  const loadOverview = async () => {
    try {
      const data = await getOverview();
      setOverview(data);

      if (!memoryDirty && typeof data.memory === 'string') {
        setMemoryDraft(data.memory);
      }

      if (!cfgDirty && data.config) {
        const c = data.config;
        setBotName(c.feishu?.bot_name || '奈克斯');
        setFeishuProfile(c.feishu?.profile || '');
        setReplyInThread(Boolean(c.feishu?.reply_in_thread));
        setAckReaction(c.feishu?.ack_reaction || '');
        setWakeKeywords((c.feishu?.wake_keywords || []).join(', '));
        setChatIds((c.feishu?.chat_ids || []).join(', '));
        setGithubRepos((c.github?.repos || []).join(', '));
        setDefaultRepo(c.default_repo || 'xdlkc/nexus');
        setAllowedActors((c.allowed_actors || []).join(', '));

        const aid = c.agent_identity || ({} as NexusOverview['config']['agent_identity']);
        setGitUserName(aid.git_user_name || 'Nexus Agent (奈克斯)');
        setGitUserEmail(aid.git_user_email || '246123951+lkc12138-cloud@users.noreply.github.com');
        setGithubBotLogin(c.github?.bot_login || 'lkc12138-cloud');
        setGithubAgentToken(aid.github_agent_token || '');
        setIsolateGhConfig(Boolean(aid.isolate_gh_config));
        setGwsAccount(aid.gws_account || '');
        setDwsProfile(aid.dws_profile || '');

        const dt = c.proactive_rules?.[0]?.default_target;
        if (dt) {
          setDefHarness(dt.harness || 'codex');
          setDefProvider(dt.provider || 'openai');
          setDefModel(dt.model || 'gpt-6-astra');
          setDefThink(dt.think_effort || 'medium');
        }

        if (c.jev && c.jev.protocol) {
          setJevMode(c.jev.protocol);
          setJevBaseUrl(c.jev.base_url || '');
          setJevModel(c.jev.model || 'jev-1');
          setJevApiKey(c.jev.api_key || '');
        } else {
          setJevMode('heuristic');
        }

        if (c.router_llm) {
          setRouterProvider(c.router_llm.provider || 'anthropic');
          setRouterModel(c.router_llm.model || 'claude-sonnet-4-6');
          setRouterThink(c.router_llm.think_effort || 'medium');
        }
      }
    } catch {
      // silent retry on next poll
    }
  };

  const loadIdentity = async () => {
    setIdentityLoading(true);
    try {
      const st = await getIdentityStatus();
      setIdentityStatus(st);
    } catch {
      // ignore
    } finally {
      setIdentityLoading(false);
    }
  };

  useEffect(() => {
    loadOverview();
    loadIdentity();
    getWorkerModels()
      .then((res) => setHarnessCatalogs(res.harnesses || {}))
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!autoRefresh) return;
    const timer = setInterval(() => {
      loadOverview();
      if (drawerTicketId) {
        getWorkerTrace(drawerTicketId).then(setDrawerTrace).catch(() => {});
      }
      if (activeTab === 'trajectory' && selectedTraceTicketId) {
        getWorkerTrace(selectedTraceTicketId).then(setFullTrace).catch(() => {});
      }
    }, 3000);
    return () => clearInterval(timer);
  }, [autoRefresh, drawerTicketId, activeTab, selectedTraceTicketId, memoryDirty, cfgDirty]);

  // Sorted tickets (newest first)
  const tickets = useMemo(() => {
    if (!overview?.state?.tickets) return [];
    return Object.values(overview.state.tickets).sort((a, b) =>
      (b.created_at || '').localeCompare(a.created_at || '')
    );
  }, [overview]);

  useEffect(() => {
    if (!selectedTraceTicketId && tickets.length > 0) {
      setSelectedTraceTicketId(tickets[0].id);
    }
  }, [tickets, selectedTraceTicketId]);

  useEffect(() => {
    if (activeTab === 'trajectory' && selectedTraceTicketId) {
      getWorkerTrace(selectedTraceTicketId).then(setFullTrace).catch(() => {});
    }
  }, [activeTab, selectedTraceTicketId]);

  const filteredTickets = useMemo(() => {
    return tickets.filter((t) => {
      if (repoFilter !== 'all' && (t.repo || 'xdlkc/nexus') !== repoFilter) return false;
      if (!ticketSearch.trim()) return true;
      const q = ticketSearch.toLowerCase();
      return (
        t.id.toLowerCase().includes(q) ||
        t.title.toLowerCase().includes(q) ||
        (t.repo || '').toLowerCase().includes(q) ||
        t.subscriptions.some((s) => s.toLowerCase().includes(q))
      );
    });
  }, [tickets, repoFilter, ticketSearch]);

  const openTrajectoryDrawer = async (ticketId: string) => {
    setDrawerTicketId(ticketId);
    setDrawerLoading(true);
    try {
      const tr = await getWorkerTrace(ticketId);
      setDrawerTrace(tr);
    } finally {
      setDrawerLoading(false);
    }
  };

  const handleQuickDispatch = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!quickPrompt.trim()) return;
    setDispatching(true);
    try {
      await dispatchSimulatedChat(quickPrompt.trim());
      setQuickPrompt('');
      notify('已注入双引擎路由队列并创建 Worker 任务！');
      await loadOverview();
    } finally {
      setDispatching(false);
    }
  };

  const handleAcceptModeChange = async (mode: AcceptMode) => {
    await setAcceptMode(mode);
    notify(`已切换全局准入模式为 ${mode.toUpperCase()}`);
    await loadOverview();
  };

  const handleSaveConfig = async () => {
    const splitCsv = (s: string) =>
      s
        .split(',')
        .map((x) => x.trim())
        .filter(Boolean);
    const jevEnabled = jevMode !== 'heuristic';
    await updateSystemConfig({
      bot_name: botName.trim(),
      feishu_profile: feishuProfile.trim(),
      github_bot_login: githubBotLogin.trim(),
      reply_in_thread: replyInThread,
      ack_reaction: ackReaction,
      wake_keywords: splitCsv(wakeKeywords),
      chat_ids: splitCsv(chatIds),
      github_repos: splitCsv(githubRepos),
      default_repo: defaultRepo.trim() || 'xdlkc/nexus',
      allowed_actors: splitCsv(allowedActors),
      agent_identity: {
        git_user_name: gitUserName.trim() || 'Nexus Agent (奈克斯)',
        git_user_email: gitUserEmail.trim() || '246123951+lkc12138-cloud@users.noreply.github.com',
        github_agent_token: githubAgentToken.trim() || null,
        isolate_gh_config: isolateGhConfig,
        gws_account: gwsAccount.trim() || null,
        dws_profile: dwsProfile.trim() || null,
      },
      default_target: {
        harness: defHarness,
        provider: defProvider.trim() || 'openai',
        model: defModel.trim() || 'gpt-6-astra',
        think_effort: defThink,
      },
      jev_enabled: jevEnabled,
      jev: jevEnabled
        ? {
            protocol: jevMode,
            base_url: jevBaseUrl.trim(),
            model: jevModel.trim() || 'jev-1',
            api_key: jevApiKey.trim(),
          }
        : null,
      router_llm: {
        provider: routerProvider.trim() || 'anthropic',
        model: routerModel.trim() || 'claude-sonnet-4-6',
        think_effort: routerThink,
        max_turns: 4,
      },
    });
    setCfgDirty(false);
    notify('系统配置与独立工作空间垫片已保存并热更新生效！');
    await loadOverview();
    await loadIdentity();
  };

  const acceptMode: AcceptMode =
    overview?.state?.accept_mode || overview?.config?.accept_mode || 'auto';

  const openTicketCount = tickets.filter((t) => t.status === 'open').length;
  const decisionList = useMemo(() => {
    if (!overview?.state?.decisions) return [];
    return Object.values(overview.state.decisions).reverse();
  }, [overview]);

  const chatStream = useMemo(() => {
    if (!overview?.state) return [];
    const items: Array<{
      id: string;
      at: string;
      role: 'user' | 'bot' | 'thinking';
      title: string;
      text: string;
      badge?: string;
    }> = [];

    for (const tr of Object.values(overview.state.triggers || {})) {
      items.push({
        id: tr.id,
        at: tr.at,
        role: 'user',
        title: `${tr.source_kind.toUpperCase()} · ${tr.actor_id}`,
        text: tr.text,
        badge: tr.thread_key,
      });
    }
    for (const ob of Object.values(overview.state.outbox || {})) {
      const isThinking = ob.body.startsWith('💭');
      items.push({
        id: ob.id,
        at: ob.created_at,
        role: isThinking ? 'thinking' : 'bot',
        title: isThinking
          ? `奈克斯 · 实时思考流 (${ob.ticket_id || ''})`
          : `奈克斯 · 回复投递 (${ob.status.toUpperCase()})`,
        text: ob.body,
        badge: ob.ticket_id || undefined,
      });
    }
    return items.sort((a, b) => (b.at || '').localeCompare(a.at || ''));
  }, [overview]);

  return (
    <div className="min-h-screen flex bg-[#07090e] text-slate-100">
      {/* LEFT SIDEBAR */}
      <aside className="w-64 shrink-0 border-r border-slate-800/80 bg-[#0a0d14]/95 backdrop-blur-xl flex flex-col justify-between p-4 sticky top-0 h-screen">
        <div>
          {/* Brand Header */}
          <div className="flex items-center gap-3 px-2 py-2.5 mb-5">
            <div className="w-9 h-9 rounded-xl bg-gradient-to-br from-indigo-500 via-violet-500 to-cyan-400 flex items-center justify-center shadow-lg shadow-indigo-500/20">
              <Zap className="w-5 h-5 text-white" />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <span className="font-bold tracking-tight text-sm text-white">NEXUS</span>
                <span className="text-[10px] font-mono uppercase px-1.5 py-0.5 rounded bg-indigo-500/15 text-indigo-300 border border-indigo-500/30">
                  TS v0.2
                </span>
              </div>
              <p className="text-[11px] text-slate-400">Super-Agent Control Plane</p>
            </div>
          </div>

          {/* Navigation */}
          <nav className="space-y-1">
            <SidebarButton
              active={activeTab === 'tickets'}
              onClick={() => setActiveTab('tickets')}
              icon={<Layers className="w-4 h-4" />}
              label="活跃工单与热切换"
              badge={String(tickets.length)}
            />
            <SidebarButton
              active={activeTab === 'trajectory'}
              onClick={() => setActiveTab('trajectory')}
              icon={<Terminal className="w-4 h-4" />}
              label="Worker 内部轨迹"
              badge="LIVE"
              badgeColor="emerald"
            />
            <SidebarButton
              active={activeTab === 'conversation'}
              onClick={() => setActiveTab('conversation')}
              icon={<MessageSquare className="w-4 h-4" />}
              label="群聊上下文与回复流"
              badge={String(chatStream.length)}
            />
            <SidebarButton
              active={activeTab === 'decisions'}
              onClick={() => setActiveTab('decisions')}
              icon={<Activity className="w-4 h-4" />}
              label="双引擎路由与 Jev"
              badge={String(decisionList.length)}
            />
            <SidebarButton
              active={activeTab === 'memory'}
              onClick={() => setActiveTab('memory')}
              icon={<BookOpen className="w-4 h-4" />}
              label="长期记忆 (MEMORY.md)"
            />
            <SidebarButton
              active={activeTab === 'settings'}
              onClick={() => setActiveTab('settings')}
              icon={<Settings className="w-4 h-4" />}
              label="独立身份与系统配置"
            />
            <SidebarButton
              active={activeTab === 'toolchain'}
              onClick={() => {
                setActiveTab('toolchain');
                loadIdentity();
              }}
              icon={<Wrench className="w-4 h-4" />}
              label="工具链与隔离探针"
            />
          </nav>
        </div>

        {/* Bottom Identity & Daemon Card */}
        <div className="space-y-3 pt-4 border-t border-slate-800/80">
          <div className="rounded-xl bg-[#0e131f] border border-slate-800/90 p-3 text-xs space-y-1.5">
            <div className="flex items-center justify-between">
              <span className="text-slate-400 flex items-center gap-1.5">
                <span className="w-2 h-2 rounded-full bg-emerald-400 animate-pulse" />
                飞书 Bot
              </span>
              <span className="font-semibold text-slate-200">
                {overview?.config?.feishu?.bot_name || '奈克斯'}
              </span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-slate-400 flex items-center gap-1.5">
                <GitBranch className="w-3.5 h-3.5 text-indigo-400" />
                独立 GitHub
              </span>
              <code className="text-emerald-300 font-mono text-[11px]">
                @{identityStatus?.github?.agent_login || overview?.config?.github?.bot_login || 'lkc12138-cloud'}
              </code>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-slate-400">默认迭代仓库</span>
              <code className="text-indigo-300 font-mono text-[11px]">
                {overview?.config?.default_repo || 'xdlkc/nexus'}
              </code>
            </div>
          </div>

          <label className="flex items-center justify-between px-2 text-xs text-slate-400 cursor-pointer">
            <span>3s 实时流式同步</span>
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
              className="accent-indigo-500 rounded"
            />
          </label>
        </div>
      </aside>

      {/* MAIN WORKSPACE */}
      <div className="flex-1 flex flex-col min-w-0">
        {/* TOP COMMAND HEADER */}
        <header className="sticky top-0 z-20 border-b border-slate-800/80 bg-[#07090e]/85 backdrop-blur-xl px-6 py-3.5 flex flex-wrap items-center justify-between gap-4">
          <form onSubmit={handleQuickDispatch} className="flex-1 min-w-[320px] max-w-2xl flex items-center gap-2">
            <div className="relative flex-1">
              <Sparkles className="w-4 h-4 text-indigo-400 absolute left-3.5 top-1/2 -translate-y-1/2" />
              <input
                type="text"
                value={quickPrompt}
                onChange={(e) => setQuickPrompt(e.target.value)}
                placeholder="直接向奈克斯下达任务 (支持 [codex:xhigh] / [claude-code:high] 内联指令)..."
                className="w-full pl-10 pr-4 py-2 rounded-xl bg-[#0d121d] border border-slate-800 focus:border-indigo-500/70 focus:outline-none text-xs text-slate-100 placeholder:text-slate-500 transition"
              />
            </div>
            <button
              type="submit"
              disabled={dispatching}
              className="inline-flex items-center gap-1.5 px-4 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 disabled:opacity-50 text-xs font-semibold text-white shadow-lg shadow-indigo-600/20 transition cursor-pointer"
            >
              <Send className="w-3.5 h-3.5" />
              派发任务
            </button>
          </form>

          {/* Accept Mode Segmented Control */}
          <div className="flex items-center gap-3">
            <span className="text-xs text-slate-400">准入模式:</span>
            <div className="inline-flex rounded-xl bg-[#0d121d] p-1 border border-slate-800">
              {(['off', 'observe', 'auto'] as AcceptMode[]).map((m) => (
                <button
                  key={m}
                  type="button"
                  onClick={() => handleAcceptModeChange(m)}
                  className={`px-3 py-1 rounded-lg text-xs font-medium transition cursor-pointer ${
                    acceptMode === m
                      ? m === 'auto'
                        ? 'bg-emerald-500/20 text-emerald-300 border border-emerald-500/40'
                        : 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/40'
                      : 'text-slate-400 hover:text-slate-200'
                  }`}
                >
                  {m.toUpperCase()}
                </button>
              ))}
            </div>
          </div>
        </header>

        {/* CONTENT BODY */}
        <main className="p-6 space-y-6 max-w-[1600px] w-full mx-auto">
          {/* BENTO KPI ROW */}
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
            <KpiCard
              title="活跃工单 / 历史总工单"
              value={`${openTicketCount} / ${tickets.length}`}
              subtitle="多态 Worker 并发池 (保留 1 个人类唤醒槽)"
              icon={<Layers className="w-4 h-4 text-indigo-400" />}
              accent="indigo"
            />
            <KpiCard
              title="奈克斯独立 GitHub 身份"
              value={`@${identityStatus?.github?.agent_login || overview?.config?.github?.bot_login || 'lkc12138-cloud'}`}
              subtitle={`宿主机本机 CLI: @${identityStatus?.github?.host_login || 'xdlkc'} (完全隔离)`}
              icon={<Shield className="w-4 h-4 text-emerald-400" />}
              accent="emerald"
            />
            <KpiCard
              title="Tier-1 Jev 分诊引擎"
              value={overview?.config?.jev ? 'TypeSafe 远端模型' : '本地启发式规则'}
              subtitle={`Model: ${overview?.config?.jev?.model || 'heuristic-jev-v1'}`}
              icon={<Cpu className="w-4 h-4 text-amber-400" />}
              accent="amber"
            />
            <KpiCard
              title="监听仓库 & 飞书群"
              value={`${overview?.config?.github?.repos?.length || 0} Repos · ${overview?.config?.feishu?.chat_ids?.length || 0} Chats`}
              subtitle={(overview?.config?.github?.repos || []).join(' · ')}
              icon={<GitPullRequest className="w-4 h-4 text-cyan-400" />}
              accent="cyan"
            />
          </div>

          {/* TAB 1: TICKETS & POLYMORPHIC WORKER ASSIGNMENTS */}
          {activeTab === 'tickets' && (
            <div className="space-y-4">
              {/* Filter Bar */}
              <div className="flex flex-wrap items-center justify-between gap-3 bg-[#0c1018] border border-slate-800/80 rounded-2xl p-4">
                <div className="flex items-center gap-2 flex-1 min-w-[260px]">
                  <div className="relative flex-1 max-w-md">
                    <Search className="w-4 h-4 text-slate-500 absolute left-3 top-1/2 -translate-y-1/2" />
                    <input
                      type="text"
                      value={ticketSearch}
                      onChange={(e) => setTicketSearch(e.target.value)}
                      placeholder="搜索工单 ID、标题、GitHub Issue # 或 PR..."
                      className="w-full pl-9 pr-3 py-1.5 rounded-xl bg-[#070a10] border border-slate-800 text-xs text-slate-200 focus:border-indigo-500/60 focus:outline-none"
                    />
                  </div>

                  <div className="flex items-center gap-1.5 bg-[#070a10] p-1 rounded-xl border border-slate-800">
                    <Filter className="w-3.5 h-3.5 text-slate-500 ml-2" />
                    {['all', 'xdlkc/nexus', 'xdlkc/clipboard-history'].map((r) => (
                      <button
                        key={r}
                        type="button"
                        onClick={() => setRepoFilter(r)}
                        className={`px-2.5 py-1 rounded-lg text-xs font-medium transition cursor-pointer ${
                          repoFilter === r
                            ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/30'
                            : 'text-slate-400 hover:text-slate-200'
                        }`}
                      >
                        {r === 'all' ? '全部仓库' : r}
                      </button>
                    ))}
                  </div>
                </div>

                <button
                  type="button"
                  onClick={loadOverview}
                  className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-xl border border-slate-800 bg-slate-900/80 hover:bg-slate-800 text-xs text-slate-300 transition cursor-pointer"
                >
                  <RefreshCw className="w-3.5 h-3.5" /> 刷新列表
                </button>
              </div>

              {/* Tickets Table */}
              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] overflow-hidden shadow-xl">
                <div className="overflow-x-auto">
                  <table className="w-full text-left border-collapse">
                    <thead>
                      <tr className="border-b border-slate-800/90 bg-[#0e131f]/70 text-[11px] font-semibold uppercase tracking-wider text-slate-400">
                        <th className="py-3.5 px-4">工单 ID / 时间</th>
                        <th className="py-3.5 px-4">任务标题 &amp; 绑定仓库 / PR</th>
                        <th className="py-3.5 px-4">执行状态</th>
                        <th className="py-3.5 px-4">多态引擎 (Harness × Model)</th>
                        <th className="py-3.5 px-4">最新 Worker 汇报总结</th>
                        <th className="py-3.5 px-4 text-right">操作</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-slate-800/60 text-xs">
                      {filteredTickets.map((t) => {
                        const lastAsg = t.assignments?.[t.assignments.length - 1];
                        const target = lastAsg?.target || {
                          harness: 'codex',
                          provider: 'openai',
                          model: 'gpt-6-astra',
                          think_effort: 'medium',
                        };
                        const report = lastAsg?.latest_report;
                        const prFollow = t.follows?.find((f) => f.thread_key.startsWith('github:pr:'));
                        const prUrl =
                          report?.pull_request_url ||
                          (prFollow
                            ? `https://github.com/${prFollow.thread_key
                                .replace('github:pr:', '')
                                .replace('#', '/pull/')}`
                            : null);

                        return (
                          <tr
                            key={t.id}
                            className="hover:bg-slate-800/25 transition-colors group"
                          >
                            <td className="py-3.5 px-4 align-top whitespace-nowrap">
                              <div className="font-mono font-semibold text-indigo-300">{t.id}</div>
                              <div className="text-[11px] text-slate-500 font-mono mt-0.5">
                                {t.short_name}
                              </div>
                              <div className="text-[10px] text-slate-500 mt-0.5">
                                {formatShortTime(t.created_at)}
                              </div>
                            </td>

                            <td className="py-3.5 px-4 align-top max-w-md">
                              <div className="font-medium text-slate-100 leading-snug break-words">
                                {t.title}
                              </div>
                              <div className="flex flex-wrap items-center gap-1.5 mt-2">
                                <span className="inline-flex items-center gap-1 text-[11px] font-mono px-2 py-0.5 rounded-md bg-indigo-500/10 text-indigo-300 border border-indigo-500/25">
                                  <GitBranch className="w-3 h-3" />
                                  {t.repo || 'xdlkc/nexus'}
                                </span>

                                {prUrl && (
                                  <a
                                    href={prUrl}
                                    target="_blank"
                                    rel="noreferrer"
                                    className="inline-flex items-center gap-1 text-[11px] font-mono px-2 py-0.5 rounded-md bg-emerald-500/15 text-emerald-300 border border-emerald-500/30 hover:bg-emerald-500/25 transition"
                                  >
                                    <GitPullRequest className="w-3 h-3" />
                                    {prFollow
                                      ? prFollow.thread_key.replace('github:pr:', '')
                                      : '查看 PR'}
                                    <ExternalLink className="w-2.5 h-2.5" />
                                  </a>
                                )}
                              </div>
                            </td>

                            <td className="py-3.5 px-4 align-top whitespace-nowrap">
                              {t.status === 'closed' ? (
                                <span className="inline-flex items-center gap-1 text-[11px] px-2.5 py-0.5 rounded-full bg-slate-800 text-slate-300 border border-slate-700">
                                  已关闭 ({t.resolution || 'completed'})
                                </span>
                              ) : report?.status === 'ready' ? (
                                <span className="inline-flex items-center gap-1 text-[11px] px-2.5 py-0.5 rounded-full bg-emerald-500/15 text-emerald-300 border border-emerald-500/30">
                                  <CheckCircle2 className="w-3 h-3" /> 已就绪 (Ready)
                                </span>
                              ) : (
                                <span className="inline-flex items-center gap-1.5 text-[11px] px-2.5 py-0.5 rounded-full bg-amber-500/15 text-amber-300 border border-amber-500/30">
                                  <span className="w-1.5 h-1.5 rounded-full bg-amber-400 animate-ping" />
                                  {t.phase || '执行中 (Working)'}
                                </span>
                              )}
                            </td>

                            <td className="py-3.5 px-4 align-top whitespace-nowrap">
                              <div className="inline-flex items-center gap-1.5 font-mono text-[11px] px-2.5 py-1 rounded-lg bg-[#070a10] border border-slate-800 text-slate-200">
                                <Bot className="w-3.5 h-3.5 text-indigo-400" />
                                <span>{target.harness}</span>
                                <span className="text-slate-600">/</span>
                                <span className="text-cyan-300">{target.model}</span>
                                <span className="px-1.5 py-0.2 rounded bg-indigo-500/20 text-indigo-300 text-[10px]">
                                  {target.think_effort}
                                </span>
                              </div>
                            </td>

                            <td className="py-3.5 px-4 align-top max-w-md">
                              <p className="text-xs text-slate-300 leading-relaxed line-clamp-3 whitespace-pre-wrap">
                                {report?.summary || '正在启动 Worker 执行任务...'}
                              </p>
                            </td>

                            <td className="py-3.5 px-4 align-top text-right whitespace-nowrap">
                              <div className="inline-flex items-center gap-1.5">
                                <button
                                  type="button"
                                  onClick={() => openTrajectoryDrawer(t.id)}
                                  className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-indigo-500/15 hover:bg-indigo-500/25 text-indigo-300 border border-indigo-500/30 text-xs font-medium transition cursor-pointer"
                                >
                                  <Terminal className="w-3.5 h-3.5" />
                                  内部轨迹
                                </button>
                                {t.status === 'open' && (
                                  <>
                                    <button
                                      type="button"
                                      onClick={() => {
                                        setSwitchModalTicket(t);
                                        setSwitchHarness(target.harness as HarnessKind);
                                        setSwitchProvider(target.provider);
                                        setSwitchModel(target.model);
                                        setSwitchThink(target.think_effort as ThinkEffort);
                                      }}
                                      className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-200 text-xs transition cursor-pointer"
                                    >
                                      热切换
                                    </button>
                                    <button
                                      type="button"
                                      onClick={async () => {
                                        await stopTicket(t.id);
                                        notify(`已终止工单 ${t.id}`);
                                        loadOverview();
                                      }}
                                      className="inline-flex items-center gap-1 px-2 py-1.5 rounded-lg bg-rose-500/10 hover:bg-rose-500/20 text-rose-300 border border-rose-500/25 text-xs transition cursor-pointer"
                                      title="终止工单"
                                    >
                                      <Square className="w-3 h-3" />
                                    </button>
                                  </>
                                )}
                              </div>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </div>
            </div>
          )}

          {/* TAB 2: FULL-SCREEN WORKER TRAJECTORY STUDIO */}
          {activeTab === 'trajectory' && (
            <div className="space-y-4">
              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] p-5">
                <div className="flex flex-wrap items-center justify-between gap-4 mb-4">
                  <div>
                    <h2 className="text-base font-semibold text-white flex items-center gap-2">
                      <Terminal className="w-4 h-4 text-emerald-400" />
                      Worker 内部执行轨迹分析器 (Step-by-Step Internal Trajectory)
                    </h2>
                    <p className="text-xs text-slate-400 mt-0.5">
                      完整透视每个工单 Worker 内部的启动 Prompt、推理思考链 (`Reasoning`)、Shell 工具调用、标准输出与 Token 消耗。
                    </p>
                  </div>

                  <div className="flex flex-wrap items-center gap-2">
                    <select
                      value={selectedTraceTicketId}
                      onChange={(e) => setSelectedTraceTicketId(e.target.value)}
                      className="px-3 py-2 rounded-xl bg-[#070a10] border border-slate-800 text-xs text-slate-100 min-w-[300px]"
                    >
                      {tickets.map((t) => (
                        <option key={t.id} value={t.id}>
                          {t.id} · [{t.repo || 'nexus'}] {t.title.slice(0, 48)}
                        </option>
                      ))}
                    </select>

                    <button
                      type="button"
                      onClick={() =>
                        selectedTraceTicketId &&
                        getWorkerTrace(selectedTraceTicketId).then(setFullTrace)
                      }
                      className="inline-flex items-center gap-1.5 px-3.5 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 text-xs font-medium text-white transition cursor-pointer"
                    >
                      <RefreshCw className="w-3.5 h-3.5" /> 刷新轨迹
                    </button>
                  </div>
                </div>

                {/* Step Kind Filter Pills */}
                <div className="flex flex-wrap items-center justify-between gap-2 pb-4 mb-4 border-b border-slate-800/80">
                  <div className="flex flex-wrap items-center gap-1.5">
                    {[
                      { id: 'all', label: '全部步骤' },
                      { id: 'thinking', label: '💭 思考推理 (Thinking)' },
                      { id: 'tool_call', label: '🔧 命令/工具调用 (Exec)' },
                      { id: 'tool_result', label: '📤 命令输出 (Stdout)' },
                      { id: 'usage', label: '📊 Token 统计' },
                      { id: 'finish', label: '✅ 完成汇报' },
                    ].map((f) => (
                      <button
                        key={f.id}
                        type="button"
                        onClick={() => setTraceKindFilter(f.id)}
                        className={`px-3 py-1 rounded-lg text-xs font-medium transition cursor-pointer ${
                          traceKindFilter === f.id
                            ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/40'
                            : 'bg-[#070a10] text-slate-400 hover:text-slate-200 border border-slate-800'
                        }`}
                      >
                        {f.label}
                      </button>
                    ))}
                  </div>

                  <span className="text-xs font-mono text-slate-400">
                    共 {(fullTrace?.steps || []).length} 个内部步骤
                  </span>
                </div>

                <div className="space-y-1">
                  {(fullTrace?.steps || [])
                    .filter((s) => traceKindFilter === 'all' || s.kind === traceKindFilter)
                    .map((step) => (
                      <StepTimelineItem key={`${step.assignment_id}-${step.seq}`} step={step} />
                    ))}
                </div>
              </div>
            </div>
          )}

          {/* TAB 3: CONVERSATION & OUTBOX STREAM */}
          {activeTab === 'conversation' && (
            <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] p-5 space-y-3">
              <h2 className="text-base font-semibold text-white mb-2">
                群聊上下文与机器人实时回复流 (Chronological Stream)
              </h2>
              <div className="space-y-3">
                {chatStream.map((item) => (
                  <div
                    key={item.id}
                    className={`rounded-xl border p-3.5 ${
                      item.role === 'user'
                        ? 'border-slate-800 bg-[#080b12]'
                        : item.role === 'thinking'
                        ? 'border-amber-500/25 bg-amber-500/5'
                        : 'border-indigo-500/30 bg-indigo-500/5'
                    }`}
                  >
                    <div className="flex items-center justify-between text-xs text-slate-400 mb-1.5">
                      <span className="font-semibold text-slate-200">{item.title}</span>
                      <span className="font-mono text-[11px]">{formatShortTime(item.at)}</span>
                    </div>
                    <p className="text-xs text-slate-200 whitespace-pre-wrap leading-relaxed">
                      {item.text}
                    </p>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* TAB 4: DECISIONS & JEV PLAYGROUND */}
          {activeTab === 'decisions' && (
            <div className="space-y-5">
              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] p-5">
                <h3 className="text-sm font-semibold text-white mb-3">
                  在线测试 Tier-1 Jev 分诊器 (测试唤醒、防注入、任务分类与复杂度评估)
                </h3>
                <div className="flex gap-2">
                  <input
                    type="text"
                    value={jevTestInput}
                    onChange={(e) => setJevTestInput(e.target.value)}
                    className="flex-1 px-3.5 py-2 rounded-xl bg-[#070a10] border border-slate-800 text-xs text-slate-100"
                  />
                  <button
                    type="button"
                    onClick={async () => {
                      const res = await testJevTriage(jevTestInput);
                      setJevTestOutput(JSON.stringify(res, null, 2));
                    }}
                    className="px-4 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 text-xs font-semibold text-white cursor-pointer"
                  >
                    运行 Jev 分诊
                  </button>
                </div>
                {jevTestOutput && (
                  <pre className="mt-3 p-3.5 rounded-xl bg-[#070a10] border border-slate-800 text-[11px] font-mono text-emerald-300 overflow-x-auto">
                    {jevTestOutput}
                  </pre>
                )}
              </div>

              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] overflow-hidden">
                <table className="w-full text-left border-collapse text-xs">
                  <thead>
                    <tr className="border-b border-slate-800 bg-[#0e131f]/70 text-[11px] text-slate-400 uppercase">
                      <th className="py-3 px-4">决策 ID</th>
                      <th className="py-3 px-4">时间</th>
                      <th className="py-3 px-4">动作 (Outcome)</th>
                      <th className="py-3 px-4">关联工单</th>
                      <th className="py-3 px-4">目标引擎</th>
                      <th className="py-3 px-4">路由原因</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-slate-800/60">
                    {decisionList.map((d) => (
                      <tr key={d.id} className="hover:bg-slate-800/20">
                        <td className="py-3 px-4 font-mono text-indigo-300">{d.id}</td>
                        <td className="py-3 px-4 font-mono text-slate-400">{formatShortTime(d.at)}</td>
                        <td className="py-3 px-4">
                          <span className="px-2 py-0.5 rounded-full bg-indigo-500/15 text-indigo-300 border border-indigo-500/30 text-[11px]">
                            {d.outcome}
                          </span>
                        </td>
                        <td className="py-3 px-4 font-mono text-emerald-300">{d.ticket_id || '-'}</td>
                        <td className="py-3 px-4 font-mono text-slate-300">
                          {d.target
                            ? `${d.target.harness}:${d.target.model}:${d.target.think_effort}`
                            : '-'}
                        </td>
                        <td className="py-3 px-4 text-slate-300">{d.reason}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}

          {/* TAB 5: PERSISTENT MEMORY */}
          {activeTab === 'memory' && (
            <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] p-5 space-y-4">
              <div className="flex items-center justify-between">
                <div>
                  <h2 className="text-base font-semibold text-white">
                    持久化长期记忆库 (`.nexus/MEMORY.md`)
                  </h2>
                  <p className="text-xs text-slate-400 mt-0.5">
                    每次启动 Worker 时自动挂载此 Markdown 与近期群聊上下文，讨论中敲定的结论或 Idea 会由奈克斯自动追加。
                  </p>
                </div>
                <button
                  type="button"
                  onClick={async () => {
                    await saveMemoryContent(memoryDraft);
                    setMemoryDirty(false);
                    notify('MEMORY.md 已保存！');
                  }}
                  className="px-4 py-2 rounded-xl bg-indigo-600 hover:bg-indigo-500 text-xs font-semibold text-white cursor-pointer"
                >
                  保存 MEMORY.md
                </button>
              </div>
              <textarea
                rows={18}
                value={memoryDraft}
                onChange={(e) => {
                  setMemoryDraft(e.target.value);
                  setMemoryDirty(true);
                }}
                className="w-full p-4 rounded-xl bg-[#070a10] border border-slate-800 font-mono text-xs text-slate-200 focus:border-indigo-500/60 focus:outline-none leading-relaxed"
              />
            </div>
          )}

          {/* TAB 6: SETTINGS & AGENT IDENTITY */}
          {activeTab === 'settings' && (
            <div className="space-y-6">
              <IdentityMatrix
                status={identityStatus}
                loading={identityLoading}
                onRefresh={loadIdentity}
              />

              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] p-6 space-y-6">
                <div className="flex items-center justify-between border-b border-slate-800 pb-4">
                  <div>
                    <h2 className="text-base font-semibold text-white">
                      奈克斯独立身份、仓库绑定与双引擎路由配置
                    </h2>
                    <p className="text-xs text-slate-400 mt-0.5">
                      修改后点击右侧按钮即刻热更新 `.nexus/config.json` 与 `.nexus/worker-bin/*` 隔离垫片。
                    </p>
                  </div>
                  <button
                    type="button"
                    onClick={handleSaveConfig}
                    className="px-5 py-2.5 rounded-xl bg-indigo-600 hover:bg-indigo-500 text-xs font-semibold text-white shadow-lg shadow-indigo-600/25 cursor-pointer"
                  >
                    保存并热更新配置
                  </button>
                </div>

                {/* Section 1: Isolated Agent Identity */}
                <div>
                  <h3 className="text-xs font-semibold uppercase tracking-wider text-indigo-400 mb-3">
                    1. 奈克斯独立身份与 CLI 工作空间隔离 (`agent_identity`)
                  </h3>
                  <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
                    <FormField label="Git 提交用户名 (`git_user_name`)">
                      <input
                        type="text"
                        value={gitUserName}
                        onChange={(e) => {
                          setGitUserName(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="Git 提交邮箱 (`git_user_email`)">
                      <input
                        type="text"
                        value={gitUserEmail}
                        onChange={(e) => {
                          setGitUserEmail(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="奈克斯独立 GitHub 账号 (`github.bot_login`)">
                      <input
                        type="text"
                        value={githubBotLogin}
                        onChange={(e) => {
                          setGithubBotLogin(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="飞书独立 CLI Profile (`feishu.profile`)">
                      <input
                        type="text"
                        value={feishuProfile}
                        onChange={(e) => {
                          setFeishuProfile(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                  </div>
                </div>

                {/* Section 2: Repositories & Feishu Wake */}
                <div>
                  <h3 className="text-xs font-semibold uppercase tracking-wider text-cyan-400 mb-3">
                    2. 仓库绑定与飞书群唤醒配置
                  </h3>
                  <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
                    <FormField label="绑定 GitHub 仓库 (`github.repos`，逗号分隔)">
                      <input
                        type="text"
                        value={githubRepos}
                        onChange={(e) => {
                          setGithubRepos(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="默认迭代仓库 (`default_repo`)">
                      <input
                        type="text"
                        value={defaultRepo}
                        onChange={(e) => {
                          setDefaultRepo(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="免 @ 唤醒词 (`wake_keywords`)">
                      <input
                        type="text"
                        value={wakeKeywords}
                        onChange={(e) => {
                          setWakeKeywords(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                    <FormField label="接单消息表情 (`ack_reaction`)">
                      <input
                        type="text"
                        value={ackReaction}
                        onChange={(e) => {
                          setAckReaction(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      />
                    </FormField>
                  </div>
                </div>

                {/* Section 3: Default Execution Engine & Jev */}
                <div>
                  <h3 className="text-xs font-semibold uppercase tracking-wider text-amber-400 mb-3">
                    3. 默认 Worker 引擎与 Tier-1 Jev / Tier-2 Router 分诊配置
                  </h3>
                  {(() => {
                    const { providers, models, thinkEfforts } = getEngineCatalogOptions(
                      defHarness,
                      defProvider,
                      defModel,
                      defThink,
                      harnessCatalogs
                    );
                    return (
                      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4 mb-4">
                        <FormField label="默认 Harness 驱动器">
                          <select
                            value={defHarness}
                            onChange={(e) => {
                              const nextH = e.target.value as HarnessKind;
                              setDefHarness(nextH);
                              const nextOpts = getEngineCatalogOptions(
                                nextH,
                                defProvider,
                                defModel,
                                defThink,
                                harnessCatalogs,
                                true
                              );
                              setDefProvider(nextOpts.recommendedProvider);
                              setDefModel(nextOpts.recommendedModel);
                              setDefThink(nextOpts.recommendedThink);
                              setCfgDirty(true);
                            }}
                            className="form-input"
                          >
                            <option value="codex">codex (OpenAI Codex CLI)</option>
                            <option value="claude-code">claude-code (Claude Code CLI)</option>
                            <option value="pi-native">pi-native (内置原生 ReAct)</option>
                            <option value="pi-rpc">pi-rpc (Pi Agent RPC)</option>
                            <option value="opencode">opencode (OpenCode CLI)</option>
                          </select>
                        </FormField>

                        <FormField label={`默认 Provider (跟随 ${defHarness})`}>
                          <select
                            value={defProvider}
                            onChange={(e) => {
                              const nextP = e.target.value;
                              setDefProvider(nextP);
                              const nextOpts = getEngineCatalogOptions(
                                defHarness,
                                nextP,
                                defModel,
                                defThink,
                                harnessCatalogs,
                                false,
                                true
                              );
                              setDefModel(nextOpts.recommendedModel);
                              setCfgDirty(true);
                            }}
                            className="form-input"
                          >
                            {providers.map((p) => (
                              <option key={p} value={p}>
                                {p}
                              </option>
                            ))}
                          </select>
                        </FormField>

                        <FormField label={`默认 Model (跟随 ${defHarness}/${defProvider})`}>
                          <select
                            value={defModel}
                            onChange={(e) => {
                              setDefModel(e.target.value);
                              setCfgDirty(true);
                            }}
                            className="form-input"
                          >
                            {models.map((m) => (
                              <option key={m} value={m}>
                                {m}
                              </option>
                            ))}
                          </select>
                        </FormField>

                        <FormField label={`默认思考程度 (跟随 ${defHarness})`}>
                          <select
                            value={defThink}
                            onChange={(e) => {
                              setDefThink(e.target.value as ThinkEffort);
                              setCfgDirty(true);
                            }}
                            className="form-input"
                          >
                            {thinkEfforts.map((te) => (
                              <option key={te} value={te}>
                                {formatThinkLabel(defHarness, te)}
                              </option>
                            ))}
                          </select>
                        </FormField>
                      </div>
                    );
                  })()}

                  <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4 mb-4">
                    <FormField label="Jev 运行协议 (`jev.protocol`)">
                      <select
                        value={jevMode}
                        onChange={(e) => {
                          setJevMode(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      >
                        <option value="heuristic">本地启发式规则分诊 (零延迟)</option>
                        <option value="typesafe-system-one">远端 TypeSafe System-One</option>
                        <option value="openrouter-system-one">远端 OpenRouter System-One</option>
                        <option value="openai-decisions">远端 OpenAI Decisions</option>
                      </select>
                    </FormField>

                    <FormField label="Jev 接口地址 (`jev.base_url`)">
                      <input
                        type="text"
                        value={jevBaseUrl}
                        onChange={(e) => {
                          setJevBaseUrl(e.target.value);
                          setCfgDirty(true);
                        }}
                        placeholder="https://api.typesafe.ai/v1"
                        className="form-input"
                      />
                    </FormField>

                    <FormField label="Jev 模型 (`jev.model`)">
                      <select
                        value={jevModel}
                        onChange={(e) => {
                          setJevModel(e.target.value);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      >
                        <option value="jev-1">jev-1 (TypeSafe 官方分诊模型)</option>
                        <option value="typesafe/jev-1">typesafe/jev-1 (OpenRouter)</option>
                        <option value="gpt-4o-mini">gpt-4o-mini (OpenAI Decisions)</option>
                        <option value="heuristic-jev-v1">heuristic-jev-v1 (本地规则)</option>
                      </select>
                    </FormField>

                    <FormField label="Tier-2 Router 思考程度 (`router_llm.think_effort`)">
                      <select
                        value={routerThink}
                        onChange={(e) => {
                          setRouterThink(e.target.value as ThinkEffort);
                          setCfgDirty(true);
                        }}
                        className="form-input"
                      >
                        <option value="low">low</option>
                        <option value="medium">medium</option>
                        <option value="high">high</option>
                      </select>
                    </FormField>
                  </div>
                </div>
              </div>
            </div>
          )}

          {/* TAB 7: TOOLCHAIN & IDENTITY MATRIX */}
          {activeTab === 'toolchain' && (
            <div className="space-y-5">
              <IdentityMatrix
                status={identityStatus}
                loading={identityLoading}
                onRefresh={loadIdentity}
              />

              <div className="rounded-2xl border border-slate-800/90 bg-[#0b0f17] overflow-hidden">
                <div className="p-4 border-b border-slate-800 font-semibold text-sm text-white">
                  宿主机 CLI 工具链安装探针
                </div>
                <table className="w-full text-left border-collapse text-xs">
                  <thead>
                    <tr className="border-b border-slate-800 bg-[#0e131f]/70 text-[11px] text-slate-400 uppercase">
                      <th className="py-3 px-4">CLI 二进制</th>
                      <th className="py-3 px-4">状态</th>
                      <th className="py-3 px-4">探测版本详情</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-slate-800/60">
                    {(overview?.toolchain || []).map((tl) => (
                      <tr key={tl.binary}>
                        <td className="py-3 px-4 font-mono text-indigo-300">{tl.binary}</td>
                        <td className="py-3 px-4">
                          {tl.installed ? (
                            <span className="px-2 py-0.5 rounded-full bg-emerald-500/15 text-emerald-300 border border-emerald-500/30 text-[11px]">
                              已就绪
                            </span>
                          ) : (
                            <span className="px-2 py-0.5 rounded-full bg-amber-500/15 text-amber-300 border border-amber-500/30 text-[11px]">
                              未安装
                            </span>
                          )}
                        </td>
                        <td className="py-3 px-4 font-mono text-slate-300">{tl.version_or_note}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </main>
      </div>

      {/* SLIDE-OVER TRAJECTORY DRAWER */}
      {drawerTicketId && (
        <div className="fixed inset-0 z-40 flex justify-end bg-black/60 backdrop-blur-sm">
          <div className="w-full max-w-3xl bg-[#090d15] border-l border-slate-800 h-full flex flex-col shadow-2xl">
            <div className="p-4 border-b border-slate-800 flex items-center justify-between">
              <div>
                <div className="flex items-center gap-2">
                  <span className="font-mono text-xs font-bold px-2 py-0.5 rounded bg-indigo-500/20 text-indigo-300">
                    {drawerTicketId}
                  </span>
                  <h3 className="text-sm font-semibold text-white truncate max-w-lg">
                    {drawerTrace?.title || 'Worker 内部执行轨迹'}
                  </h3>
                </div>
                <p className="text-[11px] text-slate-400 mt-0.5">
                  共 {drawerTrace?.steps?.length || 0} 个执行步骤 · 实时自动同步中
                </p>
              </div>
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => {
                    setSelectedTraceTicketId(drawerTicketId);
                    setDrawerTicketId(null);
                    setActiveTab('trajectory');
                  }}
                  className="px-2.5 py-1 rounded-lg bg-slate-800 hover:bg-slate-700 text-xs text-slate-200 cursor-pointer"
                >
                  全屏打开
                </button>
                <button
                  type="button"
                  onClick={() => setDrawerTicketId(null)}
                  className="p-1.5 rounded-lg hover:bg-slate-800 text-slate-400 hover:text-white cursor-pointer"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>
            </div>

            <div className="flex-1 overflow-y-auto p-5">
              {drawerLoading ? (
                <div className="text-xs text-slate-400 py-8 text-center">正在加载内部轨迹...</div>
              ) : (drawerTrace?.steps || []).length === 0 ? (
                <div className="text-xs text-slate-400 py-8 text-center">暂无轨迹步骤</div>
              ) : (
                (drawerTrace?.steps || []).map((step) => (
                  <StepTimelineItem key={`${step.assignment_id}-${step.seq}`} step={step} />
                ))
              )}
            </div>
          </div>
        </div>
      )}

      {/* MID-FLIGHT SWITCH TARGET MODAL */}
      {switchModalTicket && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/65 backdrop-blur-sm p-4">
          <div className="w-full max-w-md rounded-2xl border border-slate-800 bg-[#0c111d] p-5 space-y-4 shadow-2xl">
            <div className="flex items-center justify-between">
              <h3 className="text-sm font-semibold text-white">
                热切换工单执行引擎 ({switchModalTicket.id})
              </h3>
              <button
                onClick={() => setSwitchModalTicket(null)}
                className="text-slate-400 hover:text-white"
              >
                <X className="w-4 h-4" />
              </button>
            </div>

            {(() => {
              const { providers, models, thinkEfforts } = getEngineCatalogOptions(
                switchHarness,
                switchProvider,
                switchModel,
                switchThink,
                harnessCatalogs
              );
              return (
                <div className="space-y-3 text-xs">
                  <FormField label="目标 Harness">
                    <select
                      value={switchHarness}
                      onChange={(e) => {
                        const nextH = e.target.value as HarnessKind;
                        setSwitchHarness(nextH);
                        const nextOpts = getEngineCatalogOptions(
                          nextH,
                          switchProvider,
                          switchModel,
                          switchThink,
                          harnessCatalogs,
                          true
                        );
                        setSwitchProvider(nextOpts.recommendedProvider);
                        setSwitchModel(nextOpts.recommendedModel);
                        setSwitchThink(nextOpts.recommendedThink);
                      }}
                      className="form-input"
                    >
                      <option value="codex">codex (OpenAI Codex CLI)</option>
                      <option value="claude-code">claude-code (Claude Code CLI)</option>
                      <option value="pi-native">pi-native (内置原生 ReAct)</option>
                      <option value="pi-rpc">pi-rpc (Pi Agent RPC)</option>
                      <option value="opencode">opencode (OpenCode CLI)</option>
                    </select>
                  </FormField>
                  <FormField label={`Provider (跟随 ${switchHarness})`}>
                    <select
                      value={switchProvider}
                      onChange={(e) => {
                        const nextP = e.target.value;
                        setSwitchProvider(nextP);
                        const nextOpts = getEngineCatalogOptions(
                          switchHarness,
                          nextP,
                          switchModel,
                          switchThink,
                          harnessCatalogs,
                          false,
                          true
                        );
                        setSwitchModel(nextOpts.recommendedModel);
                      }}
                      className="form-input"
                    >
                      {providers.map((p) => (
                        <option key={p} value={p}>
                          {p}
                        </option>
                      ))}
                    </select>
                  </FormField>
                  <FormField label={`Model (跟随 ${switchHarness}/${switchProvider})`}>
                    <select
                      value={switchModel}
                      onChange={(e) => setSwitchModel(e.target.value)}
                      className="form-input"
                    >
                      {models.map((m) => (
                        <option key={m} value={m}>
                          {m}
                        </option>
                      ))}
                    </select>
                  </FormField>
                  <FormField label={`思考程度 (跟随 ${switchHarness})`}>
                    <select
                      value={switchThink}
                      onChange={(e) => setSwitchThink(e.target.value as ThinkEffort)}
                      className="form-input"
                    >
                      {thinkEfforts.map((te) => (
                        <option key={te} value={te}>
                          {formatThinkLabel(switchHarness, te)}
                        </option>
                      ))}
                    </select>
                  </FormField>
                  <FormField label="切换原因 (写入 Handoff 卡片)">
                    <input
                      type="text"
                      value={switchReason}
                      onChange={(e) => setSwitchReason(e.target.value)}
                      className="form-input"
                    />
                  </FormField>
                </div>
              );
            })()}

            <div className="flex justify-end gap-2 pt-2">
              <button
                type="button"
                onClick={() => setSwitchModalTicket(null)}
                className="px-3.5 py-1.5 rounded-xl bg-slate-800 text-xs text-slate-300"
              >
                取消
              </button>
              <button
                type="button"
                onClick={async () => {
                  await switchWorkerTarget({
                    ticket_id: switchModalTicket.id,
                    harness: switchHarness,
                    provider: switchProvider,
                    model: switchModel,
                    think_effort: switchThink,
                    reason: switchReason,
                  });
                  setSwitchModalTicket(null);
                  notify(`已热切换 ${switchModalTicket.id} 执行引擎！`);
                  loadOverview();
                }}
                className="px-4 py-1.5 rounded-xl bg-indigo-600 hover:bg-indigo-500 text-xs font-semibold text-white"
              >
                确认热切换
              </button>
            </div>
          </div>
        </div>
      )}

      {/* TOAST */}
      {toast && (
        <div className="fixed bottom-5 right-5 z-50 px-4 py-2.5 rounded-xl bg-indigo-600 text-white text-xs font-medium shadow-xl border border-indigo-400/30">
          {toast}
        </div>
      )}
    </div>
  );
}

const SidebarButton: React.FC<{
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  badge?: string;
  badgeColor?: 'indigo' | 'emerald';
}> = ({ active, onClick, icon, label, badge, badgeColor = 'indigo' }) => (
  <button
    type="button"
    onClick={onClick}
    className={`w-full flex items-center justify-between px-3 py-2.5 rounded-xl text-xs font-medium transition cursor-pointer ${
      active
        ? 'bg-indigo-600/15 text-indigo-300 border border-indigo-500/30 shadow-sm'
        : 'text-slate-400 hover:text-slate-200 hover:bg-slate-800/40 border border-transparent'
    }`}
  >
    <span className="flex items-center gap-2.5">
      {icon}
      <span>{label}</span>
    </span>
    {badge && (
      <span
        className={`text-[10px] font-mono px-1.5 py-0.5 rounded-md ${
          badgeColor === 'emerald'
            ? 'bg-emerald-500/15 text-emerald-300 border border-emerald-500/30'
            : 'bg-slate-800 text-slate-300'
        }`}
      >
        {badge}
      </span>
    )}
  </button>
);

const KpiCard: React.FC<{
  title: string;
  value: string;
  subtitle: string;
  icon: React.ReactNode;
  accent: 'indigo' | 'emerald' | 'amber' | 'cyan';
}> = ({ title, value, subtitle, icon }) => (
  <div className="rounded-2xl border border-slate-800/80 bg-gradient-to-b from-[#0d121d] to-[#090d15] p-4 shadow-md">
    <div className="flex items-center justify-between text-xs text-slate-400 mb-1.5">
      <span>{title}</span>
      <div className="p-1.5 rounded-lg bg-slate-800/80 border border-slate-700/60">{icon}</div>
    </div>
    <div className="text-lg font-bold text-white tracking-tight truncate">{value}</div>
    <div className="text-[11px] text-slate-500 mt-1 truncate">{subtitle}</div>
  </div>
);

const FormField: React.FC<{ label: string; children: React.ReactNode }> = ({
  label,
  children,
}) => (
  <div className="space-y-1">
    <label className="block text-[11px] font-medium text-slate-400">{label}</label>
    <div className="[&_.form-input]:w-full [&_.form-input]:px-3 [&_.form-input]:py-2 [&_.form-input]:rounded-xl [&_.form-input]:bg-[#070a10] [&_.form-input]:border [&_.form-input]:border-slate-800 [&_.form-input]:text-xs [&_.form-input]:text-slate-100 [&_.form-input:focus]:border-indigo-500/60 [&_.form-input:focus]:outline-none">
      {children}
    </div>
  </div>
);

const FALLBACK_HARNESS_CATALOGS: Record<string, HarnessCatalog> = {
  codex: {
    default_provider: 'openai',
    default_model: 'gpt-6-astra',
    default_think_effort: 'high',
    think_efforts: ['minimal', 'low', 'medium', 'high', 'xhigh'],
    providers: {
      openai: ['gpt-6-astra', 'gpt-6.1-sol', 'gpt-5.6-sol', 'gpt-5.3-codex', 'o3-pro'],
    },
  },
  'claude-code': {
    default_provider: 'hub',
    default_model: 'zenmux/anthropic/claude-sonnet-5.5',
    default_think_effort: 'high',
    think_efforts: ['low', 'medium', 'high', 'xhigh'],
    providers: {
      hub: [
        'zenmux/anthropic/claude-sonnet-5.5',
        'zenmux/anthropic/claude-opus-5.5',
        'zenmux/anthropic/claude-haiku-5.5',
        'deepseek/deepseek-v4-pro',
        'glm/glm-5.3',
        'kimi/k3.1-pro',
      ],
      anthropic: ['claude-sonnet-4-6', 'claude-opus-4-6', 'claude-haiku-4-5'],
    },
  },
  'pi-rpc': {
    default_provider: 'hub',
    default_model: 'zenmux/anthropic/claude-sonnet-5.5',
    default_think_effort: 'medium',
    think_efforts: ['off', 'minimal', 'low', 'medium', 'high', 'xhigh'],
    providers: {
      hub: ['zenmux/anthropic/claude-sonnet-5.5', 'deepseek/deepseek-v4-pro', 'glm/glm-5.3', 'kimi/k3.1-pro'],
      openai: ['gpt-6-astra', 'gpt-6.1-sol', 'gpt-5.6-sol'],
      xai: ['grok-4.7', 'grok-4.6'],
    },
  },
  'pi-native': {
    default_provider: 'anthropic',
    default_model: 'claude-sonnet-4-6',
    default_think_effort: 'medium',
    think_efforts: ['minimal', 'low', 'medium', 'high', 'xhigh'],
    providers: {
      anthropic: ['claude-sonnet-4-6', 'claude-opus-4-6', 'claude-haiku-4-5'],
      openai: ['gpt-6-astra', 'gpt-6.1-sol', 'gpt-5.6-sol'],
    },
  },
  opencode: {
    default_provider: 'anthropic',
    default_model: 'claude-sonnet-5-5',
    default_think_effort: 'medium',
    think_efforts: ['low', 'medium', 'high'],
    providers: {
      anthropic: ['claude-sonnet-5-5', 'claude-opus-5-5', 'claude-sonnet-4-6', 'claude-opus-4-6'],
      hub: ['zenmux/anthropic/claude-sonnet-5.5', 'deepseek/deepseek-v4-pro', 'glm/glm-5.3'],
      openai: ['gpt-6-astra', 'gpt-6.1-sol'],
    },
  },
};

function formatThinkLabel(harness: HarnessKind, te: ThinkEffort): string {
  if (harness === 'claude-code' && te === 'xhigh') {
    return 'xhigh (--effort max · Claude 极限推理)';
  }
  switch (te) {
    case 'off':
      return 'off (关闭思考)';
    case 'minimal':
      return 'minimal (极简推理)';
    case 'low':
      return 'low (轻量推理)';
    case 'medium':
      return 'medium (标准推理)';
    case 'high':
      return 'high (深度推理)';
    case 'xhigh':
      return 'xhigh (极限推理)';
  }
}

function getEngineCatalogOptions(
  harness: HarnessKind,
  currentProvider: string,
  currentModel: string,
  currentThink: ThinkEffort,
  harnessCatalogs: Record<string, HarnessCatalog>,
  resetHarness = false,
  resetProvider = false
): {
  providers: string[];
  models: string[];
  thinkEfforts: ThinkEffort[];
  recommendedProvider: string;
  recommendedModel: string;
  recommendedThink: ThinkEffort;
} {
  const cat = harnessCatalogs[harness] || FALLBACK_HARNESS_CATALOGS[harness] || FALLBACK_HARNESS_CATALOGS.codex;
  const providerKeys = Object.keys(cat.providers);

  const recommendedProvider = resetHarness
    ? cat.default_provider || providerKeys[0] || 'openai'
    : providerKeys.includes(currentProvider)
    ? currentProvider
    : cat.default_provider || providerKeys[0] || 'openai';

  const providers =
    !resetHarness && currentProvider && !providerKeys.includes(currentProvider)
      ? [currentProvider, ...providerKeys]
      : providerKeys;

  const activeProvider = resetHarness || resetProvider ? recommendedProvider : currentProvider || recommendedProvider;
  const providerModels = cat.providers[activeProvider] || [];

  const recommendedModel = resetHarness
    ? activeProvider === cat.default_provider && providerModels.includes(cat.default_model)
      ? cat.default_model
      : providerModels[0] || cat.default_model
    : resetProvider
    ? providerModels[0] || cat.default_model
    : providerModels.includes(currentModel)
    ? currentModel
    : providerModels[0] || cat.default_model;

  const models =
    !resetHarness && !resetProvider && currentModel && !providerModels.includes(currentModel)
      ? [currentModel, ...providerModels]
      : providerModels;

  const thinkEfforts = cat.think_efforts?.length
    ? cat.think_efforts
    : (['low', 'medium', 'high'] as ThinkEffort[]);

  const recommendedThink = resetHarness
    ? cat.default_think_effort || thinkEfforts[0] || 'medium'
    : thinkEfforts.includes(currentThink)
    ? currentThink
    : cat.default_think_effort || thinkEfforts[0] || 'medium';

  return {
    providers,
    models,
    thinkEfforts,
    recommendedProvider,
    recommendedModel,
    recommendedThink,
  };
}

function formatShortTime(iso?: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
    hour12: false,
  });
}
