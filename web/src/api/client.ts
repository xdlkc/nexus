import type {
  AcceptMode,
  IdentityStatusResponse,
  NexusOverview,
  WorkerModelsResponse,
  WorkerTraceResponse,
} from '../types/nexus';

export async function getOverview(): Promise<NexusOverview> {
  const res = await fetch('/api/overview');
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

export async function getWorkerTrace(ticketId?: string): Promise<WorkerTraceResponse> {
  const q = ticketId ? `?ticket_id=${encodeURIComponent(ticketId)}` : '';
  const res = await fetch(`/api/worker/trace${q}`);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

export async function getIdentityStatus(): Promise<IdentityStatusResponse> {
  const res = await fetch('/api/identity/status');
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.json();
}

export async function getWorkerModels(): Promise<WorkerModelsResponse> {
  const res = await fetch('/api/worker/models');
  const data = await res.json();
  if (!res.ok) throw new Error(data.error || 'Failed to load harness catalogs');
  return {
    models: data.models || [],
    harnesses: data.harnesses || {},
  };
}

export async function setAcceptMode(accept_mode: AcceptMode): Promise<void> {
  await fetch('/api/settings/accept-mode', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ accept_mode }),
  });
}

export async function dispatchSimulatedChat(
  text: string,
  options?: { thread_key?: string; repo?: string }
): Promise<void> {
  const res = await fetch('/api/chat/simulate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text, ...options }),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
}

export async function sendWorkspaceMessage(params: {
  text: string;
  threadKey?: string;
  repo?: string;
  actorId?: string;
  inlineTarget?: {
    harness?: string;
    provider?: string;
    model?: string;
    think_effort?: string;
  } | null;
}): Promise<{ ok: boolean; disposition?: string; thread_key: string }> {
  const ts = Date.now();
  const rand = Math.random().toString(36).slice(2, 6);
  const msgId = `web_${ts}_${rand}`;
  const threadKey =
    params.threadKey && params.threadKey.trim()
      ? params.threadKey.trim()
      : params.repo && params.repo.trim()
      ? `web:repo:${params.repo.trim()}:${msgId}`
      : `web:general:${msgId}`;

  const inlineTarget =
    params.inlineTarget &&
    (params.inlineTarget.harness ||
      params.inlineTarget.provider ||
      params.inlineTarget.model ||
      params.inlineTarget.think_effort)
      ? {
          harness: params.inlineTarget.harness || null,
          provider: params.inlineTarget.provider || null,
          model: params.inlineTarget.model || null,
          think_effort: params.inlineTarget.think_effort || null,
        }
      : null;

  const trigger = {
    id: `web-trig-${ts}-${rand}`,
    source_id: 'web-workspace',
    source_kind: 'slack',
    external_id: msgId,
    thread_key: threadKey,
    actor_id: params.actorId?.trim() || 'xdlkc',
    actor_is_bot: false,
    addressed_agent: true,
    text: params.text,
    reply_to_trigger_id: null,
    inline_target: inlineTarget,
    reply_channel: {
      kind: 'generic_im',
      source_id: 'web-workspace',
      source_kind: 'slack',
      conversation_id: params.repo?.trim() || 'general',
      thread_id: threadKey,
    },
    at: new Date(ts).toISOString(),
    meta: params.repo?.trim() ? { repo: params.repo.trim(), origin: 'web-workspace' } : { origin: 'web-workspace' },
  };

  const res = await fetch('/api/triggers', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(trigger),
  });
  const data = await res.json();
  if (!res.ok || data.ok === false) {
    throw new Error(data.error || `HTTP ${res.status}`);
  }
  return { ...data, thread_key: threadKey };
}

export async function stopTicket(ticket_id: string, reason = '管理控制台手动终止工单'): Promise<void> {
  await fetch('/api/tickets/stop', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ ticket_id, reason }),
  });
}

export async function switchWorkerTarget(payload: {
  ticket_id: string;
  harness: string;
  provider: string;
  model: string;
  think_effort: string;
  reason: string;
}): Promise<void> {
  await fetch('/api/worker/switch-target', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
}

export async function saveMemoryContent(content: string): Promise<void> {
  await fetch('/api/memory', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ content }),
  });
}

export async function updateSystemConfig(payload: Record<string, unknown>): Promise<void> {
  await fetch('/api/config', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
}

export async function testJevTriage(text: string): Promise<Record<string, unknown>> {
  const res = await fetch('/api/jev/test', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
  return res.json();
}
