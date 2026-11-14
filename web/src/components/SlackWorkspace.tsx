import React, { useEffect, useMemo, useRef, useState } from 'react';
import {
  AlertCircle,
  Bot,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Clock,
  Cpu,
  ExternalLink,
  GitBranch,
  GitPullRequest,
  Hash,
  Inbox,
  Layers,
  Menu,
  MessageSquare,
  RefreshCw,
  Search,
  Send,
  SlidersHorizontal,
  Sparkles,
  Square,
  Terminal,
  User,
  Wrench,
  X,
  Zap,
} from 'lucide-react';
import { getWorkerTrace, sendWorkspaceMessage } from '../api/client';
import type {
  HarnessCatalog,
  HarnessKind,
  NexusOverview,
  OutboxMessage,
  ThinkEffort,
  Ticket,
  Trigger,
  WorkerTraceResponse,
} from '../types/nexus';
import { StepTimelineItem } from './StepBadge';

type ChannelSelection =
  | { kind: 'inbox' }
  | { kind: 'general' }
  | { kind: 'active' }
  | { kind: 'repo'; repo: string }
  | { kind: 'source'; source: 'feishu' | 'dingtalk' | 'github' | 'web' };

type StatusFilter = 'all' | 'running' | 'ready' | 'blocked';

type DerivedTicketStatus =
  | 'needs_decision'
  | 'blocked'
  | 'ready_review'
  | 'attention'
  | 'running'
  | 'queued'
  | 'idle'
  | 'completed'
  | 'stopped'
  | 'dropped';

interface ThreadTimelineEntry {
  id: string;
  at: string;
  role: 'user' | 'bot' | 'thinking' | 'report';
  author: string;
  sourceLabel: string;
  text: string;
  question?: string | null;
  prUrl?: string | null;
  statusNote?: string;
}

interface ConversationThread {
  key: string;
  threadKey: string;
  ticket?: Ticket;
  title: string;
  repo: string;
  sourceKind: 'feishu' | 'dingtalk' | 'github' | 'web' | 'slack' | 'gws';
  sourceLabel: string;
  rootActor: string;
  rootText: string;
  createdAt: string;
  latestAt: string;
  derivedStatus: DerivedTicketStatus;
  latestReply?: string;
  latestThinking?: string;
  question?: string | null;
  prUrl?: string | null;
  prLabel?: string | null;
  target?: {
    harness: string;
    provider: string;
    model: string;
    think_effort: string;
  };
  timeline: ThreadTimelineEntry[];
  replyCount: number;
  isOpen: boolean;
}

interface SlackWorkspaceProps {
  overview: NexusOverview | null;
  harnessCatalogs: Record<string, HarnessCatalog>;
  onRefresh: () => Promise<void>;
  onOpenSwitchModal: (ticket: Ticket) => void;
  onStopTicket: (ticketId: string) => Promise<void>;
  onSwitchToAdmin: (tab?: string) => void;
  onNotify: (msg: string) => void;
  autoRefresh: boolean;
  onToggleAutoRefresh: (next: boolean) => void;
}

