//! Polymorphic Worker Harness drivers (`pi-native`, `pi-rpc`, `claude-code`, `codex`, `opencode`).

use crate::core::model::{ExecutionTarget, HarnessKind, ReportStatus, ThinkEffort};
use crate::router::llm::{LlmClient, LlmCompletionRequest, LlmMessage, LlmToolSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

/// Execution phase of a `pi-native` Worker, used for automatic `Planning -> Implementation`
/// `ThinkEffort` downgrade after the first file edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerPhase {
    Planning,
    Implementation,
}

/// Downgrades `ThinkEffort` by one tier when transitioning from `Planning` to `Implementation`.
pub fn downgrade_for_implementation(effort: ThinkEffort) -> ThinkEffort {
    match effort {
        ThinkEffort::Xhigh => ThinkEffort::High,
        ThinkEffort::High => ThinkEffort::Medium,
        ThinkEffort::Medium => ThinkEffort::Low,
        ThinkEffort::Low => ThinkEffort::Minimal,
        ThinkEffort::Minimal => ThinkEffort::Minimal,
        ThinkEffort::Off => ThinkEffort::Off,
    }
}

/// Maps `ThinkEffort` to Claude Code's `--effort` CLI flag.
pub fn claude_code_effort_flag(effort: ThinkEffort) -> &'static str {
    match effort {
        ThinkEffort::Off | ThinkEffort::Minimal | ThinkEffort::Low => "low",
        ThinkEffort::Medium => "medium",
        ThinkEffort::High => "high",
        ThinkEffort::Xhigh => "max",
    }
}

/// Maps `ThinkEffort` to Codex's `model_reasoning_effort` config value.
pub fn codex_reasoning_effort(effort: ThinkEffort) -> &'static str {
    match effort {
        ThinkEffort::Off | ThinkEffort::Minimal => "minimal",
        ThinkEffort::Low => "low",
        ThinkEffort::Medium => "medium",
        ThinkEffort::High => "high",
        ThinkEffort::Xhigh => "xhigh",
    }
}

/// Returns `true` if an environment variable key looks like a raw peripheral credential that must be scrubbed
/// from subprocess worker environments (workers use `<state_dir>/worker-bin` shims for GitHub/Feishu/GWS/DWS instead).
pub fn is_sensitive_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    // Preserve NEXUS_* internal runtime variables and LLM provider keys required by CLI harnesses.
    if upper.starts_with("NEXUS_")
        || upper.starts_with("ANTHROPIC_")
        || upper.starts_with("OPENAI_")
        || upper.starts_with("OPENROUTER_")
        || upper.starts_with("DEEPSEEK_")
        || upper.starts_with("GEMINI_")
    {
        return false;
    }
    upper.contains("SECRET")
        || upper.contains("TOKEN")
        || upper.contains("PASSWORD")
        || upper.ends_with("_KEY")
        || upper.contains("PRIVATE_KEY")
}

/// Runtime context passed to a Worker Harness when launching an Assignment.
#[derive(Debug, Clone)]
pub struct WorkerLaunchContext {
    pub ticket_id: String,
    pub assignment_id: String,
    pub target: ExecutionTarget,
    pub worktree_dir: PathBuf,
    pub state_dir: PathBuf,
    pub api_url: String,
    pub prompt: String,
    pub max_turns: usize,
    /// Optional real-time channel for streaming intermediate thinking / progress notes (`💭 ...`) to IM while working.
    pub progress_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

/// Result returned when a Worker Assignment finishes a turn or exits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerExecutionResult {
    pub status: ReportStatus,
    pub summary: String,
    pub question: Option<String>,
    pub pull_request_thread_key: Option<String>,
    pub pull_request_url: Option<String>,
    pub raw_output: String,
}

/// A single chronological event in a Worker Assignment's execution trajectory (`<state_dir>/traces/<assignment_id>.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerTraceStep {
    pub seq: usize,
    pub at: String,
    pub assignment_id: String,
    pub ticket_id: String,
    /// Step category: `"launch"`, `"thinking"`, `"tool_call"`, `"tool_result"`, `"usage"`, `"finish"`, `"error"`
    pub kind: String,
    pub title: String,
    pub detail: String,
}

