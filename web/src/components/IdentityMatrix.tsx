import React from 'react';
import {
  ShieldCheck,
  GitBranch,
  GitFork,
  MessageSquare,
  FolderLock,
  RefreshCw,
  CheckCircle2,
  AlertCircle,
} from 'lucide-react';
import type { IdentityStatusResponse } from '../types/nexus';

export const IdentityMatrix: React.FC<{
  status: IdentityStatusResponse | null;
  loading: boolean;
  onRefresh: () => void;
}> = ({ status, loading, onRefresh }) => {
  if (!status) {
    return (
      <div className="rounded-2xl border border-slate-800/80 bg-[#0c1018] p-5 text-sm text-slate-400 flex items-center justify-between">
        <span>正在探测宿主机本地 CLI 与奈克斯独立工作空间隔离状态...</span>
        <button
          onClick={onRefresh}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-slate-800 hover:bg-slate-700 text-xs text-slate-200 transition"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? 'animate-spin' : ''}`} />
          刷新探测
        </button>
      </div>
    );
  }

  return (
    <div className="rounded-2xl border border-slate-800/90 bg-gradient-to-b from-[#0e131f] to-[#0a0d15] p-5 shadow-lg">
      <div className="flex flex-wrap items-center justify-between gap-3 mb-4">
        <div className="flex items-center gap-2.5">
          <div className="w-9 h-9 rounded-xl bg-emerald-500/10 border border-emerald-500/30 flex items-center justify-center">
            <ShieldCheck className="w-5 h-5 text-emerald-400" />
          </div>
          <div>
            <h3 className="text-sm font-semibold text-slate-100 flex items-center gap-2">
              宿主机本地 CLI vs 奈克斯独立工作空间 (`agent-home` + `worker-bin`)
              <span className="text-[11px] font-medium px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30">
                0700 沙箱已激活
              </span>
            </h3>
            <p className="text-xs text-slate-400 mt-0.5">
              独立目录: <code className="text-indigo-300">{status.workspace.agent_home}</code> · 垫片目录:{' '}
              <code className="text-indigo-300">{status.workspace.worker_bin}</code>
            </p>
          </div>
        </div>

        <button
          onClick={onRefresh}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-slate-700/80 bg-slate-800/70 hover:bg-slate-700/70 text-xs font-medium text-slate-200 transition"
        >
          <RefreshCw className={`w-3.5 h-3.5 ${loading ? 'animate-spin' : ''}`} />
          重新探测身份
        </button>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-4 gap-3.5">
        {/* GitHub Card */}
        <div className="rounded-xl border border-slate-800/90 bg-[#080b11]/90 p-3.5 flex flex-col justify-between">
          <div>
            <div className="flex items-center justify-between mb-2">
              <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-slate-200">
                <GitFork className="w-4 h-4 text-indigo-400" /> GitHub CLI (`gh-agent`)
              </span>
              {status.github.isolated ? (
                <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30">
                  <CheckCircle2 className="w-3 h-3" /> 已隔离
                </span>
              ) : (
                <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-amber-500/10 text-amber-300 border border-amber-500/30">
                  <AlertCircle className="w-3 h-3" /> 共享本机
                </span>
              )}
            </div>
            <div className="space-y-1.5 text-xs mt-2">
              <div className="flex items-center justify-between text-slate-400">
                <span>宿主机 `~/.config/gh`:</span>
                <code className="text-slate-300 font-semibold">{status.github.host_login}</code>
              </div>
              <div className="flex items-center justify-between text-slate-200">
                <span>奈克斯独立账号:</span>
                <code className="text-emerald-300 font-semibold bg-emerald-500/10 px-1.5 py-0.5 rounded">
                  {status.github.agent_login}
                </code>
              </div>
            </div>
          </div>
          <div className="mt-3 pt-2.5 border-t border-slate-800/80 text-[11px] text-slate-500 font-mono truncate" title={status.github.login_cmd}>
            {status.github.login_cmd}
          </div>
        </div>

        {/* Git Commit Identity Card */}
        <div className="rounded-xl border border-slate-800/90 bg-[#080b11]/90 p-3.5 flex flex-col justify-between">
          <div>
            <div className="flex items-center justify-between mb-2">
              <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-slate-200">
                <GitBranch className="w-4 h-4 text-cyan-400" /> Git Commit & HTTPS 凭据
              </span>
              <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30">
                <CheckCircle2 className="w-3 h-3" /> 已隔离
              </span>
            </div>
            <div className="space-y-1.5 text-xs mt-2">
              <div className="text-slate-400 truncate">
                宿主机: <code className="text-slate-300">{status.git.host_name} &lt;{status.git.host_email}&gt;</code>
              </div>
              <div className="text-slate-200 truncate" title={`${status.git.agent_name} <${status.git.agent_email}>`}>
                奈克斯: <code className="text-cyan-300">{status.git.agent_name}</code>
              </div>
              <div className="text-[11px] text-slate-400 font-mono truncate">
                {status.git.agent_email}
              </div>
            </div>
          </div>
          <div className="mt-3 pt-2.5 border-t border-slate-800/80 text-[11px] text-slate-500 font-mono truncate">
            GIT_CONFIG_GLOBAL=.nexus/worker-gitconfig
          </div>
        </div>

        {/* Feishu Bot Identity Card */}
        <div className="rounded-xl border border-slate-800/90 bg-[#080b11]/90 p-3.5 flex flex-col justify-between">
          <div>
            <div className="flex items-center justify-between mb-2">
              <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-slate-200">
                <MessageSquare className="w-4 h-4 text-blue-400" /> 飞书 CLI (`lark-bot`)
              </span>
              <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30">
                <CheckCircle2 className="w-3 h-3" /> 已隔离
              </span>
            </div>
            <div className="space-y-1.5 text-xs mt-2">
              <div className="flex items-center justify-between text-slate-400">
                <span>机器人名称:</span>
                <span className="text-slate-200 font-medium">{status.feishu.bot_name}</span>
              </div>
              <div className="flex items-center justify-between text-slate-400">
                <span>独立 Profile:</span>
                <code className="text-blue-300">{status.feishu.profile || 'default'}</code>
              </div>
            </div>
          </div>
          <div className="mt-3 pt-2.5 border-t border-slate-800/80 text-[11px] text-slate-500 font-mono truncate">
            {status.feishu.bot_open_id || 'ou_83c7badc63c451f57306c05861ff858d'}
          </div>
        </div>

        {/* DingTalk / GWS Sandbox Card */}
        <div className="rounded-xl border border-slate-800/90 bg-[#080b11]/90 p-3.5 flex flex-col justify-between">
          <div>
            <div className="flex items-center justify-between mb-2">
              <span className="inline-flex items-center gap-1.5 text-xs font-semibold text-slate-200">
                <FolderLock className="w-4 h-4 text-purple-400" /> 钉钉 (`dws`) &amp; GWS
              </span>
              <span className="inline-flex items-center gap-1 text-[11px] px-2 py-0.5 rounded-full bg-emerald-500/10 text-emerald-300 border border-emerald-500/30">
                <CheckCircle2 className="w-3 h-3" /> 已就绪
              </span>
            </div>
            <div className="space-y-1.5 text-xs mt-2">
              <div className="flex items-center justify-between text-slate-400">
                <span>钉钉 RobotCode:</span>
                <code className="text-emerald-300 font-semibold">
                  {status.dws.robot_code || 'dingztmnesaskuiyqfup'}
                </code>
              </div>
              <div className="flex items-center justify-between text-slate-400">
                <span>钉钉组织 Profile:</span>
                <code className="text-purple-300 truncate max-w-[140px]" title={status.dws.profile || ''}>
                  {status.dws.profile || 'Lkc (已登录)'}
                </code>
              </div>
            </div>
          </div>
          <div className="mt-3 pt-2.5 border-t border-slate-800/80 text-[11px] text-slate-500 font-mono truncate">
            群 ID: {(status.dws.chat_ids || []).join(', ') || 'cidrlrbHu3h3bSaJWi6oDzOKA=='}
          </div>
        </div>
      </div>
    </div>
  );
};
