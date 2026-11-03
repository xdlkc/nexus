import React, { useState } from 'react';
import {
  Brain,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Cpu,
  AlertTriangle,
  Play,
  Terminal,
  FileCode2,
  MessageSquare,
} from 'lucide-react';
import type { WorkerTraceStep } from '../types/nexus';

export const StepTimelineItem: React.FC<{
  step: WorkerTraceStep;
  defaultExpanded?: boolean;
}> = ({ step, defaultExpanded = false }) => {
  const isLong = (step.detail || '').length > 260 || (step.detail || '').split('\n').length > 6;
  const [expanded, setExpanded] = useState(defaultExpanded || !isLong);

  const meta = getStepMeta(step.kind);

  return (
    <div className="relative pl-7 pb-4 last:pb-0 group">
      {/* Vertical connector */}
      <div className="absolute left-[11px] top-6 bottom-0 w-px bg-slate-800/90 group-last:hidden" />
      {/* Node icon */}
      <div
        className={`absolute left-0 top-1 w-6 h-6 rounded-full flex items-center justify-center border ${meta.ringClass} ${meta.bgClass}`}
      >
        {meta.icon}
      </div>

      <div className="rounded-xl border border-slate-800/80 bg-[#0d111a]/90 hover:border-slate-700/80 transition-colors p-3.5 shadow-sm">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex items-center gap-2 min-w-0">
            <span className="text-[11px] font-mono px-1.5 py-0.5 rounded bg-slate-800/90 text-slate-400">
              #{step.seq}
            </span>
            <span
              className={`text-[11px] font-medium px-2 py-0.5 rounded-full border ${meta.badgeClass}`}
            >
              {meta.label}
            </span>
            <span className="text-xs font-medium text-slate-200 truncate">
              {step.title}
            </span>
          </div>

          <div className="flex items-center gap-2 text-[11px] text-slate-500 font-mono shrink-0">
            <span>{step.assignment_id}</span>
            <span>·</span>
            <span>{formatTime(step.at)}</span>
            {isLong && (
              <button
                type="button"
                onClick={() => setExpanded((v) => !v)}
                className="inline-flex items-center gap-0.5 px-1.5 py-0.5 rounded bg-slate-800/70 hover:bg-slate-700/70 text-slate-300 transition"
              >
                {expanded ? (
                  <>
                    <ChevronDown className="w-3 h-3" /> 收起
                  </>
                ) : (
                  <>
                    <ChevronRight className="w-3 h-3" /> 展开 ({step.detail.length} chars)
                  </>
                )}
              </button>
            )}
          </div>
        </div>

        {step.detail && (
          <div className="mt-2.5">
            {step.kind === 'thinking' ? (
              <div className="text-xs leading-relaxed text-amber-100/90 bg-amber-500/5 border border-amber-500/20 rounded-lg px-3 py-2 whitespace-pre-wrap">
                {expanded ? step.detail : step.detail.slice(0, 260) + (isLong ? ' …' : '')}
              </div>
            ) : (
              <pre className="text-[11.5px] leading-relaxed font-mono text-slate-300 bg-[#070a10] border border-slate-800/90 rounded-lg p-3 overflow-x-auto whitespace-pre-wrap break-words max-h-96 overflow-y-auto">
                {expanded ? step.detail : step.detail.slice(0, 260) + (isLong ? '\n... (点击右上角展开完整输出)' : '')}
              </pre>
            )}
          </div>
        )}
      </div>
    </div>
  );
};

function formatTime(iso?: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleTimeString('zh-CN', { hour12: false });
}

function getStepMeta(kind: string) {
  switch (kind) {
    case 'launch':
      return {
        label: 'LAUNCH',
        icon: <Play className="w-3 h-3 text-indigo-400" />,
        ringClass: 'border-indigo-500/40',
        bgClass: 'bg-indigo-500/10',
        badgeClass: 'bg-indigo-500/10 text-indigo-300 border-indigo-500/30',
      };
    case 'thinking':
      return {
        label: 'THINKING 💭',
        icon: <Brain className="w-3 h-3 text-amber-400" />,
        ringClass: 'border-amber-500/40',
        bgClass: 'bg-amber-500/10',
        badgeClass: 'bg-amber-500/10 text-amber-300 border-amber-500/30',
      };
    case 'tool_call':
      return {
        label: 'EXEC / TOOL',
        icon: <Terminal className="w-3 h-3 text-cyan-400" />,
        ringClass: 'border-cyan-500/40',
        bgClass: 'bg-cyan-500/10',
        badgeClass: 'bg-cyan-500/10 text-cyan-300 border-cyan-500/30',
      };
    case 'tool_result':
      return {
        label: 'STDOUT / EXIT',
        icon: <FileCode2 className="w-3 h-3 text-emerald-400" />,
        ringClass: 'border-emerald-500/40',
        bgClass: 'bg-emerald-500/10',
        badgeClass: 'bg-emerald-500/10 text-emerald-300 border-emerald-500/30',
      };
    case 'usage':
      return {
        label: 'TOKENS',
        icon: <Cpu className="w-3 h-3 text-purple-400" />,
        ringClass: 'border-purple-500/40',
        bgClass: 'bg-purple-500/10',
        badgeClass: 'bg-purple-500/10 text-purple-300 border-purple-500/30',
      };
    case 'finish':
      return {
        label: 'READY',
        icon: <CheckCircle2 className="w-3 h-3 text-emerald-400" />,
        ringClass: 'border-emerald-500/50',
        bgClass: 'bg-emerald-500/15',
        badgeClass: 'bg-emerald-500/15 text-emerald-300 border-emerald-500/40',
      };
    case 'error':
      return {
        label: 'ERROR',
        icon: <AlertTriangle className="w-3 h-3 text-rose-400" />,
        ringClass: 'border-rose-500/50',
        bgClass: 'bg-rose-500/15',
        badgeClass: 'bg-rose-500/15 text-rose-300 border-rose-500/40',
      };
    default:
      return {
        label: kind.toUpperCase(),
        icon: <MessageSquare className="w-3 h-3 text-slate-400" />,
        ringClass: 'border-slate-600',
        bgClass: 'bg-slate-800',
        badgeClass: 'bg-slate-800 text-slate-300 border-slate-700',
      };
  }
}