/// Appends a single `WorkerTraceStep` to `<state_dir>/traces/<assignment_id>.jsonl` in real time.
pub fn append_worker_trace_step(
    state_dir: &Path,
    seq: &mut usize,
    assignment_id: &str,
    ticket_id: &str,
    kind: &str,
    title: impl Into<String>,
    detail: impl Into<String>,
) {
    *seq += 1;
    let step = WorkerTraceStep {
        seq: *seq,
        at: chrono::Utc::now().to_rfc3339(),
        assignment_id: assignment_id.to_owned(),
        ticket_id: ticket_id.to_owned(),
        kind: kind.to_owned(),
        title: title.into(),
        detail: detail.into(),
    };
    let traces_dir = state_dir.join("traces");
    let _ = std::fs::create_dir_all(&traces_dir);
    let path = traces_dir.join(format!("{assignment_id}.jsonl"));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        && let Ok(line) = serde_json::to_string(&step)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Reads all persisted `WorkerTraceStep`s from `<state_dir>/traces/<assignment_id>.jsonl`.
pub fn read_worker_trace_steps(state_dir: &Path, assignment_id: &str) -> Vec<WorkerTraceStep> {
    let path = state_dir.join("traces").join(format!("{assignment_id}.jsonl"));
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<WorkerTraceStep>(line).ok())
        .collect()
}

/// Command line specification constructed for an external CLI Worker Harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliHarnessSpec {
    pub program: String,
    pub args: Vec<String>,
    pub stdin_lines: Vec<String>,
    pub env_overrides: BTreeMap<String, String>,
}

/// Builds the exact CLI invocation and stdin payload for any external CLI Harness
/// (`pi-rpc`, `claude-code`, `codex`, `opencode`).
pub fn build_cli_harness_spec(ctx: &WorkerLaunchContext) -> CliHarnessSpec {
    let abs_state_dir =
        std::fs::canonicalize(&ctx.state_dir).unwrap_or_else(|_| ctx.state_dir.clone());
    let worker_bin = abs_state_dir.join("worker-bin");
    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", worker_bin.display(), current_path);
    let gitconfig = abs_state_dir.join("worker-gitconfig");

    let env_overrides = BTreeMap::from([
        ("PATH".to_owned(), new_path),
        (
            "GIT_CONFIG_GLOBAL".to_owned(),
            gitconfig.to_string_lossy().into_owned(),
        ),
        ("NEXUS_TICKET_ID".to_owned(), ctx.ticket_id.clone()),
        ("NEXUS_ASSIGNMENT_ID".to_owned(), ctx.assignment_id.clone()),
        ("NEXUS_API_URL".to_owned(), ctx.api_url.clone()),
    ]);

    match ctx.target.harness {
        HarnessKind::PiRpc => {
            let set_think = serde_json::json!({
                "id": "req-1",
                "type": "set_thinking_level",
                "level": ctx.target.think_effort.as_str(),
            })
            .to_string();
            let prompt_cmd = serde_json::json!({
                "id": "req-2",
                "type": "prompt",
                "message": ctx.prompt,
            })
            .to_string();

            CliHarnessSpec {
                program: "pi".to_owned(),
                args: vec![
                    "--mode".to_owned(),
                    "rpc".to_owned(),
                    "--provider".to_owned(),
                    ctx.target.provider.clone(),
                    "--model".to_owned(),
                    ctx.target.model.clone(),
                ],
                stdin_lines: vec![set_think, prompt_cmd],
                env_overrides,
            }
        }
        HarnessKind::ClaudeCode => CliHarnessSpec {
            program: "claude".to_owned(),
            args: vec![
                "-p".to_owned(),
                "--verbose".to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--permission-mode".to_owned(),
                "bypassPermissions".to_owned(),
                "--model".to_owned(),
                ctx.target.model.clone(),
                "--effort".to_owned(),
                claude_code_effort_flag(ctx.target.think_effort).to_owned(),
                ctx.prompt.clone(),
            ],
            stdin_lines: Vec::new(),
            env_overrides,
        },
        HarnessKind::Codex => CliHarnessSpec {
            program: "codex".to_owned(),
            args: vec![
                "exec".to_owned(),
                "--skip-git-repo-check".to_owned(),
                "--json".to_owned(),
                "--dangerously-bypass-approvals-and-sandbox".to_owned(),
                "-m".to_owned(),
                ctx.target.model.clone(),
                "-c".to_owned(),
                format!(
                    "model_reasoning_effort=\"{}\"",
                    codex_reasoning_effort(ctx.target.think_effort)
                ),
                ctx.prompt.clone(),
            ],
            stdin_lines: Vec::new(),
            env_overrides,
        },
        HarnessKind::Opencode => CliHarnessSpec {
            program: "opencode".to_owned(),
            args: vec![
                "run".to_owned(),
                "--format".to_owned(),
                "json".to_owned(),
                "--model".to_owned(),
                format!("{}/{}", ctx.target.provider, ctx.target.model),
                ctx.prompt.clone(),
            ],
            stdin_lines: Vec::new(),
            env_overrides,
        },
        HarnessKind::PiNative => CliHarnessSpec {
            program: "nexus".to_owned(),
            args: vec!["worker".to_owned(), "pi-native".to_owned()],
            stdin_lines: Vec::new(),
            env_overrides,
        },
    }
}