export const SlackWorkspace: React.FC<SlackWorkspaceProps> = ({
  overview,
  harnessCatalogs,
  onRefresh,
  onOpenSwitchModal,
  onStopTicket,
  onSwitchToAdmin,
  onNotify,
  autoRefresh,
  onToggleAutoRefresh,
}) => {
  const [channel, setChannel] = useState<ChannelSelection>({ kind: 'general' });
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('all');
  const [searchQuery, setSearchQuery] = useState('');
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);

  // Right Thread / Steps Drawer state
  const [activeThreadKey, setActiveThreadKey] = useState<string | null>(null);
  const [drawerTab, setDrawerTab] = useState<'thread' | 'steps'>('thread');
  const [threadTrace, setThreadTrace] = useState<WorkerTraceResponse | null>(null);
  const [traceLoading, setTraceLoading] = useState(false);
  const [traceKindFilter, setTraceKindFilter] = useState<string>('all');

  // Expanded thinking/text toggles
  const [expandedThinkingKeys, setExpandedThinkingKeys] = useState<Record<string, boolean>>({});
  const [expandedLongRoots, setExpandedLongRoots] = useState<Record<string, boolean>>({});

  // Main Composer state
  const [composerText, setComposerText] = useState('');
  const [composerRepo, setComposerRepo] = useState<string>('xdlkc/nexus');
  const [composerSending, setComposerSending] = useState(false);
  const [showEnginePicker, setShowEnginePicker] = useState(false);
  const [overrideHarness, setOverrideHarness] = useState<string>('');
  const [overrideModel, setOverrideModel] = useState<string>('');
  const [overrideThink, setOverrideThink] = useState<string>('');

  // Thread Reply Composer state
  const [replyText, setReplyText] = useState('');
  const [replySending, setReplySending] = useState(false);

  const feedEndRef = useRef<HTMLDivElement>(null);
  const feedScrollRef = useRef<HTMLDivElement>(null);
  const threadEndRef = useRef<HTMLDivElement>(null);
  const threadInputRef = useRef<HTMLTextAreaElement>(null);
  const prevChannelSignatureRef = useRef<string>('');

  const configuredRepos = useMemo(() => {
    const set = new Set<string>();
    const cfgRepos = overview?.config?.github?.repos || [];
    for (const r of cfgRepos) {
      if (r.trim()) set.add(r.trim());
    }
    if (overview?.config?.default_repo) {
      set.add(overview.config.default_repo);
    }
    for (const t of Object.values(overview?.state?.tickets || {})) {
      if (t.repo && t.repo.trim()) set.add(t.repo.trim());
    }
    if (set.size === 0) set.add('xdlkc/nexus');
    return Array.from(set);
  }, [overview]);

  const defaultActorId = useMemo(() => {
    const actors = overview?.config?.allowed_actors || [];
    if (actors.includes('xdlkc')) return 'xdlkc';
    return actors[0] || 'xdlkc';
  }, [overview]);

  useEffect(() => {
    if (channel.kind === 'repo') {
      setComposerRepo(channel.repo);
    } else if (overview?.config?.default_repo && !configuredRepos.includes(composerRepo)) {
      setComposerRepo(overview.config.default_repo);
    }
  }, [channel, overview?.config?.default_repo, configuredRepos, composerRepo]);

  // Build unified Slack conversation threads from EngineState
  const allThreads = useMemo<ConversationThread[]>(() => {
    const state = overview?.state;
    if (!state) return [];

    const triggersList = Object.values(state.triggers || {});
    const outboxList = Object.values(state.outbox || {});
    const ticketsList = Object.values(state.tickets || {});
    const defaultRepo = overview?.config?.default_repo || 'xdlkc/nexus';

    const triggersByThread = new Map<string, Trigger[]>();
    const triggerById = new Map<string, Trigger>();
    for (const tr of triggersList) {
      triggerById.set(tr.id, tr);
      const arr = triggersByThread.get(tr.thread_key) || [];
      arr.push(tr);
      triggersByThread.set(tr.thread_key, arr);
    }

    const outboxByTicket = new Map<string, OutboxMessage[]>();
    for (const ob of outboxList) {
      if (!ob.ticket_id) continue;
      const arr = outboxByTicket.get(ob.ticket_id) || [];
      arr.push(ob);
      outboxByTicket.set(ob.ticket_id, arr);
    }

    const usedTriggerIds = new Set<string>();
    const conversations: ConversationThread[] = [];

    for (const ticket of ticketsList) {
      const originKey =
        ticket.origin_thread_key ||
        ticket.subscriptions?.[0] ||
        `ticket:${ticket.id}`;

      const matchedTriggersMap = new Map<string, Trigger>();
      if (ticket.origin_trigger_id && triggerById.has(ticket.origin_trigger_id)) {
        const orig = triggerById.get(ticket.origin_trigger_id)!;
        matchedTriggersMap.set(orig.id, orig);
      }
      const relatedThreadKeys = new Set<string>([
        originKey,
        ...(ticket.subscriptions || []),
      ]);
      for (const tk of relatedThreadKeys) {
        for (const tr of triggersByThread.get(tk) || []) {
          matchedTriggersMap.set(tr.id, tr);
        }
      }
      for (const inp of ticket.inputs || []) {
        const tid = inp.kind?.trigger_id;
        if (tid && triggerById.has(tid)) {
          matchedTriggersMap.set(tid, triggerById.get(tid)!);
        }
      }

      const matchedTriggers = Array.from(matchedTriggersMap.values()).sort((a, b) =>
        (a.at || '').localeCompare(b.at || '')
      );
      for (const tr of matchedTriggers) usedTriggerIds.add(tr.id);

      const ticketOutbox = (outboxByTicket.get(ticket.id) || []).sort((a, b) =>
        (a.created_at || '').localeCompare(b.created_at || '')
      );

      const rootTrig =
        (ticket.origin_trigger_id ? triggerById.get(ticket.origin_trigger_id) : undefined) ||
        matchedTriggers[0];

      const sourceMeta = classifySource(
        originKey,
        rootTrig?.source_id,
        rootTrig?.source_kind
      );

      const lastAsg = ticket.assignments?.[ticket.assignments.length - 1];
      const latestReport = lastAsg?.latest_report;
      const derivedStatus = deriveTicketStatus(ticket);
      const { prUrl, prLabel } = extractPrLink(ticket, latestReport?.summary);

      const timeline: ThreadTimelineEntry[] = [];
      for (const tr of matchedTriggers) {
        const src = classifySource(tr.thread_key, tr.source_id, tr.source_kind);
        timeline.push({
          id: `trig-${tr.id}`,
          at: tr.at,
          role: 'user',
          author: formatActorName(tr.actor_id, src.kind),
          sourceLabel: src.label,
          text: tr.text,
        });
      }

      for (const ob of ticketOutbox) {
        const isThinking = ob.body.startsWith('💭');
        timeline.push({
          id: `ob-${ob.id}`,
          at: ob.created_at,
          role: isThinking ? 'thinking' : 'bot',
          author: '奈克斯',
          sourceLabel: isThinking ? '实时进展' : `回复 · ${ob.purpose || ob.status}`,
          text: ob.body,
        });
      }

      // If Worker produced a latestReport whose summary/question isn't already represented in outbox
      const nonThinkingOutbox = ticketOutbox.filter((o) => !o.body.startsWith('💭'));
      if (latestReport && (latestReport.summary || latestReport.question)) {
        const latestNonThinkingText =
          nonThinkingOutbox[nonThinkingOutbox.length - 1]?.body || '';
        const summaryTrimmed = (latestReport.summary || '').trim();
        const isThinkingSummary = summaryTrimmed.startsWith('💭');
        const sameAsOutbox = Boolean(
          latestNonThinkingText &&
            summaryTrimmed &&
            (latestNonThinkingText.includes(summaryTrimmed.slice(0, 40)) ||
              summaryTrimmed.includes(latestNonThinkingText.slice(0, 40)))
        );
        if (!sameAsOutbox || latestReport.question) {
          timeline.push({
            id: `rep-${lastAsg?.id || ticket.id}-${latestReport.at}`,
            at: latestReport.at,
            role: isThinkingSummary ? 'thinking' : 'report',
            author: '奈克斯',
            sourceLabel: latestReport.question
              ? '等待拍板'
              : isThinkingSummary
              ? '实时进展'
              : latestReport.status === 'ready'
              ? '交付汇报'
              : '状态更新',
            text: latestReport.summary,
            question: latestReport.question,
            prUrl,
          });
        }
      }

      timeline.sort((a, b) => (a.at || '').localeCompare(b.at || ''));

      const latestThinking = ticketOutbox
        .filter((o) => o.body.startsWith('💭'))
        .slice(-1)[0]?.body;

      const latestReply =
        latestReport?.summary?.trim() ||
        nonThinkingOutbox[nonThinkingOutbox.length - 1]?.body ||
        undefined;

      const latestAt =
        timeline[timeline.length - 1]?.at ||
        ticket.ended?.at ||
        ticket.updated_at ||
        ticket.created_at;

      const rootActor = rootTrig
        ? formatActorName(rootTrig.actor_id, sourceMeta.kind)
        : '系统任务';

      const rootText = rootTrig?.text || ticket.title;
      const inferredRepo =
        ticket.repo || inferRepoFromThreadKey(originKey) || defaultRepo;

      conversations.push({
        key: ticket.id,
        threadKey: originKey,
        ticket,
        title: ticket.title,
        repo: inferredRepo,
        sourceKind: sourceMeta.kind,
        sourceLabel: sourceMeta.label,
        rootActor,
        rootText,
        createdAt: rootTrig?.at || ticket.created_at,
        latestAt,
        derivedStatus,
        latestReply,
        latestThinking,
        question: latestReport?.question,
        prUrl,
        prLabel,
        target: lastAsg?.target,
        timeline,
        replyCount: Math.max(0, timeline.length - 1),
        isOpen: isTicketOpen(ticket),
      });
    }

    // Group remaining unattached triggers by thread_key (e.g. freshly sent messages in eval)
    const orphanGroups = new Map<string, Trigger[]>();
    for (const tr of triggersList) {
      if (usedTriggerIds.has(tr.id)) continue;
      const arr = orphanGroups.get(tr.thread_key) || [];
      arr.push(tr);
      orphanGroups.set(tr.thread_key, arr);
    }

    for (const [tk, trigs] of orphanGroups.entries()) {
      const threadObj = state.threads?.[tk];
      const isStillQueued = threadObj
        ? (threadObj.queued_trigger_ids?.length || 0) > 0
        : Object.values(state.decisions || {}).every(
            (d) => !trigs.some((tr) => d.trigger_ids?.includes(tr.id))
          );
      if (!isStillQueued) continue;

      trigs.sort((a, b) => (a.at || '').localeCompare(b.at || ''));
      const first = trigs[0];
      const last = trigs[trigs.length - 1];
      const src = classifySource(tk, first.source_id, first.source_kind);
      const repo =
        (typeof first.meta?.repo === 'string' && first.meta.repo) ||
        inferRepoFromThreadKey(tk) ||
        defaultRepo;

      conversations.push({
        key: `thread:${tk}`,
        threadKey: tk,
        title: first.text.split('\n')[0].slice(0, 80),
        repo,
        sourceKind: src.kind,
        sourceLabel: src.label,
        rootActor: formatActorName(first.actor_id, src.kind),
        rootText: first.text,
        createdAt: first.at,
        latestAt: last.at,
        derivedStatus: 'queued',
        latestReply: '正在由双引擎路由 (Jev + Router) 分诊并准备执行...',
        timeline: trigs.map((tr) => ({
          id: `trig-${tr.id}`,
          at: tr.at,
          role: 'user',
          author: formatActorName(tr.actor_id, src.kind),
          sourceLabel: src.label,
          text: tr.text,
        })),
        replyCount: Math.max(0, trigs.length - 1),
        isOpen: true,
      });
    }

    return conversations;
  }, [overview]);

  // Sidebar counters
  const stats = useMemo(() => {
    let inboxCount = 0;
    let activeCount = 0;
    const repoActiveCounts: Record<string, number> = {};
    const repoInboxCounts: Record<string, number> = {};

    for (const c of allThreads) {
      if (
        c.derivedStatus === 'needs_decision' ||
        c.derivedStatus === 'blocked' ||
        c.derivedStatus === 'ready_review' ||
        c.derivedStatus === 'attention'
      ) {
        inboxCount += 1;
        repoInboxCounts[c.repo] = (repoInboxCounts[c.repo] || 0) + 1;
      }
      if (c.derivedStatus === 'running' || c.derivedStatus === 'queued') {
        activeCount += 1;
        repoActiveCounts[c.repo] = (repoActiveCounts[c.repo] || 0) + 1;
      }
    }

    return { inboxCount, activeCount, repoActiveCounts, repoInboxCounts };
  }, [allThreads]);

  // Visible threads for current channel & filters
  const visibleThreads = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    const filtered = allThreads.filter((c) => {
      if (channel.kind === 'inbox') {
        const needsAction =
          c.derivedStatus === 'needs_decision' ||
          c.derivedStatus === 'blocked' ||
          c.derivedStatus === 'ready_review' ||
          c.derivedStatus === 'attention';
        if (!needsAction) return false;
      } else if (channel.kind === 'active') {
        if (c.derivedStatus !== 'running' && c.derivedStatus !== 'queued') return false;
      } else if (channel.kind === 'repo') {
        if (c.repo !== channel.repo) return false;
      } else if (channel.kind === 'source') {
        if (c.sourceKind !== channel.source) return false;
      }

      if (statusFilter === 'running') {
        if (c.derivedStatus !== 'running' && c.derivedStatus !== 'queued') return false;
      } else if (statusFilter === 'ready') {
        if (c.derivedStatus !== 'ready_review' && c.derivedStatus !== 'completed') return false;
      } else if (statusFilter === 'blocked') {
        if (
          c.derivedStatus !== 'needs_decision' &&
          c.derivedStatus !== 'blocked' &&
          c.derivedStatus !== 'attention'
        ) {
          return false;
        }
      }

      if (q) {
        const hay = [
          c.ticket?.id || '',
          c.title,
          c.rootText,
          c.latestReply || '',
          c.repo,
          c.rootActor,
          c.prUrl || '',
        ]
          .join(' ')
          .toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });

    if (channel.kind === 'inbox' || channel.kind === 'active') {
      const priority: Record<DerivedTicketStatus, number> = {
        needs_decision: 0,
        attention: 1,
        blocked: 2,
        ready_review: 3,
        running: 4,
        queued: 5,
        idle: 6,
        completed: 7,
        stopped: 8,
        dropped: 9,
      };
      return filtered.sort((a, b) => {
        const pa = priority[a.derivedStatus] ?? 99;
        const pb = priority[b.derivedStatus] ?? 99;
        if (pa !== pb) return pa - pb;
        return (b.latestAt || '').localeCompare(a.latestAt || '');
      });
    }

    // Channel feed: chronological (oldest at top, newest at bottom near composer)
    return filtered.sort((a, b) => (a.latestAt || '').localeCompare(b.latestAt || ''));
  }, [allThreads, channel, statusFilter, searchQuery]);

  // Auto-scroll to newest message when switching channels
  useEffect(() => {
    const sig =
      channel.kind === 'repo'
        ? `repo:${channel.repo}`
        : channel.kind === 'source'
        ? `source:${channel.source}`
        : channel.kind;
    if (prevChannelSignatureRef.current !== sig && visibleThreads.length > 0) {
      prevChannelSignatureRef.current = sig;
      if (channel.kind !== 'inbox' && channel.kind !== 'active') {
        requestAnimationFrame(() => {
          feedEndRef.current?.scrollIntoView({ behavior: 'auto' });
        });
      } else {
        feedScrollRef.current?.scrollTo({ top: 0 });
      }
    }
  }, [channel, visibleThreads.length]);

  const activeThread = useMemo(
    () => allThreads.find((c) => c.key === activeThreadKey || c.threadKey === activeThreadKey) || null,
    [allThreads, activeThreadKey]
  );

  // Fetch Worker trace whenever Right Drawer is open on a ticket (both Thread & Live Steps tabs)
  useEffect(() => {
    const ticketId = activeThread?.ticket?.id;
    if (!ticketId) {
      setThreadTrace(null);
      return;
    }

    let cancelled = false;
    const loadTrace = async () => {
      setTraceLoading(true);
      try {
        const res = await getWorkerTrace(ticketId);
        if (!cancelled) setThreadTrace(res);
      } catch {
        // ignore transient errors
      } finally {
        if (!cancelled) setTraceLoading(false);
      }
    };
    loadTrace();

    if (!autoRefresh) return;
    const timer = setInterval(loadTrace, 3000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [activeThread?.ticket?.id, autoRefresh]);

  const activeThreadMessages = useMemo<ThreadTimelineEntry[]>(() => {
    if (!activeThread) return [];
    const base = [...activeThread.timeline];
    const traceSteps =
      threadTrace && threadTrace.ticket_id === activeThread.ticket?.id
        ? threadTrace.steps || []
        : [];

    const normalizePrefix = (s: string) => s.replace(/^💭\s*/, '').trim().slice(0, 48);
    const seen = new Set<string>(
      base.map((entry) => normalizePrefix(entry.text || '')).filter(Boolean)
    );

    for (const step of traceSteps) {
      if (
        step.kind !== 'thinking' &&
        step.kind !== 'message' &&
        step.kind !== 'finish' &&
        step.kind !== 'error'
      ) {
        continue;
      }
      const rawText = (step.detail || '').trim();
      if (!rawText) continue;
      const norm = normalizePrefix(rawText);
      if (norm && seen.has(norm)) continue;
      if (norm) seen.add(norm);

      base.push({
        id: `trace-${step.assignment_id}-${step.seq}`,
        at: step.at,
        role:
          step.kind === 'thinking'
            ? 'thinking'
            : step.kind === 'finish' || step.kind === 'error'
            ? 'report'
            : 'bot',
        author: '奈克斯',
        sourceLabel:
          step.kind === 'thinking'
            ? '思考进展'
            : step.kind === 'finish'
            ? '执行汇报'
            : step.kind === 'error'
            ? '执行异常'
            : '回复',
        text: step.kind === 'thinking' && !rawText.startsWith('💭') ? `💭 ${rawText}` : rawText,
      });
    }

    return base.sort((a, b) => (a.at || '').localeCompare(b.at || ''));
  }, [activeThread, threadTrace]);

  useEffect(() => {
    if (activeThreadKey && drawerTab === 'thread' && activeThreadMessages.length > 0) {
      requestAnimationFrame(() => {
        threadEndRef.current?.scrollIntoView({ behavior: 'smooth' });
      });
    }
  }, [activeThreadKey, drawerTab, activeThreadMessages.length]);

  const openThread = (c: ConversationThread, tab: 'thread' | 'steps' = 'thread', focusInput = false) => {
    setActiveThreadKey(c.key);
    setDrawerTab(tab);
    if (focusInput && tab === 'thread') {
      setTimeout(() => threadInputRef.current?.focus(), 80);
    }
  };

  const handleSendMainMessage = async () => {
    const trimmed = composerText.trim();
    if (!trimmed || composerSending) return;
    setComposerSending(true);
    try {
      const targetRepo = channel.kind === 'repo' ? channel.repo : composerRepo;
      const inlineTarget = overrideHarness
        ? {
            harness: overrideHarness,
            model: overrideModel || undefined,
            think_effort: overrideThink || undefined,
          }
        : null;

      await sendWorkspaceMessage({
        text: trimmed,
        repo: targetRepo,
        actorId: defaultActorId,
        inlineTarget,
      });
      setComposerText('');
      if (channel.kind === 'inbox' || (channel.kind === 'source' && channel.source !== 'web')) {
        setChannel({ kind: 'repo', repo: targetRepo });
      }
      onNotify(`已发送到 #${shortRepoName(targetRepo)}，奈克斯正在后台接单处理`);
      await onRefresh();
      requestAnimationFrame(() => {
        feedEndRef.current?.scrollIntoView({ behavior: 'smooth' });
      });
    } catch (err) {
      onNotify(`发送失败: ${err instanceof Error ? err.message : '未知错误'}`);
    } finally {
      setComposerSending(false);
    }
  };

  const handleSendThreadReply = async () => {
    const trimmed = replyText.trim();
    if (!trimmed || !activeThread || replySending) return;
    setReplySending(true);
    try {
      await sendWorkspaceMessage({
        text: trimmed,
        threadKey: activeThread.threadKey,
        repo: activeThread.repo,
        actorId: defaultActorId,
      });
      setReplyText('');
      onNotify('回复已投递至当前话题，奈克斯会接着上下文继续处理');
      await onRefresh();
    } catch (err) {
      onNotify(`回复失败: ${err instanceof Error ? err.message : '未知错误'}`);
    } finally {
      setReplySending(false);
    }
  };

  const handleQuickAcknowledge = async (c: ConversationThread) => {
    try {
      await sendWorkspaceMessage({
        text: '收到，已确认验收！请总结收尾（如有未合入改动可保持当前 PR 状态）。',
        threadKey: c.threadKey,
        repo: c.repo,
        actorId: defaultActorId,
      });
      onNotify(`已发送确认反馈到 ${c.ticket?.id || '当前话题'}`);
      await onRefresh();
    } catch (err) {
      onNotify(`操作失败: ${err instanceof Error ? err.message : '未知错误'}`);
    }
  };

  const availableModelsForOverride = useMemo(() => {
    if (!overrideHarness) return [];
    const cat = harnessCatalogs[overrideHarness];
    if (!cat) return [];
    const models: string[] = [];
    for (const [prov, list] of Object.entries(cat.providers || {})) {
      for (const m of list) {
        models.push(prov === cat.default_provider ? m : `${prov}/${m}`);
      }
    }
    return models;
  }, [overrideHarness, harnessCatalogs]);

  const channelHeaderInfo = useMemo(() => {
    switch (channel.kind) {
      case 'inbox':
        return {
          title: '待我决策与验收 (Action Inbox)',
          subtitle: '聚焦需要你拍板回答的问题、阻塞异常与已完成待验收的 PR 成果',
          icon: <Inbox className="w-4 h-4 text-amber-400" />,
          repoUrl: null,
        };
      case 'active':
        return {
          title: '后台正在执行中 (Active Workers)',
          subtitle: '实时查看所有并发 Worker 正在推进的任务进度与思考流',
          icon: <Zap className="w-4 h-4 text-emerald-400" />,
          repoUrl: null,
        };
      case 'repo':
        return {
          title: `# ${shortRepoName(channel.repo)}`,
          subtitle: `仓库专属协作频道 · ${channel.repo}`,
          icon: <Hash className="w-4 h-4 text-indigo-400" />,
          repoUrl: `https://github.com/${channel.repo}`,
        };
      case 'source': {
        const map = {
          feishu: { name: '# feishu-飞书群聊', desc: '来自飞书群的话题与实时同步回复' },
          dingtalk: { name: '# dingtalk-钉钉群聊', desc: '来自钉钉群的话题与实时同步回复' },
          github: { name: '# github-issues-prs', desc: '来自 GitHub Issues / PR / Review 的协作流' },
          web: { name: '# web-工作台', desc: '直接在 Nexus 网页工作台发起的对话与任务' },
        }[channel.source];
        return {
          title: map.name,
          subtitle: map.desc,
          icon: <Layers className="w-4 h-4 text-cyan-400" />,
          repoUrl: null,
        };
      }
      default:
        return {
          title: '# general · 综合协同大厅',
          subtitle: '统一聚合飞书、钉钉、GitHub 与 Web 工作台的所有任务对话流',
          icon: <Hash className="w-4 h-4 text-indigo-400" />,
          repoUrl: null,
        };
    }
  }, [channel]);

  const maxWorkers = overview?.config?.max_concurrent_workers || 4;
  const botName = overview?.config?.feishu?.bot_name || '奈克斯';

  return (
    <div className="h-[calc(100vh-44px)] flex bg-[#090c14] text-slate-100 overflow-hidden relative">
      {/* MOBILE SIDEBAR BACKDROP */}
      {mobileSidebarOpen && (
        <div
          onClick={() => setMobileSidebarOpen(false)}
          className="fixed inset-0 z-30 bg-black/60 lg:hidden"
        />
      )}

      {/* COLUMN 1: SLACK WORKSPACE LEFT SIDEBAR */}
      <aside
        className={`fixed lg:static inset-y-0 left-0 z-40 w-[264px] shrink-0 bg-[#0d101b] border-r border-slate-800/90 flex flex-col transition-transform duration-200 ${
          mobileSidebarOpen ? 'translate-x-0' : '-translate-x-full lg:translate-x-0'
        }`}
      >
        {/* Workspace Title Header */}
        <div className="px-4 py-3.5 border-b border-slate-800/80 flex items-center justify-between">
          <div className="flex items-center gap-2.5 min-w-0">
            <div className="w-8 h-8 rounded-lg bg-gradient-to-br from-indigo-500 via-violet-500 to-cyan-400 flex items-center justify-center shadow-md shadow-indigo-500/20 shrink-0">
              <Zap className="w-4 h-4 text-white" />
            </div>
            <div className="min-w-0">
              <div className="font-bold text-sm text-white truncate flex items-center gap-1.5">
                Nexus Workspace
              </div>
              <div className="text-[11px] text-emerald-400 flex items-center gap-1.5">
                <span className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" />
                @{botName} 在线协同中
              </div>
            </div>
          </div>
          <button
            type="button"
            onClick={() => setMobileSidebarOpen(false)}
            className="lg:hidden p-1 rounded text-slate-400 hover:text-white"
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* Sidebar Search */}
        <div className="px-3 pt-3 pb-2">
          <div className="relative">
            <Search className="w-3.5 h-3.5 text-slate-500 absolute left-2.5 top-1/2 -translate-y-1/2" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="搜索频道、消息、工单或 PR..."
              className="w-full pl-8 pr-7 py-1.5 rounded-lg bg-[#070a10] border border-slate-800 text-xs text-slate-200 placeholder:text-slate-500 focus:border-indigo-500/60 focus:outline-none"
            />
            {searchQuery && (
              <button
                type="button"
                onClick={() => setSearchQuery('')}
                className="absolute right-2 top-1/2 -translate-y-1/2 text-slate-500 hover:text-slate-300 cursor-pointer"
              >
                <X className="w-3 h-3" />
              </button>
            )}
          </div>
        </div>

        {/* Scrollable Channel List */}
        <div className="flex-1 overflow-y-auto px-2.5 py-2 space-y-5">
          {/* Core Views */}
          <div>
            <div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-500">
              核心收件箱与大厅
            </div>
            <div className="space-y-0.5">
              <SlackNavItem
                active={channel.kind === 'inbox'}
                onClick={() => {
                  setChannel({ kind: 'inbox' });
                  setMobileSidebarOpen(false);
                }}
                icon={<Inbox className="w-4 h-4 text-amber-400" />}
                label="待我决策与验收"
                badge={stats.inboxCount > 0 ? String(stats.inboxCount) : undefined}
                badgeTone="amber"
              />
              <SlackNavItem
                active={channel.kind === 'general'}
                onClick={() => {
                  setChannel({ kind: 'general' });
                  setMobileSidebarOpen(false);
                }}
                icon={<Hash className="w-4 h-4 text-slate-400" />}
                label="general · 综合大厅"
                badge={String(allThreads.length)}
                badgeTone="muted"
              />
              <SlackNavItem
                active={channel.kind === 'active'}
                onClick={() => {
                  setChannel({ kind: 'active' });
                  setMobileSidebarOpen(false);
                }}
                icon={<Zap className="w-4 h-4 text-emerald-400" />}
                label="正在执行中"
                badge={stats.activeCount > 0 ? String(stats.activeCount) : undefined}
                badgeTone="emerald"
              />
            </div>
          </div>

          {/* Project Channels */}
          <div>
            <div className="px-2 pb-1 flex items-center justify-between text-[10px] font-semibold uppercase tracking-wider text-slate-500">
              <span>项目仓库频道</span>
              <span>{configuredRepos.length}</span>
            </div>
            <div className="space-y-0.5">
              {configuredRepos.map((repo) => {
                const runningN = stats.repoActiveCounts[repo] || 0;
                const inboxN = stats.repoInboxCounts[repo] || 0;
                return (
                  <SlackNavItem
                    key={repo}
                    active={channel.kind === 'repo' && channel.repo === repo}
                    onClick={() => {
                      setChannel({ kind: 'repo', repo });
                      setMobileSidebarOpen(false);
                    }}
                    icon={<Hash className="w-3.5 h-3.5 text-indigo-400" />}
                    label={shortRepoName(repo)}
                    subLabel={repo}
                    livePulse={runningN > 0}
                    badge={
                      runningN > 0
                        ? `${runningN} 运行中`
                        : inboxN > 0
                        ? String(inboxN)
                        : undefined
                    }
                    badgeTone={runningN > 0 ? 'emerald' : 'amber'}
                  />
                );
              })}
            </div>
          </div>

          {/* Connected Channels */}
          <div>
            <div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-500">
              跨渠道同步视图
            </div>
            <div className="space-y-0.5">
              <SlackNavItem
                active={channel.kind === 'source' && channel.source === 'feishu'}
                onClick={() => {
                  setChannel({ kind: 'source', source: 'feishu' });
                  setMobileSidebarOpen(false);
                }}
                icon={<MessageSquare className="w-3.5 h-3.5 text-sky-400" />}
                label="feishu-飞书群聊"
              />
              <SlackNavItem
                active={channel.kind === 'source' && channel.source === 'dingtalk'}
                onClick={() => {
                  setChannel({ kind: 'source', source: 'dingtalk' });
                  setMobileSidebarOpen(false);
                }}
                icon={<MessageSquare className="w-3.5 h-3.5 text-cyan-400" />}
                label="dingtalk-钉钉群聊"
              />
              <SlackNavItem
                active={channel.kind === 'source' && channel.source === 'github'}
                onClick={() => {
                  setChannel({ kind: 'source', source: 'github' });
                  setMobileSidebarOpen(false);
                }}
                icon={<GitPullRequest className="w-3.5 h-3.5 text-purple-400" />}
                label="github-issues-prs"
              />
              <SlackNavItem
                active={channel.kind === 'source' && channel.source === 'web'}
                onClick={() => {
                  setChannel({ kind: 'source', source: 'web' });
                  setMobileSidebarOpen(false);
                }}
                icon={<Sparkles className="w-3.5 h-3.5 text-indigo-400" />}
                label="web-工作台"
              />
            </div>
          </div>

          {/* Agent Roster */}
          <div>
            <div className="px-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-slate-500">
              智能体与并发池
            </div>
            <div className="rounded-xl bg-[#090c14] border border-slate-800/80 p-2.5 space-y-2 text-xs">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2 min-w-0">
                  <span className="relative flex h-2.5 w-2.5">
                    <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-60" />
                    <span className="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-500" />
                  </span>
                  <span className="font-semibold text-slate-100 truncate">@{botName}</span>
                </div>
                <span className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-indigo-500/15 text-indigo-300 border border-indigo-500/25">
                  Router
                </span>
              </div>
              <div className="flex items-center justify-between text-[11px] text-slate-400">
                <span>后台并发槽位</span>
                <span className="font-mono text-slate-200">
                  {stats.activeCount} / {maxWorkers} 忙碌
                </span>
              </div>
              <div className="flex flex-wrap gap-1 pt-0.5">
                {['codex', 'claude-code', 'pi-native', 'opencode'].map((h) => (
                  <span
                    key={h}
                    className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-slate-800/80 text-slate-400"
                  >
                    {h}
                  </span>
                ))}
              </div>
            </div>
          </div>
        </div>

        {/* Bottom Footer Controls */}
        <div className="p-3 border-t border-slate-800/80 space-y-2 text-xs">
          <label className="flex items-center justify-between px-1 text-[11px] text-slate-400 cursor-pointer">
            <span>3s 实时流同步</span>
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => onToggleAutoRefresh(e.target.checked)}
              className="accent-indigo-500 rounded"
            />
          </label>
          <button
            type="button"
            onClick={() => onSwitchToAdmin('settings')}
            className="w-full flex items-center justify-center gap-1.5 px-3 py-1.5 rounded-lg bg-slate-800/70 hover:bg-slate-800 text-[11px] text-slate-300 border border-slate-700/60 transition cursor-pointer"
          >
            <Wrench className="w-3.5 h-3.5 text-slate-400" />
            前往控制面配置 / 记忆
          </button>
        </div>
      </aside>

      {/* COLUMN 2: CENTER MESSAGE FEED + RICH COMPOSER */}
      <div className="flex-1 flex flex-col min-w-0 bg-[#0b0f18]">
        {/* Channel Header */}
        <header className="shrink-0 px-4 sm:px-6 py-3 border-b border-slate-800/90 bg-[#0c101b]/95 backdrop-blur flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2.5 min-w-0">
            <button
              type="button"
              onClick={() => setMobileSidebarOpen(true)}
              className="lg:hidden p-1.5 rounded-lg bg-slate-800/70 text-slate-300 hover:text-white"
            >
              <Menu className="w-4 h-4" />
            </button>
            <div className="p-1.5 rounded-lg bg-slate-800/70 border border-slate-700/50 shrink-0">
              {channelHeaderInfo.icon}
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <h1 className="font-bold text-sm text-white truncate">
                  {channelHeaderInfo.title}
                </h1>
                {channelHeaderInfo.repoUrl && (
                  <a
                    href={channelHeaderInfo.repoUrl}
                    target="_blank"
                    rel="noreferrer"
                    className="hidden sm:inline-flex items-center gap-1 text-[11px] font-mono px-2 py-0.5 rounded bg-slate-800/90 text-slate-300 hover:text-indigo-300 border border-slate-700/60"
                  >
                    <GitBranch className="w-3 h-3" />
                    GitHub
                    <ExternalLink className="w-2.5 h-2.5" />
                  </a>
                )}
              </div>
              <p className="text-[11px] text-slate-400 truncate">{channelHeaderInfo.subtitle}</p>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <div className="inline-flex items-center rounded-lg bg-[#070a10] p-0.5 border border-slate-800 text-[11px]">
              {(
                [
                  { id: 'all', label: '全部' },
                  { id: 'running', label: '进行中' },
                  { id: 'ready', label: '待验收' },
                  { id: 'blocked', label: '待拍板' },
                ] as const
              ).map((tab) => (
                <button
                  key={tab.id}
                  type="button"
                  onClick={() => setStatusFilter(tab.id)}
                  className={`px-2.5 py-1 rounded-md font-medium transition cursor-pointer ${
                    statusFilter === tab.id
                      ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/30'
                      : 'text-slate-400 hover:text-slate-200 border border-transparent'
                  }`}
                >
                  {tab.label}
                </button>
              ))}
            </div>

            <button
              type="button"
              onClick={() => onRefresh()}
              className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-slate-800/80 hover:bg-slate-700/80 text-[11px] text-slate-300 border border-slate-700/60 transition cursor-pointer"
              title="立即同步"
            >
              <RefreshCw className="w-3.5 h-3.5" />
            </button>
          </div>
        </header>

        {/* Scrollable Message Stream */}
        <div
          ref={feedScrollRef}
          className="flex-1 overflow-y-auto px-4 sm:px-6 py-5 space-y-4"
        >
          {visibleThreads.length === 0 ? (
            <div className="max-w-lg mx-auto my-12 rounded-2xl border border-dashed border-slate-800 bg-[#0d121d]/60 p-8 text-center space-y-3">
              <div className="w-11 h-11 rounded-2xl bg-indigo-500/15 border border-indigo-500/30 flex items-center justify-center mx-auto">
                {channel.kind === 'inbox' ? (
                  <CheckCircle2 className="w-5 h-5 text-emerald-400" />
                ) : (
                  <MessageSquare className="w-5 h-5 text-indigo-400" />
                )}
              </div>
              <div className="text-sm font-semibold text-white">
                {channel.kind === 'inbox'
                  ? '太棒了！当前没有待决策或待验收的事项'
                  : '该频道下暂无匹配的任务或讨论'}
              </div>
              <p className="text-xs text-slate-400 leading-relaxed">
                {channel.kind === 'inbox'
                  ? '当奈克斯遇到需要拍板的问题、任务受阻或产出新的 PR 待你验收时，会自动置顶出现在这里。'
                  : '在下方输入框直接向 @奈克斯 下达新任务，系统会自动分派后台 Worker 在独立 Worktree 中推进。'}
              </p>
            </div>
          ) : (
            visibleThreads.map((c) => {
              const isSelected = activeThread?.key === c.key;
              const isRootLong = c.rootText.length > 380 || c.rootText.split('\n').length > 7;
              const rootExpanded = Boolean(expandedLongRoots[c.key]);
              const thinkingExpanded = Boolean(expandedThinkingKeys[c.key]);
              const statusMeta = getStatusBadgeMeta(c.derivedStatus);

              return (
                <article
                  key={c.key}
                  className={`rounded-2xl border transition-colors p-4 ${
                    isSelected
                      ? 'border-indigo-500/60 bg-[#111728]/95 shadow-lg shadow-indigo-950/30'
                      : c.derivedStatus === 'needs_decision' || c.derivedStatus === 'blocked'
                      ? 'border-amber-500/35 bg-[#12131c]/90 hover:border-amber-500/55'
                      : 'border-slate-800/85 bg-[#0e131e]/85 hover:border-slate-700/90'
                  }`}
                >
                  {/* Root Human / Trigger Message */}
                  <div className="flex items-start gap-3">
                    <div className="w-9 h-9 rounded-xl bg-slate-800 border border-slate-700 flex items-center justify-center shrink-0 text-slate-200 font-semibold text-xs">
                      <User className="w-4 h-4 text-sky-300" />
                    </div>

                    <div className="flex-1 min-w-0">
                      <div className="flex flex-wrap items-center gap-2 mb-1">
                        <span className="font-semibold text-xs text-white">{c.rootActor}</span>
                        <span
                          className={`text-[10px] px-1.5 py-0.5 rounded border font-medium ${getSourceBadgeClass(
                            c.sourceKind
                          )}`}
                        >
                          {c.sourceLabel}
                        </span>
                        <span className="text-[11px] font-mono text-indigo-300/90">
                          #{shortRepoName(c.repo)}
                        </span>
                        <span className="text-[11px] text-slate-500 font-mono">
                          {formatRelativeTime(c.createdAt)}
                        </span>
                      </div>

                      <div className="text-[13px] text-slate-200 leading-relaxed whitespace-pre-wrap break-words">
                        {isRootLong && !rootExpanded
                          ? `${c.rootText.slice(0, 380)}...`
                          : c.rootText}
                      </div>
                      {isRootLong && (
                        <button
                          type="button"
                          onClick={() =>
                            setExpandedLongRoots((prev) => ({ ...prev, [c.key]: !prev[c.key] }))
                          }
                          className="mt-1 text-[11px] text-indigo-400 hover:text-indigo-300 cursor-pointer"
                        >
                          {rootExpanded ? '收起原文' : '展开完整需求内容'}
                        </button>
                      )}

                      {/* Inline @奈克斯 Reply & Slack Attachment Card */}
                      <div className="mt-3 pl-3 border-l-2 border-indigo-500/40 space-y-2.5">
                        {/* Collapsible Live Thinking Progress */}
                        {c.latestThinking && (
                          <button
                            type="button"
                            onClick={() =>
                              setExpandedThinkingKeys((prev) => ({
                                ...prev,
                                [c.key]: !prev[c.key],
                              }))
                            }
                            className="w-full text-left rounded-lg bg-amber-500/5 border border-amber-500/20 px-2.5 py-1.5 text-[11px] text-amber-200/85 hover:bg-amber-500/10 transition cursor-pointer flex items-start gap-2"
                          >
                            {thinkingExpanded ? (
                              <ChevronDown className="w-3.5 h-3.5 shrink-0 mt-0.5 text-amber-400" />
                            ) : (
                              <ChevronRight className="w-3.5 h-3.5 shrink-0 mt-0.5 text-amber-400" />
                            )}
                            <span className={thinkingExpanded ? 'whitespace-pre-wrap' : 'truncate'}>
                              {c.latestThinking}
                            </span>
                          </button>
                        )}

                        {/* Bot Header + Main Summary */}
                        <div className="flex items-start gap-2.5">
                          <div className="w-7 h-7 rounded-lg bg-gradient-to-br from-indigo-500 to-cyan-500 flex items-center justify-center shrink-0 mt-0.5 shadow-sm">
                            <Bot className="w-4 h-4 text-white" />
                          </div>
                          <div className="flex-1 min-w-0">
                            <div className="flex flex-wrap items-center gap-1.5 mb-0.5">
                              <span className="font-semibold text-xs text-white">{botName}</span>
                              <span className="text-[9px] font-mono uppercase px-1.5 py-0.2 rounded bg-indigo-500/20 text-indigo-300 border border-indigo-500/30">
                                APP
                              </span>
                              <span className="text-[11px] text-slate-500 font-mono">
                                {formatRelativeTime(c.latestAt)}
                              </span>
                            </div>

                            {c.latestReply ? (
                              <p className="text-[13px] text-slate-200 leading-relaxed whitespace-pre-wrap break-words">
                                {c.latestReply}
                              </p>
                            ) : (
                              <p className="text-xs text-slate-400 italic">
                                Worker 已启动，正在独立工作区分析上下文与执行任务...
                              </p>
                            )}
                          </div>
                        </div>

                        {/* Question / Decision Callout */}
                        {c.question && (
                          <div className="rounded-xl border border-amber-500/40 bg-amber-500/10 p-3 space-y-2">
                            <div className="flex items-center gap-1.5 text-xs font-semibold text-amber-300">
                              <AlertCircle className="w-4 h-4 shrink-0" />
                              需要你拍板决定
                            </div>
                            <p className="text-xs text-amber-100 whitespace-pre-wrap leading-relaxed">
                              {c.question}
                            </p>
                            <button
                              type="button"
                              onClick={() => openThread(c, 'thread', true)}
                              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-amber-500 hover:bg-amber-400 text-slate-950 text-xs font-semibold transition cursor-pointer"
                            >
                              <MessageSquare className="w-3.5 h-3.5" />
                              回复拍板
                            </button>
                          </div>
                        )}

                        {/* Slack Block-Kit Status Card */}
                        {c.ticket && (
                          <div
                            className={`rounded-xl border border-slate-800/90 bg-[#090c14]/90 p-2.5 pl-3 border-l-4 ${statusMeta.cardBorder} flex flex-wrap items-center justify-between gap-2`}
                          >
                            <div className="flex flex-wrap items-center gap-2 min-w-0">
                              <span className="font-mono text-[11px] font-semibold text-indigo-300">
                                {c.ticket.id}
                              </span>
                              <span
                                className={`inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full border font-medium ${statusMeta.badgeClass}`}
                              >
                                {statusMeta.dotPulse && (
                                  <span className="w-1.5 h-1.5 rounded-full bg-current animate-ping" />
                                )}
                                {statusMeta.label}
                              </span>
                              {c.target && (
                                <span className="inline-flex items-center gap-1 font-mono text-[10.5px] px-2 py-0.5 rounded bg-slate-800/80 text-slate-300 border border-slate-700/60">
                                  <Cpu className="w-3 h-3 text-cyan-400" />
                                  {c.target.harness} · {c.target.model} ({c.target.think_effort})
                                </span>
                              )}
                            </div>

                            <div className="flex flex-wrap items-center gap-1.5">
                              {c.prUrl && (
                                <a
                                  href={c.prUrl}
                                  target="_blank"
                                  rel="noreferrer"
                                  className="inline-flex items-center gap-1 text-[11px] font-medium px-2.5 py-1 rounded-lg bg-emerald-500/15 hover:bg-emerald-500/25 text-emerald-300 border border-emerald-500/35 transition"
                                >
                                  <GitPullRequest className="w-3 h-3" />
                                  {c.prLabel || '打开 PR'}
                                  <ExternalLink className="w-2.5 h-2.5" />
                                </a>
                              )}
                              {c.derivedStatus === 'ready_review' && c.isOpen && (
                                <button
                                  type="button"
                                  onClick={() => handleQuickAcknowledge(c)}
                                  className="inline-flex items-center gap-1 text-[11px] font-medium px-2.5 py-1 rounded-lg bg-indigo-500/15 hover:bg-indigo-500/25 text-indigo-300 border border-indigo-500/30 transition cursor-pointer"
                                >
                                  <CheckCircle2 className="w-3 h-3" />
                                  确认已知
                                </button>
                              )}
                            </div>
                          </div>
                        )}

                        {/* Slack Thread Action Footer */}
                        <div className="flex flex-wrap items-center justify-between gap-2 pt-0.5">
                          <div className="flex items-center gap-2">
                            <button
                              type="button"
                              onClick={() => openThread(c, 'thread', true)}
                              className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-lg hover:bg-indigo-500/15 text-[11px] font-medium text-indigo-300 transition cursor-pointer"
                            >
                              <span className="flex -space-x-1">
                                <span className="w-4 h-4 rounded bg-slate-700 border border-slate-900 flex items-center justify-center">
                                  <User className="w-2.5 h-2.5 text-sky-300" />
                                </span>
                                <span className="w-4 h-4 rounded bg-indigo-600 border border-slate-900 flex items-center justify-center">
                                  <Bot className="w-2.5 h-2.5 text-white" />
                                </span>
                              </span>
                              <span>
                                {c.replyCount > 0
                                  ? `${c.replyCount} 条话题回复 · 展开回复`
                                  : '在话题串中回复 (Reply in thread)'}
                              </span>
                            </button>

                            {c.ticket && (
                              <button
                                type="button"
                                onClick={() => openThread(c, 'steps')}
                                className="inline-flex items-center gap-1 px-2 py-1 rounded-lg hover:bg-slate-800 text-[11px] text-slate-400 hover:text-slate-200 transition cursor-pointer"
                              >
                                <Terminal className="w-3 h-3 text-emerald-400" />
                                实时执行步骤
                              </button>
                            )}
                          </div>

                          {c.ticket && c.isOpen && (
                            <div className="flex items-center gap-1.5">
                              <button
                                type="button"
                                onClick={() => onOpenSwitchModal(c.ticket!)}
                                className="inline-flex items-center gap-1 px-2 py-1 rounded-lg bg-slate-800/80 hover:bg-slate-700 text-[11px] text-slate-300 transition cursor-pointer"
                              >
                                <SlidersHorizontal className="w-3 h-3 text-indigo-400" />
                                切换模型
                              </button>
                              <button
                                type="button"
                                onClick={() => onStopTicket(c.ticket!.id)}
                                className="inline-flex items-center gap-1 px-2 py-1 rounded-lg bg-rose-500/10 hover:bg-rose-500/20 text-[11px] text-rose-300 border border-rose-500/25 transition cursor-pointer"
                                title="终止当前任务"
                              >
                                <Square className="w-2.5 h-2.5" />
                                终止
                              </button>
                            </div>
                          )}
                        </div>
                      </div>
                    </div>
                  </div>
                </article>
              );
            })
          )}
          <div ref={feedEndRef} />
        </div>

        {/* SLACK-STYLE MESSAGE COMPOSER */}
        <div className="shrink-0 px-4 sm:px-6 pb-4 pt-2 border-t border-slate-800/80 bg-[#0b0f18]">
          {/* Quick Starter Chips */}
          <div className="flex flex-wrap items-center gap-1.5 mb-2">
            {[
              { label: '🐛 排查 Bug 并提 PR', text: '帮我排查并修复这个问题，补齐测试并提交 PR：' },
              { label: '✨ 实现新功能', text: '请帮我设计并实现以下新功能：' },
              { label: '🔍 代码审查与重构', text: '请对这个模块做一次全面代码审查并给出重构方案：' },
              { label: '📋 整理今日进展', text: '帮我总结一下当前仓库最近的工单进展和待办事项。' },
            ].map((chip) => (
              <button
                key={chip.label}
                type="button"
                onClick={() =>
                  setComposerText((prev) => (prev.trim() ? `${prev}\n${chip.text}` : chip.text))
                }
                className="text-[11px] px-2.5 py-1 rounded-full bg-[#111624] hover:bg-indigo-500/15 text-slate-300 hover:text-indigo-200 border border-slate-800 hover:border-indigo-500/35 transition cursor-pointer"
              >
                {chip.label}
              </button>
            ))}
          </div>

          <div className="rounded-2xl border border-slate-700/80 bg-[#0d121d] focus-within:border-indigo-500/70 transition shadow-lg">
            {/* Optional Engine Picker Bar */}
            {showEnginePicker && (
              <div className="px-3.5 py-2 border-b border-slate-800/90 flex flex-wrap items-center gap-2 text-[11px] bg-[#0a0e17] rounded-t-2xl">
                <span className="text-slate-400">指定执行引擎:</span>
                <select
                  value={overrideHarness}
                  onChange={(e) => {
                    const nextH = e.target.value;
                    setOverrideHarness(nextH);
                    const cat = harnessCatalogs[nextH];
                    setOverrideModel(cat?.default_model || '');
                    setOverrideThink(cat?.default_think_effort || '');
                  }}
                  className="bg-[#070a10] border border-slate-700 rounded-lg px-2 py-1 text-slate-200 focus:outline-none"
                >
                  <option value="">自动路由 (Jev 默认)</option>
                  {(['codex', 'claude-code', 'pi-native', 'pi-rpc', 'opencode'] as HarnessKind[]).map(
                    (h) => (
                      <option key={h} value={h}>
                        {h}
                      </option>
                    )
                  )}
                </select>
                {overrideHarness && (
                  <>
                    <select
                      value={overrideModel}
                      onChange={(e) => setOverrideModel(e.target.value)}
                      className="bg-[#070a10] border border-slate-700 rounded-lg px-2 py-1 text-slate-200 max-w-[200px] focus:outline-none"
                    >
                      <option value="">默认模型</option>
                      {availableModelsForOverride.map((m) => (
                        <option key={m} value={m}>
                          {m}
                        </option>
                      ))}
                    </select>
                    <select
                      value={overrideThink}
                      onChange={(e) => setOverrideThink(e.target.value)}
                      className="bg-[#070a10] border border-slate-700 rounded-lg px-2 py-1 text-slate-200 focus:outline-none"
                    >
                      <option value="">默认推理深度</option>
                      {(['minimal', 'low', 'medium', 'high', 'xhigh'] as ThinkEffort[]).map((t) => (
                        <option key={t} value={t}>
                          think: {t}
                        </option>
                      ))}
                    </select>
                  </>
                )}
              </div>
            )}

            <textarea
              rows={2}
              value={composerText}
              onChange={(e) => setComposerText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  handleSendMainMessage();
                }
              }}
              placeholder={`给 #${shortRepoName(
                channel.kind === 'repo' ? channel.repo : composerRepo
              )} 的 @${botName} 发送需求或指令... (Enter 发送，Shift+Enter 换行)`}
              className="w-full px-4 pt-3 pb-2 bg-transparent text-[13px] text-slate-100 placeholder:text-slate-500 resize-none focus:outline-none"
            />

            <div className="px-3 pb-2.5 flex flex-wrap items-center justify-between gap-2">
              <div className="flex flex-wrap items-center gap-2">
                {channel.kind === 'repo' ? (
                  <span className="inline-flex items-center gap-1 text-[11px] font-mono px-2.5 py-1 rounded-lg bg-indigo-500/15 text-indigo-300 border border-indigo-500/30">
                    <GitBranch className="w-3 h-3" />
                    {channel.repo}
                  </span>
                ) : (
                  <div className="inline-flex items-center gap-1 text-[11px] bg-[#080b12] border border-slate-800 rounded-lg px-2 py-1">
                    <GitBranch className="w-3 h-3 text-indigo-400" />
                    <select
                      value={composerRepo}
                      onChange={(e) => setComposerRepo(e.target.value)}
                      className="bg-transparent text-slate-200 font-mono focus:outline-none cursor-pointer"
                    >
                      {configuredRepos.map((r) => (
                        <option key={r} value={r} className="bg-[#0d121d]">
                          {r}
                        </option>
                      ))}
                    </select>
                  </div>
                )}

                <button
                  type="button"
                  onClick={() => setShowEnginePicker((v) => !v)}
                  className={`inline-flex items-center gap-1 text-[11px] px-2.5 py-1 rounded-lg border transition cursor-pointer ${
                    overrideHarness
                      ? 'bg-cyan-500/15 text-cyan-300 border-cyan-500/35'
                      : 'bg-[#080b12] text-slate-400 hover:text-slate-200 border-slate-800'
                  }`}
                >
                  <Cpu className="w-3 h-3" />
                  {overrideHarness
                    ? `${overrideHarness}${overrideModel ? ` / ${overrideModel}` : ''}`
                    : '自动路由 (Jev)'}
                </button>
              </div>

              <button
                type="button"
                disabled={!composerText.trim() || composerSending}
                onClick={handleSendMainMessage}
                className="inline-flex items-center gap-1.5 px-3.5 py-1.5 rounded-xl bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 disabled:cursor-not-allowed text-xs font-semibold text-white shadow-md shadow-indigo-600/20 transition cursor-pointer"
              >
                <Send className="w-3.5 h-3.5" />
                {composerSending ? '派发中...' : '发送给奈克斯'}
              </button>
            </div>
          </div>
        </div>
      </div>

      {/* COLUMN 3: SLACK RIGHT THREAD & LIVE STEPS DRAWER */}
      {activeThread && (
        <aside className="fixed inset-y-0 right-0 z-40 w-full sm:w-[440px] lg:static lg:w-[430px] xl:w-[470px] shrink-0 border-l border-slate-800/90 bg-[#0a0d15] flex flex-col shadow-2xl lg:shadow-none">
          {/* Drawer Header */}
          <div className="px-4 py-3 border-b border-slate-800/90 flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="font-bold text-sm text-white">话题详情</span>
                {activeThread.ticket && (
                  <span className="text-[11px] font-mono px-1.5 py-0.5 rounded bg-indigo-500/20 text-indigo-300 border border-indigo-500/30">
                    {activeThread.ticket.id}
                  </span>
                )}
                <span
                  className={`text-[10px] px-2 py-0.5 rounded-full border ${
                    getStatusBadgeMeta(activeThread.derivedStatus).badgeClass
                  }`}
                >
                  {getStatusBadgeMeta(activeThread.derivedStatus).label}
                </span>
              </div>
              <p className="text-xs text-slate-400 truncate mt-0.5">{activeThread.title}</p>
            </div>
            <button
              type="button"
              onClick={() => setActiveThreadKey(null)}
              className="p-1.5 rounded-lg hover:bg-slate-800 text-slate-400 hover:text-white cursor-pointer"
            >
              <X className="w-4 h-4" />
            </button>
          </div>

          {/* Drawer Sub-tabs */}
          <div className="px-4 py-2 border-b border-slate-800/80 flex items-center justify-between gap-2 bg-[#0c101a]">
            <div className="inline-flex rounded-lg bg-[#070a10] p-0.5 border border-slate-800 text-xs">
              <button
                type="button"
                onClick={() => setDrawerTab('thread')}
                className={`inline-flex items-center gap-1.5 px-3 py-1 rounded-md font-medium transition cursor-pointer ${
                  drawerTab === 'thread'
                    ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/30'
                    : 'text-slate-400 hover:text-slate-200 border border-transparent'
                }`}
              >
                <MessageSquare className="w-3.5 h-3.5" />
                话题讨论串 ({activeThreadMessages.length})
              </button>
              <button
                type="button"
                disabled={!activeThread.ticket}
                onClick={() => setDrawerTab('steps')}
                className={`inline-flex items-center gap-1.5 px-3 py-1 rounded-md font-medium transition cursor-pointer disabled:opacity-40 ${
                  drawerTab === 'steps'
                    ? 'bg-emerald-500/20 text-emerald-300 border border-emerald-500/30'
                    : 'text-slate-400 hover:text-slate-200 border border-transparent'
                }`}
              >
                <Terminal className="w-3.5 h-3.5" />
                执行步骤
              </button>
            </div>

            {activeThread.ticket && activeThread.isOpen && (
              <button
                type="button"
                onClick={() => onOpenSwitchModal(activeThread.ticket!)}
                className="text-[11px] px-2 py-1 rounded-lg bg-slate-800 hover:bg-slate-700 text-slate-200 cursor-pointer"
              >
                切换模型
              </button>
            )}
          </div>

          {/* Drawer Body */}
          {drawerTab === 'thread' ? (
            <>
              <div className="flex-1 overflow-y-auto p-4 space-y-3">
                {activeThreadMessages.map((item) => (
                  <div
                    key={item.id}
                    className={`rounded-xl border p-3 ${
                      item.role === 'user'
                        ? 'border-slate-800 bg-[#0e131e]'
                        : item.role === 'thinking'
                        ? 'border-amber-500/25 bg-amber-500/5'
                        : 'border-indigo-500/30 bg-indigo-500/5'
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2 mb-1.5">
                      <div className="flex items-center gap-2">
                        {item.role === 'user' ? (
                          <User className="w-3.5 h-3.5 text-sky-400" />
                        ) : (
                          <Bot className="w-3.5 h-3.5 text-indigo-400" />
                        )}
                        <span className="text-xs font-semibold text-slate-100">{item.author}</span>
                        <span className="text-[10px] px-1.5 py-0.2 rounded bg-slate-800/90 text-slate-400">
                          {item.sourceLabel}
                        </span>
                      </div>
                      <span className="text-[10.5px] font-mono text-slate-500 flex items-center gap-1">
                        <Clock className="w-2.5 h-2.5" />
                        {formatExactShortTime(item.at)}
                      </span>
                    </div>

                    {item.text && (
                      <p className="text-xs text-slate-200 leading-relaxed whitespace-pre-wrap break-words">
                        {item.text}
                      </p>
                    )}

                    {item.question && (
                      <div className="mt-2 p-2.5 rounded-lg bg-amber-500/10 border border-amber-500/35 text-xs text-amber-200 whitespace-pre-wrap">
                        <span className="font-semibold text-amber-300">❓ 提问等待回复：</span>
                        {item.question}
                      </div>
                    )}

                    {item.prUrl && (
                      <a
                        href={item.prUrl}
                        target="_blank"
                        rel="noreferrer"
                        className="mt-2 inline-flex items-center gap-1 text-[11px] font-medium px-2.5 py-1 rounded-lg bg-emerald-500/15 text-emerald-300 border border-emerald-500/30"
                      >
                        <GitPullRequest className="w-3 h-3" />
                        打开 Pull Request
                        <ExternalLink className="w-2.5 h-2.5" />
                      </a>
                    )}
                  </div>
                ))}
                <div ref={threadEndRef} />
              </div>

              {/* Thread Reply Input */}
              <div className="p-3.5 border-t border-slate-800/90 bg-[#0c101a] space-y-2">
                <div className="text-[11px] text-slate-400 flex items-center justify-between">
                  <span>
                    {activeThread.ticket
                      ? `回复将直接投递给 ${activeThread.ticket.id} 并唤醒/续跑 Worker`
                      : '追加消息到当前话题上下文'}
                  </span>
                  <span className="font-mono text-slate-500">
                    #{shortRepoName(activeThread.repo)}
                  </span>
                </div>
                <div className="rounded-xl border border-slate-700/80 bg-[#070a10] focus-within:border-indigo-500/70 transition">
                  <textarea
                    ref={threadInputRef}
                    rows={3}
                    value={replyText}
                    onChange={(e) => setReplyText(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
                        e.preventDefault();
                        handleSendThreadReply();
                      }
                    }}
                    placeholder={
                      activeThread.question
                        ? '输入你的拍板决定，奈克斯会立刻继续执行...'
                        : '在话题中追问进展、补充上下文或提出修改意见...'
                    }
                    className="w-full px-3 pt-2.5 pb-1 bg-transparent text-xs text-slate-100 placeholder:text-slate-500 resize-none focus:outline-none"
                  />
                  <div className="px-2.5 pb-2 flex items-center justify-between">
                    <span className="text-[10px] text-slate-500">Enter 发送 · Shift+Enter 换行</span>
                    <button
                      type="button"
                      disabled={!replyText.trim() || replySending}
                      onClick={handleSendThreadReply}
                      className="inline-flex items-center gap-1 px-3 py-1 rounded-lg bg-indigo-600 hover:bg-indigo-500 disabled:opacity-40 text-xs font-semibold text-white transition cursor-pointer"
                    >
                      <Send className="w-3 h-3" />
                      {replySending ? '发送中...' : '回复话题'}
                    </button>
                  </div>
                </div>
              </div>
            </>
          ) : (
            <div className="flex-1 flex flex-col min-h-0">
              <div className="px-4 py-2.5 border-b border-slate-800/80 flex flex-wrap items-center justify-between gap-2">
                <div className="flex flex-wrap gap-1">
                  {[
                    { id: 'all', label: '全部' },
                    { id: 'thinking', label: '💭 思考' },
                    { id: 'tool_call', label: '🔧 工具' },
                    { id: 'tool_result', label: '📤 输出' },
                    { id: 'finish', label: '✅ 汇报' },
                  ].map((f) => (
                    <button
                      key={f.id}
                      type="button"
                      onClick={() => setTraceKindFilter(f.id)}
                      className={`px-2 py-0.5 rounded text-[11px] cursor-pointer ${
                        traceKindFilter === f.id
                          ? 'bg-indigo-500/20 text-indigo-300 border border-indigo-500/35'
                          : 'bg-slate-900 text-slate-400 border border-slate-800'
                      }`}
                    >
                      {f.label}
                    </button>
                  ))}
                </div>
                <button
                  type="button"
                  onClick={() => onSwitchToAdmin('trajectory')}
                  className="text-[11px] text-indigo-400 hover:text-indigo-300 cursor-pointer"
                >
                  控制面全屏 →
                </button>
              </div>

              <div className="flex-1 overflow-y-auto p-4">
                {traceLoading && !threadTrace ? (
                  <div className="text-xs text-slate-400 text-center py-8">正在加载执行步骤...</div>
                ) : (threadTrace?.steps || []).length === 0 ? (
                  <div className="text-xs text-slate-400 text-center py-8">暂无记录步骤</div>
                ) : (
                  (threadTrace?.steps || [])
                    .filter((s) => traceKindFilter === 'all' || s.kind === traceKindFilter)
                    .map((step) => (
                      <StepTimelineItem key={`${step.assignment_id}-${step.seq}`} step={step} />
                    ))
                )}
              </div>
            </div>
          )}
        </aside>
      )}
    </div>
  );
};

const SlackNavItem: React.FC<{
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  subLabel?: string;
  badge?: string;
  badgeTone?: 'amber' | 'emerald' | 'muted';
  livePulse?: boolean;
}> = ({ active, onClick, icon, label, subLabel, badge, badgeTone = 'muted', livePulse }) => (
  <button
    type="button"
    onClick={onClick}
    title={subLabel || label}
    className={`w-full flex items-center justify-between gap-2 px-2.5 py-1.5 rounded-lg text-xs transition cursor-pointer ${
      active
        ? 'bg-indigo-600/20 text-white font-semibold border border-indigo-500/30'
        : 'text-slate-300 hover:bg-slate-800/60 hover:text-white border border-transparent'
    }`}
  >
    <span className="flex items-center gap-2 min-w-0">
      {icon}
      <span className="truncate">{label}</span>
    </span>
    <span className="flex items-center gap-1.5 shrink-0">
      {livePulse && <span className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" />}
      {badge && (
        <span
          className={`text-[10px] font-mono px-1.5 py-0.2 rounded-full ${
            badgeTone === 'amber'
              ? 'bg-amber-500/20 text-amber-300 border border-amber-500/35'
              : badgeTone === 'emerald'
              ? 'bg-emerald-500/20 text-emerald-300 border border-emerald-500/35'
              : 'bg-slate-800 text-slate-400'
          }`}
        >
          {badge}
        </span>
      )}
    </span>
  </button>
);

function isTicketOpen(t: Ticket): boolean {
  if (t.status === 'closed') return false;
  return t.ended == null;
}

function deriveTicketStatus(t: Ticket): DerivedTicketStatus {
  if (!isTicketOpen(t)) {
    const outcome = t.ended?.outcome || t.resolution || '';
    if (t.ended?.by_person_stop || outcome === 'stopped') return 'stopped';
    if (outcome === 'dropped') return 'dropped';
    return 'completed';
  }

  if (t.delivery_error) return 'attention';

  const lastAsg = t.assignments?.[t.assignments.length - 1];
  const report = lastAsg?.latest_report;

  if (report) {
    if (report.question && report.question.trim()) return 'needs_decision';
    if (report.status === 'ready') return 'ready_review';
    if (report.status === 'blocked' || report.status === 'failed') {
      const humanArrivedSince = (t.inputs || []).some(
        (inp) => inp.from_person && inp.at > report.at
      );
      if (!humanArrivedSince) return 'blocked';
    }
  }

  if (!lastAsg || lastAsg.status === 'pending') return 'queued';
  if (lastAsg.status === 'ended' || report?.status === 'waiting') return 'idle';
  return 'running';
}

function getStatusBadgeMeta(status: DerivedTicketStatus) {
  switch (status) {
    case 'needs_decision':
      return {
        label: '等待你拍板',
        badgeClass: 'bg-amber-500/15 text-amber-300 border-amber-500/35',
        cardBorder: 'border-l-amber-400',
        dotPulse: true,
      };
    case 'blocked':
      return {
        label: '任务受阻 / 失败',
        badgeClass: 'bg-rose-500/15 text-rose-300 border-rose-500/35',
        cardBorder: 'border-l-rose-400',
        dotPulse: false,
      };
    case 'attention':
      return {
        label: '投递异常',
        badgeClass: 'bg-rose-500/15 text-rose-300 border-rose-500/35',
        cardBorder: 'border-l-rose-400',
        dotPulse: false,
      };
    case 'ready_review':
      return {
        label: '已就绪 · 待验收',
        badgeClass: 'bg-emerald-500/15 text-emerald-300 border-emerald-500/35',
        cardBorder: 'border-l-emerald-400',
        dotPulse: false,
      };
    case 'running':
      return {
        label: '后台执行中',
        badgeClass: 'bg-amber-500/15 text-amber-300 border-amber-500/35',
        cardBorder: 'border-l-amber-400',
        dotPulse: true,
      };
    case 'queued':
      return {
        label: '排队调度中',
        badgeClass: 'bg-indigo-500/15 text-indigo-300 border-indigo-500/35',
        cardBorder: 'border-l-indigo-400',
        dotPulse: true,
      };
    case 'idle':
      return {
        label: '等待续跑',
        badgeClass: 'bg-slate-800 text-slate-300 border-slate-700',
        cardBorder: 'border-l-slate-500',
        dotPulse: false,
      };
    case 'stopped':
      return {
        label: '已手动终止',
        badgeClass: 'bg-slate-800 text-slate-400 border-slate-700',
        cardBorder: 'border-l-slate-600',
        dotPulse: false,
      };
    case 'dropped':
      return {
        label: '已忽略',
        badgeClass: 'bg-slate-800 text-slate-400 border-slate-700',
        cardBorder: 'border-l-slate-600',
        dotPulse: false,
      };
    default:
      return {
        label: '已完成交付',
        badgeClass: 'bg-emerald-500/10 text-emerald-300 border-emerald-500/25',
        cardBorder: 'border-l-emerald-500',
        dotPulse: false,
      };
  }
}

function classifySource(
  threadKey: string,
  sourceId?: string,
  sourceKind?: string
): { kind: ConversationThread['sourceKind']; label: string } {
  if (
    sourceId === 'web-workspace' ||
    threadKey.startsWith('web:') ||
    threadKey.startsWith('admin:')
  ) {
    return { kind: 'web', label: 'Web 工作台' };
  }
  if (sourceKind === 'feishu' || threadKey.startsWith('feishu:')) {
    return { kind: 'feishu', label: '飞书群聊' };
  }
  if (sourceKind === 'dingtalk' || threadKey.startsWith('dingtalk:')) {
    return { kind: 'dingtalk', label: '钉钉群聊' };
  }
  if (sourceKind === 'github' || threadKey.startsWith('github:')) {
    return { kind: 'github', label: 'GitHub' };
  }
  if (sourceKind === 'gws' || threadKey.startsWith('gws:')) {
    return { kind: 'gws', label: 'Google Workspace' };
  }
  return { kind: 'slack', label: 'Slack' };
}

function getSourceBadgeClass(kind: ConversationThread['sourceKind']): string {
  switch (kind) {
    case 'feishu':
      return 'bg-sky-500/15 text-sky-300 border-sky-500/30';
    case 'dingtalk':
      return 'bg-cyan-500/15 text-cyan-300 border-cyan-500/30';
    case 'github':
      return 'bg-purple-500/15 text-purple-300 border-purple-500/30';
    case 'web':
      return 'bg-indigo-500/15 text-indigo-300 border-indigo-500/30';
    default:
      return 'bg-slate-800 text-slate-300 border-slate-700';
  }
}

function formatActorName(actorId: string, sourceKind: ConversationThread['sourceKind']): string {
  if (actorId === 'xdlkc' || actorId === 'admin' || sourceKind === 'web') {
    return actorId === 'xdlkc' ? '你 (@xdlkc)' : `你 (${actorId})`;
  }
  if (actorId.startsWith('ou_')) {
    return `飞书成员 (${actorId.slice(0, 7)})`;
  }
  return actorId;
}

function shortRepoName(repo: string): string {
  return repo.includes('/') ? repo.split('/').slice(-1)[0] : repo;
}

function inferRepoFromThreadKey(threadKey: string): string | null {
  if (threadKey.startsWith('web:repo:')) {
    const rest = threadKey.slice('web:repo:'.length);
    const parts = rest.split(':');
    if (parts[0] && parts[0].includes('/')) return parts[0];
  }
  const ghMatch = threadKey.match(/^github:(?:issue|pr):([^#]+)#\d+/);
  if (ghMatch) return ghMatch[1];
  return null;
}

function extractPrLink(
  ticket: Ticket,
  summary?: string
): { prUrl: string | null; prLabel: string | null } {
  const lastAsg = ticket.assignments?.[ticket.assignments.length - 1];
  const prFollow = ticket.follows?.find((f) => f.thread_key.startsWith('github:pr:'));
  const followUrl = prFollow
    ? `https://github.com/${prFollow.thread_key
        .replace('github:pr:', '')
        .replace('#', '/pull/')}`
    : null;
  const summaryMatch = summary?.match(/https:\/\/github\.com\/[^\s)]+\/pull\/\d+/);
  const prUrl = lastAsg?.latest_report?.pull_request_url || followUrl || summaryMatch?.[0] || null;
  const prLabel = prFollow ? prFollow.thread_key.replace('github:pr:', '') : null;
  return { prUrl, prLabel };
}

function formatRelativeTime(iso?: string): string {
  if (!iso) return '';
  const ts = new Date(iso).getTime();
  if (Number.isNaN(ts)) return iso;
  const diffSec = Math.floor((Date.now() - ts) / 1000);
  if (diffSec < 60 && diffSec >= 0) return '刚刚';
  if (diffSec < 3600 && diffSec >= 0) return `${Math.floor(diffSec / 60)} 分钟前`;
  if (diffSec < 86400 && diffSec >= 0) return `${Math.floor(diffSec / 3600)} 小时前`;
  return formatExactShortTime(iso);
}

function formatExactShortTime(iso?: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
}
