use nexus::core::model::{ExecutionTarget, HarnessKind, ReportStatus, ThinkEffort};
use nexus::router::llm::{
    LlmClient, LlmCompletionRequest, LlmCompletionResponse, LlmToolCall,
};
use nexus::workers::dispatcher::{LaunchPriority, WorkerSlots};
use nexus::workers::driver::{
    BackgroundJobManager, WorkerLaunchContext, build_cli_harness_spec, is_sensitive_env_key,
    run_pi_native_worker,
};
use nexus::workers::worktree::{capture_git_status_diff, prepare_worktree};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[test]
fn worker_slots_reserves_one_slot_for_human_wakeups() {
    let mut slots = WorkerSlots::new(4);
    assert!(slots.try_acquire("asg-1", LaunchPriority::General));
    assert!(slots.try_acquire("asg-2", LaunchPriority::General));
    assert!(slots.try_acquire("asg-3", LaunchPriority::General));

    // 4th slot is reserved for HumanWakeup, so General launch is rejected
    assert!(!slots.try_acquire("asg-4-general", LaunchPriority::General));

    // HumanWakeup can acquire the reserved 4th slot!
    assert!(slots.try_acquire("asg-4-human", LaunchPriority::HumanWakeup));
    assert_eq!(slots.active_count(), 4);

    slots.release("asg-1");
    assert_eq!(slots.active_count(), 3);
}

#[test]
fn cli_harness_specs_map_think_effort_and_scrub_credentials() {
    assert!(is_sensitive_env_key("GITHUB_TOKEN"));
    assert!(is_sensitive_env_key("FEISHU_APP_SECRET"));
    assert!(is_sensitive_env_key("SSH_PRIVATE_KEY"));
    assert!(!is_sensitive_env_key("ANTHROPIC_API_KEY"));
    assert!(!is_sensitive_env_key("NEXUS_TICKET_ID"));
    assert!(!is_sensitive_env_key("PATH"));

    let base_ctx = WorkerLaunchContext {
        ticket_id: "tkt-1".to_owned(),
        assignment_id: "asg-tkt-1-1".to_owned(),
        target: ExecutionTarget {
            harness: HarnessKind::ClaudeCode,
            provider: "anthropic".to_owned(),
            model: "claude-opus-4-6".to_owned(),
            think_effort: ThinkEffort::Xhigh,
        },
        worktree_dir: "/tmp/wt-1".into(),
        state_dir: "/tmp/nexus-state".into(),
        api_url: "http://127.0.0.1:3082".to_owned(),
        prompt: "Fix bug".to_owned(),
        max_turns: 4,
    };

    // 1. Claude Code maps Xhigh -> --effort max
    let claude_spec = build_cli_harness_spec(&base_ctx);
    assert_eq!(claude_spec.program, "claude");
    assert!(claude_spec.args.windows(2).any(|w| w == ["--effort", "max"]));

    // 2. Codex maps High -> -c model_reasoning_effort="high"
    let mut codex_ctx = base_ctx.clone();
    codex_ctx.target = ExecutionTarget {
        harness: HarnessKind::Codex,
        provider: "openai-codex".to_owned(),
        model: "gpt-5.6-sol".to_owned(),
        think_effort: ThinkEffort::High,
    };
    let codex_spec = build_cli_harness_spec(&codex_ctx);
    assert_eq!(codex_spec.program, "codex");
    assert!(
        codex_spec
            .args
            .contains(&"model_reasoning_effort=\"high\"".to_owned())
    );

    // 3. PiRpc sends JSONL set_thinking_level + prompt over stdin
    let mut pi_rpc_ctx = base_ctx.clone();
    pi_rpc_ctx.target = ExecutionTarget {
        harness: HarnessKind::PiRpc,
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-4-6".to_owned(),
        think_effort: ThinkEffort::Medium,
    };
    let pi_rpc_spec = build_cli_harness_spec(&pi_rpc_ctx);
    assert_eq!(pi_rpc_spec.program, "pi");
    assert_eq!(pi_rpc_spec.stdin_lines.len(), 2);
    assert!(pi_rpc_spec.stdin_lines[0].contains("\"set_thinking_level\""));
    assert!(pi_rpc_spec.stdin_lines[1].contains("\"prompt\""));

    // 4. OpenCode passes --model <provider>/<model>
    let mut opencode_ctx = base_ctx;
    opencode_ctx.target = ExecutionTarget {
        harness: HarnessKind::Opencode,
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-4-6".to_owned(),
        think_effort: ThinkEffort::Low,
    };
    let opencode_spec = build_cli_harness_spec(&opencode_ctx);
    assert_eq!(opencode_spec.program, "opencode");
    assert!(
        opencode_spec
            .args
            .windows(2)
            .any(|w| w == ["--model", "anthropic/claude-sonnet-4-6"])
    );
}