/// Background shell job state managed by `PiNativeDriver` for commands that exceed the 5s foreground wait.
struct BackgroundJob {
    output_buffer: Arc<Mutex<String>>,
    finished: Arc<Mutex<Option<i32>>>,
    kill_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Manages foreground-to-background shell commands for `PiNativeDriver`.
#[derive(Default)]
pub struct BackgroundJobManager {
    next_id: u64,
    jobs: BTreeMap<String, BackgroundJob>,
}

impl BackgroundJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `cmd_str` inside `cwd`. If it finishes within `wait_timeout`, returns the output synchronously.
    /// Otherwise promotes it to a background job and returns its `job_id` immediately.
    pub async fn run_or_background(
        &mut self,
        cmd_str: &str,
        cwd: &Path,
        env_overrides: &BTreeMap<String, String>,
        wait_timeout: std::time::Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let mut cmd = Command::new("zsh");
        cmd.arg("-lc")
            .arg(cmd_str)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (k, _) in std::env::vars() {
            if is_sensitive_env_key(&k) {
                cmd.env_remove(&k);
            }
        }
        for (k, v) in env_overrides {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let output_buffer = Arc::new(Mutex::new(String::new()));
        let finished = Arc::new(Mutex::new(None));

        let buf_out = Arc::clone(&output_buffer);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut guard = buf_out.lock().await;
                guard.push_str(&line);
                guard.push('\n');
            }
        });

        let buf_err = Arc::clone(&output_buffer);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let mut guard = buf_err.lock().await;
                guard.push_str(&line);
                guard.push('\n');
            }
        });

        let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel::<()>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<i32>();
        let fin_clone = Arc::clone(&finished);

        tokio::spawn(async move {
            let code = tokio::select! {
                _ = &mut kill_rx => {
                    let _ = child.kill().await;
                    -9
                }
                status = child.wait() => {
                    status.ok().and_then(|s| s.code()).unwrap_or(-1)
                }
            };
            *fin_clone.lock().await = Some(code);
            let _ = done_tx.send(code);
        });

        match tokio::time::timeout(wait_timeout, done_rx).await {
            Ok(Ok(exit_code)) => {
                // Give stdout/stderr reader tasks a brief moment to flush remaining lines.
                tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                let out = output_buffer.lock().await.clone();
                Ok(serde_json::json!({
                    "backgrounded": false,
                    "exit_code": exit_code,
                    "output": out,
                }))
            }
            _ => {
                self.next_id += 1;
                let job_id = format!("job-{}", self.next_id);
                self.jobs.insert(
                    job_id.clone(),
                    BackgroundJob {
                        output_buffer: Arc::clone(&output_buffer),
                        finished,
                        kill_tx: Some(kill_tx),
                    },
                );
                let partial = output_buffer.lock().await.clone();
                Ok(serde_json::json!({
                    "backgrounded": true,
                    "job_id": job_id,
                    "partial_output": partial,
                    "note": "Command exceeded foreground wait window and continues in the background. Use `job_output` or `kill_job`.",
                }))
            }
        }
    }

    pub async fn job_output(&self, job_id: &str) -> anyhow::Result<serde_json::Value> {
        let job = self
            .jobs
            .get(job_id)
            .ok_or_else(|| anyhow::anyhow!("Background job {job_id} not found"))?;
        let out = job.output_buffer.lock().await.clone();
        let exit_code = *job.finished.lock().await;
        Ok(serde_json::json!({
            "job_id": job_id,
            "running": exit_code.is_none(),
            "exit_code": exit_code,
            "output": out,
        }))
    }

    pub async fn kill_job(&mut self, job_id: &str) -> anyhow::Result<serde_json::Value> {
        let job = self
            .jobs
            .get_mut(job_id)
            .ok_or_else(|| anyhow::anyhow!("Background job {job_id} not found"))?;
        if let Some(tx) = job.kill_tx.take() {
            let _ = tx.send(());
        }
        Ok(serde_json::json!({
            "job_id": job_id,
            "killed": true,
        }))
    }
}

