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

export async function dispatchSimulatedChat(text: string): Promise<void> {
  await fetch('/api/chat/simulate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ text }),
  });
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
