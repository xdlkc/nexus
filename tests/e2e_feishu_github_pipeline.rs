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

#[test]
fn feishu_wake_conditions_and_conversation_memory_prompt() {
    use nexus::adapters::feishu::is_feishu_message_addressed;
    use nexus::core::model::{
        AdmissionStrength, ChannelAddress, ExecutionTarget, OutboxPurpose, SourceKindId, Trigger,
    };
    use nexus::infra::effects::mark_outbox_sent;
    use nexus::workers::dispatcher::build_worker_prompt_with_memory;
    use std::collections::BTreeMap;

    let mut cfg = NexusConfig::default_with_state_dir("/tmp/nexus-mem-test".into());
    cfg.feishu.bot_name = "奈克斯".to_owned();
    assert!(!cfg.feishu.reply_in_thread);

    // 1. Un-`@`ed casual chat in group is NOT addressed
    assert!(!is_feishu_message_addressed(
        &cfg.feishu,
        "group",
        "今天中午吃什么？",
        false
    ));

    // 2. Un-`@`ed message mentioning "奈克斯，你来做xxx" IS addressed
    assert!(is_feishu_message_addressed(
        &cfg.feishu,
        "group",
        "奈克斯，你来做一下剪贴板历史搜索功能",
        false
    ));

    // 3. Conversational memory prompt includes prior turns in the same chat
    let mut engine = Engine::new("/tmp/nexus-wt");
    let ch = ChannelAddress::FeishuTopic {
        source_id: "feishu-main".to_owned(),
        chat_id: "oc_group_1".to_owned(),
        root_message_id: "om_1".to_owned(),
        reply_to_message_id: "om_1".to_owned(),
    };
    engine.receive_trigger(
        Trigger {
            id: "feishu-msg-om_1".to_owned(),
            source_id: "feishu-main".to_owned(),
            source_kind: SourceKindId::Feishu,
            external_id: "om_1".to_owned(),
            thread_key: "feishu:topic:oc_group_1:om_1".to_owned(),
            actor_id: "ou_user".to_owned(),
            actor_is_bot: false,
            addressed_agent: true,
            text: "记住我的项目叫 clipboard-history".to_owned(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: Some(ch.clone()),
            at: "2026-11-03T01:00:00Z".to_owned(),
            meta: BTreeMap::new(),
        },
        AdmissionStrength::Explicit,
    );
    let out_id = engine
        .queue_outbox_message(
            None,
            ch.clone(),
            OutboxPurpose::Answer,
            "好的，已记住你的项目是 clipboard-history。",
            "2026-11-03T01:00:05Z",
        )
        .unwrap();
    mark_outbox_sent(&mut engine, &out_id);

    // Second turn in the same chat
    engine.receive_trigger(
        Trigger {
            id: "feishu-msg-om_2".to_owned(),
            source_id: "feishu-main".to_owned(),
            source_kind: SourceKindId::Feishu,
            external_id: "om_2".to_owned(),
            thread_key: "feishu:topic:oc_group_1:om_2".to_owned(),
            actor_id: "ou_user".to_owned(),
            actor_is_bot: false,
            addressed_agent: true,
            text: "我刚刚让你记住的项目叫什么？".to_owned(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: Some(ch),
            at: "2026-11-03T01:01:00Z".to_owned(),
            meta: BTreeMap::new(),
        },
        AdmissionStrength::Explicit,
    );
    let tkt_id = engine
        .accept_ticket(
            "feishu-msg-om_2",
            "询问项目名",
            None,
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::Codex,
                provider: "openai".to_owned(),
                model: "gpt-6-astra".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            None,
            "2026-11-03T01:01:01Z",
        )
        .unwrap();

    let ticket = &engine.state.tickets[&tkt_id];
    let assign = &ticket.assignments[0];
    let prompt = build_worker_prompt_with_memory(ticket, assign, Some(&engine.state), None);
    assert!(prompt.contains("Recent Conversation History in This Chat"));
    assert!(prompt.contains("记住我的项目叫 clipboard-history"));
    assert!(prompt.contains("好的，已记住你的项目是 clipboard-history。"));
}
