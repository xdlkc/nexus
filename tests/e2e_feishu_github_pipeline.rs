use nexus::adapters::feishu::{parse_feishu_chat_members_json, parse_feishu_messages_json};
use nexus::adapters::github::{
    parse_github_issue_comments_json, parse_github_pr_state_json, parse_github_review_comments_json,
};
use nexus::config::NexusConfig;
use nexus::core::engine::Engine;
use nexus::core::model::{HarnessKind, ThinkEffort, TicketStage};
use nexus::infra::store::SqliteStore;
use nexus::router::jev_http::HeuristicJevClassifier;
use nexus::router::llm::{
    LlmClient, LlmCompletionRequest, LlmCompletionResponse, LlmToolCall,
};
use nexus::runtime::NexusRuntime;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn write_executable_script(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn feishu_and_github_json_parsers_extract_threads_directives_and_mentions() {
    let cfg = NexusConfig::default_with_state_dir("/tmp/nexus-test".into());

    // 1. Feishu chat members JSON
    let members_json = serde_json::json!({
        "code": 0,
        "data": {
            "items": [
                { "member_id": "ou_alice" },
                { "member_id": "ou_bob" }
            ]
        }
    });
    let members = parse_feishu_chat_members_json(&members_json);
    assert!(members.contains("ou_alice"));
    assert!(members.contains("ou_bob"));

    // 2. Feishu messages JSON with inline directive
    let msg_json = serde_json::json!({
        "code": 0,
        "data": {
            "items": [{
                "message_id": "om_root_100",
                "create_time": "1730592000000",
                "sender": { "id": "ou_alice", "sender_type": "user" },
                "body": {
                    "content": "{\"text\":\"@nexus [codex:openai-codex/gpt-5.6-sol:xhigh] Fix deadlock in worker pool\"}"
                }
            }]
        }
    });
    let (f_trigs, cursor) =
        parse_feishu_messages_json(&cfg.feishu, "oc_chat_dev", &msg_json, None);
    assert_eq!(f_trigs.len(), 1);
    assert_eq!(cursor.as_deref(), Some("1730592000000"));
    assert_eq!(f_trigs[0].thread_key, "feishu:topic:oc_chat_dev:om_root_100");
    assert!(f_trigs[0].addressed_agent);
    assert_eq!(f_trigs[0].text, "Fix deadlock in worker pool");
    assert_eq!(
        f_trigs[0].inline_target.as_ref().unwrap().harness,
        Some(HarnessKind::Codex)
    );

    // 3. GitHub issue/PR comments and review comments JSON
    let ic_json = serde_json::json!([{
        "id": 9001,
        "updated_at": "2026-11-03T04:00:00Z",
        "user": { "login": "alice", "type": "User" },
        "issue_url": "https://api.github.com/repos/acme/nexus/issues/42",
        "html_url": "https://github.com/acme/nexus/pull/42#issuecomment-9001",
        "body": "@nexus please also add a regression test"
    }]);
    let (gh_ic, _) = parse_github_issue_comments_json(&cfg.github, "acme/nexus", &ic_json, None);
    assert_eq!(gh_ic.len(), 1);
    assert_eq!(gh_ic[0].thread_key, "github:pr:acme/nexus#42");
    assert!(gh_ic[0].addressed_agent);

    let rc_json = serde_json::json!([{
        "id": 9002,
        "updated_at": "2026-11-03T04:01:00Z",
        "user": { "login": "bob", "type": "User" },
        "pull_request_url": "https://api.github.com/repos/acme/nexus/pulls/42",
        "path": "src/core/engine.rs",
        "body": "Use saturating_sub here"
    }]);
    let (gh_rc, _) = parse_github_review_comments_json(&cfg.github, "acme/nexus", &rc_json, None);
    assert_eq!(gh_rc.len(), 1);
    assert!(gh_rc[0].text.contains("[File `src/core/engine.rs`]"));

    assert_eq!(
        parse_github_pr_state_json(&serde_json::json!({
            "state": "MERGED",
            "mergedAt": "2026-11-03T04:05:00Z"
        })),
        Some("merged".to_owned())
    );
}

#[tokio::test]
async fn end_to_end_feishu_lark_cli_and_github_gh_closed_loop() {
    let tmp_dir = std::env::temp_dir().join(format!("nexus-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let feishu_replies_log = tmp_dir.join("feishu_replies.log");
    let pr_merged_flag = tmp_dir.join("pr_merged.flag");

    // Create mock `lark-cli` binary
    let mock_lark_bin = tmp_dir.join("mock-lark-cli");
    let lark_script = format!(
        "#!/usr/bin/env zsh\n\
         if [[ \"$2\" == \"GET\" ]]; then\n\
           echo '{{\"code\":0,\"data\":{{\"items\":[{{\"message_id\":\"om_root_100\",\"create_time\":\"1730592000000\",\"sender\":{{\"id\":\"ou_alice\",\"sender_type\":\"user\"}},\"body\":{{\"content\":\"{{\\\"text\\\":\\\"@nexus [pi-native:anthropic/claude-sonnet-4-6:high] Fix panic in parser when input is empty\\\"}}\"}}}}]}}}}'\n\
         elif [[ \"$2\" == \"POST\" ]]; then\n\
           echo \"$@\" >> '{}'\n\
           echo '{{\"code\":0}}'\n\
         fi\n",
        feishu_replies_log.display()
    );
    write_executable_script(&mock_lark_bin, &lark_script);

    // Create mock `gh` binary
    let mock_gh_bin = tmp_dir.join("mock-gh");
    let gh_script = format!(
        "#!/usr/bin/env zsh\n\
         if [[ \"$1\" == \"api\" ]]; then\n\
           echo '[]'\n\
         elif [[ \"$1\" == \"pr\" && \"$2\" == \"view\" ]]; then\n\
           if [[ -f '{}' ]]; then\n\
             echo '{{\"state\":\"MERGED\",\"mergedAt\":\"2026-11-03T04:10:00Z\"}}'\n\
           else\n\
             echo '{{\"state\":\"OPEN\",\"mergedAt\":null}}'\n\
           fi\n\
         fi\n",
        pr_merged_flag.display()
    );
    write_executable_script(&mock_gh_bin, &gh_script);

    // Configure Nexus with Feishu + GitHub
    let mut cfg = NexusConfig::default_with_state_dir(tmp_dir.clone());
    cfg.feishu.chat_ids = vec!["oc_chat_dev".to_owned()];
    cfg.feishu.lark_bin = mock_lark_bin;
    cfg.github.repos = vec!["acme/nexus".to_owned()];
    cfg.github.gh_bin = mock_gh_bin;

    let store = Arc::new(SqliteStore::open(&tmp_dir.join("nexus.db")).unwrap());
    let engine = Arc::new(Mutex::new(Engine::new(
        cfg.worktree_root.to_string_lossy(),
    )));
    let shared_cfg = Arc::new(Mutex::new(cfg));

    struct MockWorkerLlm;
    impl LlmClient for MockWorkerLlm {
        async fn complete(
            &self,
            _request: &LlmCompletionRequest,
        ) -> anyhow::Result<LlmCompletionResponse> {
            Ok(LlmCompletionResponse {
                content: "Opened PR #42".to_owned(),
                tool_calls: vec![LlmToolCall {
                    id: "call-rep".to_owned(),
                    name: "worker_report".to_owned(),
                    arguments: serde_json::json!({
                        "status": "ready",
                        "summary": "Fixed empty-input parser panic and opened PR #42.",
                        "pull_request_thread_key": "github:pr:acme/nexus#42",
                        "pull_request_url": "https://github.com/acme/nexus/pull/42"
                    }),
                }],
            })
        }
    }

    let mut runtime = NexusRuntime::new(
        Arc::clone(&engine),
        Arc::clone(&store),
        Arc::clone(&shared_cfg),
        HeuristicJevClassifier,
        MockWorkerLlm,
    )
    .unwrap();

    // Tick 1: Feishu message polled -> Jev FastDispatch -> PiNative worker opens PR #42 -> Feishu reply sent!
    let tick1 = runtime.tick("2026-11-03T04:00:00Z").await.unwrap();
    assert_eq!(tick1.polled_triggers, 1);
    assert_eq!(tick1.evaluated_batches, 1);
    assert_eq!(tick1.launched_assignments, 1);
    assert_eq!(tick1.delivered_outbox_messages, 1);

    {
        let eng = engine.lock().unwrap();
        let ticket = eng.state.tickets.values().next().unwrap();
        assert_eq!(ticket.current_target().harness, HarnessKind::PiNative);
        assert_eq!(ticket.current_target().think_effort, ThinkEffort::High);
        assert_eq!(ticket.stage(), TicketStage::PersonReview);
        assert!(ticket.is_open());
    }

    // Verify Feishu reply was delivered via `lark-cli`
    let replies_after_tick1 = std::fs::read_to_string(&feishu_replies_log).unwrap();
    assert!(replies_after_tick1.contains("om_root_100/reply"));
    assert!(replies_after_tick1.contains("PR #42"));

    // Simulate PR #42 merging on GitHub
    std::fs::write(&pr_merged_flag, "merged").unwrap();

    // Tick 2: GithubAdapter detects PR #42 merged -> Engine auto-completes ticket -> Feishu merge notice sent!
    let tick2 = runtime.tick("2026-11-03T04:10:00Z").await.unwrap();
    assert_eq!(tick2.closed_threads, 1);
    assert_eq!(tick2.delivered_outbox_messages, 1);

    {
        let eng = engine.lock().unwrap();
        let ticket = eng.state.tickets.values().next().unwrap();
        assert!(!ticket.is_open());
        assert_eq!(ticket.ended.as_ref().unwrap().outcome, "merged");
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
}