#[tokio::test]
async fn pi_native_downgrades_think_effort_after_edit_and_auto_backgrounds_slow_commands() {
    let tmp_root = std::env::temp_dir().join(format!("nexus-test-wt-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&tmp_root).await;
    tokio::fs::create_dir_all(&tmp_root).await.unwrap();

    struct RecordingLlm {
        efforts_seen: Mutex<Vec<ThinkEffort>>,
    }

    impl LlmClient for RecordingLlm {
        async fn complete(
            &self,
            request: &LlmCompletionRequest,
        ) -> anyhow::Result<LlmCompletionResponse> {
            let mut seen = self.efforts_seen.lock().unwrap();
            seen.push(request.think_effort);
            let turn = seen.len();
            if turn == 1 {
                // Turn 1 (Planning @ High): write a file -> triggers downgrade to Medium!
                Ok(LlmCompletionResponse {
                    content: "Writing fix".to_owned(),
                    tool_calls: vec![LlmToolCall {
                        id: "c1".to_owned(),
                        name: "write_file".to_owned(),
                        arguments: serde_json::json!({
                            "path": "src/fix.rs",
                            "content": "pub fn fixed() -> bool { true }\n"
                        }),
                    }],
                })
            } else {
                // Turn 2 (Implementation @ Medium): report ready with PR
                Ok(LlmCompletionResponse {
                    content: "Reporting ready".to_owned(),
                    tool_calls: vec![LlmToolCall {
                        id: "c2".to_owned(),
                        name: "worker_report".to_owned(),
                        arguments: serde_json::json!({
                            "status": "ready",
                            "summary": "Fixed parser issue and opened PR #7.",
                            "pull_request_thread_key": "github:pr:owner/nexus#7",
                            "pull_request_url": "https://github.com/owner/nexus/pull/7"
                        }),
                    }],
                })
            }
        }
    }

    let llm = RecordingLlm {
        efforts_seen: Mutex::new(Vec::new()),
    };
    let ctx = WorkerLaunchContext {
        ticket_id: "tkt-10".to_owned(),
        assignment_id: "asg-tkt-10-1".to_owned(),
        target: ExecutionTarget {
            harness: HarnessKind::PiNative,
            provider: "anthropic".to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            think_effort: ThinkEffort::High,
        },
        worktree_dir: tmp_root.clone(),
        state_dir: tmp_root.clone(),
        api_url: "http://127.0.0.1:3082".to_owned(),
        prompt: "Fix parser bug".to_owned(),
        max_turns: 4,
    };

    let result = run_pi_native_worker(&llm, &ctx, std::time::Duration::from_millis(200))
        .await
        .unwrap();
    assert_eq!(result.status, ReportStatus::Ready);
    assert_eq!(
        result.pull_request_thread_key.as_deref(),
        Some("github:pr:owner/nexus#7")
    );

    // Verify Planning (High) -> Implementation (Medium) automatic downgrade!
    let efforts = llm.efforts_seen.lock().unwrap().clone();
    assert_eq!(efforts, vec![ThinkEffort::High, ThinkEffort::Medium]);

    // Also test BackgroundJobManager auto-backgrounding a slow command
    let mut jobs = BackgroundJobManager::new();
    let bg_res = jobs
        .run_or_background(
            "echo start-bg && sleep 1",
            &tmp_root,
            &BTreeMap::new(),
            std::time::Duration::from_millis(60),
        )
        .await
        .unwrap();
    assert_eq!(bg_res["backgrounded"], serde_json::json!(true));
    let job_id = bg_res["job_id"].as_str().unwrap().to_owned();
    let kill_res = jobs.kill_job(&job_id).await.unwrap();
    assert_eq!(kill_res["killed"], serde_json::json!(true));

    // Also verify git worktree status capture
    prepare_worktree(None, &tmp_root, "work-10").await.unwrap();
    let _ = capture_git_status_diff(&tmp_root).await;

    let _ = tokio::fs::remove_dir_all(&tmp_root).await;
}