/// Tool specifications exposed to the built-in `pi-native` ReAct Worker.
pub fn pi_native_tool_specs() -> Vec<LlmToolSpec> {
    vec![
        LlmToolSpec {
            name: "read_file".to_owned(),
            description: "Read the contents of a file relative to the worktree.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        },
        LlmToolSpec {
            name: "write_file".to_owned(),
            description: "Write full contents to a file relative to the worktree.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        },
        LlmToolSpec {
            name: "edit_file".to_owned(),
            description: "Replace an exact string target in a file relative to the worktree.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "target": { "type": "string" },
                    "replacement": { "type": "string" }
                },
                "required": ["path", "target", "replacement"]
            }),
        },
        LlmToolSpec {
            name: "run_command".to_owned(),
            description: "Execute a shell command in the worktree (`gh-agent`, `lark-bot`, `cargo`, `git`, etc.). Automatically backgrounds after 5 seconds if still running.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        },
        LlmToolSpec {
            name: "job_output".to_owned(),
            description: "Read the latest stdout/stderr and status of a backgrounded shell job.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "job_id": { "type": "string" } },
                "required": ["job_id"]
            }),
        },
        LlmToolSpec {
            name: "kill_job".to_owned(),
            description: "Terminate a backgrounded shell job.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "job_id": { "type": "string" } },
                "required": ["job_id"]
            }),
        },
        LlmToolSpec {
            name: "worker_report".to_owned(),
            description: "Report the Worker's structured outcome (`ready`, `waiting`, `blocked`, `failed`, or `dropped`) and finish the turn.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["ready", "waiting", "blocked", "failed", "dropped"]
                    },
                    "summary": { "type": "string" },
                    "question": { "type": "string" },
                    "pull_request_thread_key": { "type": "string" },
                    "pull_request_url": { "type": "string" }
                },
                "required": ["status", "summary"]
            }),
        },
    ]
}

/// Runs the built-in `pi-native` ReAct loop with 5-second command auto-backgrounding and
/// automatic `Planning -> Implementation` `ThinkEffort` downgrade after the first file edit.
pub async fn run_pi_native_worker<L: LlmClient>(
    llm: &L,
    ctx: &WorkerLaunchContext,
    foreground_wait: std::time::Duration,
) -> anyhow::Result<WorkerExecutionResult> {
    let spec = build_cli_harness_spec(ctx);
    let mut jobs = BackgroundJobManager::new();
    let mut phase = WorkerPhase::Planning;
    let mut current_effort = ctx.target.think_effort;

    let mut messages = vec![
        LlmMessage::system(format!(
            "You are a Nexus `pi-native` autonomous engineering worker.\n\
             Active Runtime Engine: harness=`pi-native` (built-in Rust ReAct loop inside the `nexus` daemon process), provider=`{}`, model=`{}`, think_effort=`{}`.\n\
             Inspect files, make minimal accurate edits, verify with tests, and finish promptly by calling `worker_report` with a concise Chinese summary (<= 85 characters).",
            ctx.target.provider,
            ctx.target.model,
            current_effort.as_str()
        )),
        LlmMessage::user(ctx.prompt.clone()),
    ];
    let tools = pi_native_tool_specs();
    let mut last_text = String::new();
    let mut trace_seq = 0usize;
    let mut sent_progress_count = 0usize;
    let max_turns = ctx.max_turns.max(1);

    for turn in 0..max_turns {
        if turn > 0 && turn + 1 == max_turns {
            messages.push(LlmMessage::user(
                "已达到最大工具轮次，请勿再执行其他命令，立即调用 `worker_report` 提交最终简短中文结论（<= 85 字）。",
            ));
        }
        let resp = match llm
            .complete(&LlmCompletionRequest {
                provider: ctx.target.provider.clone(),
                model: ctx.target.model.clone(),
                think_effort: current_effort,
                messages: messages.clone(),
                tools: tools.clone(),
                max_tokens: 8_192,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                append_worker_trace_step(
                    &ctx.state_dir,
                    &mut trace_seq,
                    &ctx.assignment_id,
                    &ctx.ticket_id,
                    "error",
                    format!(
                        "pi-native LLM 调用失败 ({}/{})",
                        ctx.target.provider, ctx.target.model
                    ),
                    e.to_string(),
                );
                return Err(e);
            }
        };

        if trace_seq == 0 {
            append_worker_trace_step(
                &ctx.state_dir,
                &mut trace_seq,
                &ctx.assignment_id,
                &ctx.ticket_id,
                "launch",
                format!(
                    "启动 Worker `{}` (pi-native:{}/{}:{})",
                    ctx.assignment_id,
                    ctx.target.provider,
                    ctx.target.model,
                    current_effort.as_str()
                ),
                format!("Worktree: {}\n\n{}", ctx.worktree_dir.display(), ctx.prompt),
            );
        }

        if !resp.content.is_empty() {
            append_worker_trace_step(
                &ctx.state_dir,
                &mut trace_seq,
                &ctx.assignment_id,
                &ctx.ticket_id,
                "thinking",
                format!("pi-native 思考输出 (effort={})", current_effort.as_str()),
                &resp.content,
            );
            last_text = resp.content.clone();
        }
        if resp.tool_calls.is_empty() {
            break;
        }

        // If the LLM emitted thinking text before invoking non-report tools, stream it as live progress!
        let has_only_report = resp
            .tool_calls
            .iter()
            .all(|tc| tc.name == "worker_report");
        if !has_only_report
            && sent_progress_count < 3
            && !resp.content.trim().is_empty()
            && let Some(tx) = &ctx.progress_tx
        {
            let first_line = resp
                .content
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or(resp.content.trim());
            let note = crate::core::recall::truncate_to_im_word_limit(
                &format!("💭 {}", first_line.trim()),
                85,
            );
            let _ = tx.send(note);
            sent_progress_count += 1;
        }

        messages.push(LlmMessage::assistant(
            resp.content.clone(),
            resp.tool_calls.clone(),
        ));

        for tc in resp.tool_calls {
            append_worker_trace_step(
                &ctx.state_dir,
                &mut trace_seq,
                &ctx.assignment_id,
                &ctx.ticket_id,
                "tool_call",
                format!("调用工具: {}", tc.name),
                serde_json::to_string_pretty(&tc.arguments).unwrap_or_default(),
            );
            match tc.name.as_str() {
                "worker_report" => {
                    let status_str = tc
                        .arguments
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("ready");
                    let status = match status_str {
                        "waiting" => ReportStatus::Waiting,
                        "blocked" => ReportStatus::Blocked,
                        "failed" => ReportStatus::Failed,
                        "dropped" => ReportStatus::Dropped,
                        _ => ReportStatus::Ready,
                    };
                    let raw_summary = tc
                        .arguments
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Completed task");
                    let summary = crate::core::recall::truncate_to_im_word_limit(raw_summary, 95);
                    let question = tc
                        .arguments
                        .get("question")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                    let mut pr_thread = tc
                        .arguments
                        .get("pull_request_thread_key")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned);
                    let pr_url = tc
                        .arguments
                        .get("pull_request_url")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(str::to_owned);
                    if pr_thread.is_none()
                        && let Some(u) = &pr_url
                        && let Some((derived_key, _)) = extract_github_pr_reference(u)
                    {
                        pr_thread = Some(derived_key);
                    }
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "finish",
                        format!("pi-native 执行完成 (status={status:?})"),
                        &summary,
                    );
                    return Ok(WorkerExecutionResult {
                        status,
                        summary,
                        question,
                        pull_request_thread_key: pr_thread,
                        pull_request_url: pr_url,
                        raw_output: last_text,
                    });
                }
                "read_file" => {
                    let rel = tc
                        .arguments
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let full = ctx.worktree_dir.join(rel);
                    let res = match tokio::fs::read_to_string(&full).await {
                        Ok(content) => serde_json::json!({ "content": content }).to_string(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                "write_file" => {
                    let rel = tc
                        .arguments
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let content = tc
                        .arguments
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let full = ctx.worktree_dir.join(rel);
                    if let Some(parent) = full.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    let res = match tokio::fs::write(&full, content).await {
                        Ok(()) => {
                            if phase == WorkerPhase::Planning {
                                phase = WorkerPhase::Implementation;
                                current_effort = downgrade_for_implementation(current_effort);
                            }
                            serde_json::json!({
                                "ok": true,
                                "phase": phase,
                                "active_think_effort": current_effort.as_str()
                            })
                            .to_string()
                        }
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                "edit_file" => {
                    let rel = tc
                        .arguments
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let target = tc
                        .arguments
                        .get("target")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let replacement = tc
                        .arguments
                        .get("replacement")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let full = ctx.worktree_dir.join(rel);
                    let res = match tokio::fs::read_to_string(&full).await {
                        Ok(existing) if existing.contains(target) => {
                            let updated = existing.replace(target, replacement);
                            match tokio::fs::write(&full, updated).await {
                                Ok(()) => {
                                    if phase == WorkerPhase::Planning {
                                        phase = WorkerPhase::Implementation;
                                        current_effort =
                                            downgrade_for_implementation(current_effort);
                                    }
                                    serde_json::json!({
                                        "ok": true,
                                        "phase": phase,
                                        "active_think_effort": current_effort.as_str()
                                    })
                                    .to_string()
                                }
                                Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                            }
                        }
                        Ok(_) => serde_json::json!({ "error": "Target string not found in file" })
                            .to_string(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                "run_command" => {
                    let command = tc
                        .arguments
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let res = match jobs
                        .run_or_background(
                            command,
                            &ctx.worktree_dir,
                            &spec.env_overrides,
                            foreground_wait,
                        )
                        .await
                    {
                        Ok(val) => val.to_string(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "tool_result",
                        format!("命令结果: {command}"),
                        res.chars().take(4_000).collect::<String>(),
                    );
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                "job_output" => {
                    let job_id = tc
                        .arguments
                        .get("job_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let res = match jobs.job_output(job_id).await {
                        Ok(val) => val.to_string(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                "kill_job" => {
                    let job_id = tc
                        .arguments
                        .get("job_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let res = match jobs.kill_job(job_id).await {
                        Ok(val) => val.to_string(),
                        Err(e) => serde_json::json!({ "error": e.to_string() }).to_string(),
                    };
                    messages.push(LlmMessage::tool_result(tc.id, res));
                }
                other => {
                    messages.push(LlmMessage::tool_result(
                        tc.id,
                        serde_json::json!({ "error": format!("Unknown tool {other}") }).to_string(),
                    ));
                }
            }
        }
    }

    Ok(WorkerExecutionResult {
        status: ReportStatus::Ready,
        summary: if last_text.is_empty() {
            "Finished worker execution".to_owned()
        } else {
            last_text.chars().take(300).collect()
        },
        question: None,
        pull_request_thread_key: None,
        pull_request_url: None,
        raw_output: last_text,
    })
}

/// Executes an external CLI Worker Harness (`pi-rpc`, `claude-code`, `codex`, `opencode`) inside `ctx.worktree_dir`.
pub async fn run_cli_harness_worker(
    ctx: &WorkerLaunchContext,
) -> anyhow::Result<WorkerExecutionResult> {
    let spec = build_cli_harness_spec(ctx);

    let stdin_cfg = if spec.stdin_lines.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    };

    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&ctx.worktree_dir)
        .stdin(stdin_cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, _) in std::env::vars() {
        if is_sensitive_env_key(&k) {
            cmd.env_remove(&k);
        }
    }
    for (k, v) in &spec.env_overrides {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        for line in &spec.stdin_lines {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }
        stdin.flush().await?;
    }

    let stdout_pipe = child.stdout.take().unwrap();
    let stderr_pipe = child.stderr.take().unwrap();

    let stderr_handle = tokio::spawn(async move {
        let mut err_buf = String::new();
        let mut reader = BufReader::new(stderr_pipe).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            err_buf.push_str(&line);
            err_buf.push('\n');
        }
        err_buf
    });

    let mut stdout_buf = String::new();
    let mut reader = BufReader::new(stdout_pipe).lines();
    let mut pending_agent_thought: Option<String> = None;
    let mut sent_progress_count = 0usize;
    let mut trace_seq = 0usize;

    append_worker_trace_step(
        &ctx.state_dir,
        &mut trace_seq,
        &ctx.assignment_id,
        &ctx.ticket_id,
        "launch",
        format!(
            "启动 Worker `{}` ({}:{}/{}:{})",
            ctx.assignment_id,
            ctx.target.harness.as_str(),
            ctx.target.provider,
            ctx.target.model,
            ctx.target.think_effort.as_str()
        ),
        format!(
            "Worktree: {}\nProgram: {} {}\n\n{}",
            ctx.worktree_dir.display(),
            spec.program,
            spec.args.iter().take(6).cloned().collect::<Vec<_>>().join(" "),
            ctx.prompt
        ),
    );

    while let Ok(Some(line)) = reader.next_line().await {
        stdout_buf.push_str(&line);
        stdout_buf.push('\n');

        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
            let ev_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if ev_type == "item.completed" {
                let item_type = val
                    .pointer("/item/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let item_text = val
                    .pointer("/item/text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if item_type == "agent_message" && !item_text.is_empty() {
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "thinking",
                        "Agent 思考 / 输出消息 (agent_message)",
                        item_text,
                    );
                    pending_agent_thought = Some(item_text.to_owned());
                } else if item_type == "reasoning" && !item_text.is_empty() {
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "thinking",
                        "模型推理链 (reasoning)",
                        item_text,
                    );
                    if sent_progress_count < 3
                        && let Some(tx) = &ctx.progress_tx
                    {
                        let note = crate::core::recall::truncate_to_im_word_limit(
                            &format!("💭 {item_text}"),
                            85,
                        );
                        let _ = tx.send(note);
                        sent_progress_count += 1;
                    }
                } else if item_type == "command_execution" {
                    let cmd_str = val
                        .pointer("/item/command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let exit_code = val
                        .pointer("/item/exit_code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let agg_out = val
                        .pointer("/item/aggregated_output")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "tool_result",
                        format!("命令执行完成 (exit={exit_code}): {cmd_str}"),
                        if agg_out.trim().is_empty() {
                            "(无输出)".to_owned()
                        } else {
                            agg_out.chars().take(4_000).collect()
                        },
                    );
                } else if item_type == "file_change" {
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "tool_result",
                        "文件变更 (file_change)",
                        serde_json::to_string_pretty(&val["item"]).unwrap_or_default(),
                    );
                }
            } else if ev_type == "item.started" {
                let item_type = val
                    .pointer("/item/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if item_type == "command_execution" {
                    let cmd_str = val
                        .pointer("/item/command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    append_worker_trace_step(
                        &ctx.state_dir,
                        &mut trace_seq,
                        &ctx.assignment_id,
                        &ctx.ticket_id,
                        "tool_call",
                        format!("开始执行命令: {cmd_str}"),
                        cmd_str,
                    );
                }
                // An `agent_message` followed by `item.started` (e.g. `command_execution`) is an intermediate
                // thinking update before running a tool—stream it immediately to Feishu!
                if let Some(thought) = pending_agent_thought.take()
                    && sent_progress_count < 3
                    && let Some(tx) = &ctx.progress_tx
                {
                    let note = crate::core::recall::truncate_to_im_word_limit(
                        &format!("💭 {thought}"),
                        85,
                    );
                    let _ = tx.send(note);
                    sent_progress_count += 1;
                }
            } else if ev_type == "turn.completed"
                && let Some(usage) = val.get("usage")
            {
                append_worker_trace_step(
                    &ctx.state_dir,
                    &mut trace_seq,
                    &ctx.assignment_id,
                    &ctx.ticket_id,
                    "usage",
                    "轮次完成 & Token 用量统计",
                    serde_json::to_string_pretty(usage).unwrap_or_default(),
                );
            }
        }
    }

    let exit_status = child.wait().await?;
    let stderr = stderr_handle.await.unwrap_or_default().trim().to_owned();
    let stdout = stdout_buf.trim().to_owned();

    let status = if exit_status.success() {
        ReportStatus::Ready
    } else {
        ReportStatus::Failed
    };

    let summary = extract_summary_from_jsonl_or_text(&stdout, &stderr);

    append_worker_trace_step(
        &ctx.state_dir,
        &mut trace_seq,
        &ctx.assignment_id,
        &ctx.ticket_id,
        if exit_status.success() { "finish" } else { "error" },
        format!(
            "Worker 执行结束 (status={:?}, code={:?})",
            status,
            exit_status.code()
        ),
        if !summary.is_empty() {
            summary.clone()
        } else {
            stderr.clone()
        },
    );

    let (pull_request_thread_key, pull_request_url) = extract_github_pr_reference(&summary)
        .map(|(k, u)| (Some(k), Some(u)))
        .unwrap_or((None, None));

    Ok(WorkerExecutionResult {
        status,
        summary,
        question: None,
        pull_request_thread_key,
        pull_request_url,
        raw_output: if stdout.is_empty() { stderr } else { stdout },
    })
}

/// Extracts `("github:pr:<owner>/<repo>#<num>", "https://github.com/<owner>/<repo>/pull/<num>")`
/// if `text` contains a GitHub Pull Request URL.
pub fn extract_github_pr_reference(text: &str) -> Option<(String, String)> {
    let marker = "https://github.com/";
    let mut rest = text;
    while let Some(idx) = rest.find(marker) {
        let after = &rest[idx + marker.len()..];
        let end = after
            .find(|c: char| {
                c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | '`' | '，' | '。')
            })
            .unwrap_or(after.len());
        let path = &after[..end];
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() >= 4
            && parts[2] == "pull"
            && let Ok(num) = parts[3].parse::<u64>()
        {
            let owner = parts[0];
            let repo = parts[1];
            return Some((
                format!("github:pr:{owner}/{repo}#{num}"),
                format!("https://github.com/{owner}/{repo}/pull/{num}"),
            ));
        }
        rest = &after[end..];
    }
    None
}

fn extract_summary_from_jsonl_or_text(stdout: &str, stderr: &str) -> String {
    let raw = extract_raw_summary_from_jsonl_or_text(stdout, stderr);
    crate::core::recall::truncate_to_im_word_limit(&raw, 95)
}

fn extract_raw_summary_from_jsonl_or_text(stdout: &str, stderr: &str) -> String {
    for line in stdout.lines().rev() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(text) = val.pointer("/item/text").and_then(|v| v.as_str())
                && !text.trim().is_empty()
            {
                return text.trim().chars().take(300).collect();
            }
            if let Some(res) = val.get("result").and_then(|v| v.as_str())
                && !res.trim().is_empty()
            {
                return res.trim().chars().take(300).collect();
            }
            if let Some(msg) = val.get("message").and_then(|v| v.as_str())
                && !msg.trim().is_empty()
            {
                return msg.trim().chars().take(300).collect();
            }
        }
    }
    let fallback = if !stdout.is_empty() { stdout } else { stderr };
    if fallback.is_empty() {
        "CLI harness finished.".to_owned()
    } else {
        fallback
            .lines()
            .last()
            .unwrap_or("CLI harness finished.")
            .chars()
            .take(300)
            .collect()
    }
}
