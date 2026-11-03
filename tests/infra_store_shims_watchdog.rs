use nexus::core::engine::Engine;
use nexus::core::model::{
    AdmissionStrength, ExecutionTarget, HarnessKind, SourceKindId, ThinkEffort, Trigger,
};
use nexus::infra::effects::{compute_backoff_ms, pop_next_effect, record_effect_failure};
use nexus::infra::shims::{ShimConfig, provision_worker_shims};
use nexus::infra::store::SqliteStore;
use nexus::infra::watchdog::{WatchdogVerdict, inspect_and_recover_worker};
use nexus::router::jev::{
    ClassifierAnswer, ClassifierContext, ClassifierResult, JevClassifier,
};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

#[test]
fn sqlite_store_roundtrips_engine_state_cursors_and_settings() {
    let store = SqliteStore::open_in_memory().unwrap();
    assert!(store.load_engine_state().unwrap().is_none());

    let mut engine = Engine::new("/tmp/nexus-wt");
    engine.receive_trigger(
        Trigger {
            id: "trig-1".to_owned(),
            source_id: "feishu-main".to_owned(),
            source_kind: SourceKindId::Feishu,
            external_id: "om_1".to_owned(),
            thread_key: "feishu:topic:r1".to_owned(),
            actor_id: "ou_1".to_owned(),
            actor_is_bot: false,
            addressed_agent: true,
            text: "@nexus hello".to_owned(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: None,
            at: "2026-11-03T03:00:00Z".to_owned(),
            meta: BTreeMap::new(),
        },
        AdmissionStrength::Explicit,
    );

    store
        .save_engine_state(&engine.state, "2026-11-03T03:00:01Z")
        .unwrap();
    let loaded = store.load_engine_state().unwrap().unwrap();
    assert!(loaded.triggers.contains_key("trig-1"));

    store
        .set_cursor(
            "feishu-main",
            "chat:oc_1",
            "1730590000000",
            "2026-11-03T03:00:02Z",
        )
        .unwrap();
    assert_eq!(
        store.get_cursor("feishu-main", "chat:oc_1").unwrap(),
        Some("1730590000000".to_owned())
    );

    store
        .set_setting(
            "accept_mode",
            &serde_json::json!("auto"),
            "2026-11-03T03:00:03Z",
        )
        .unwrap();
    assert_eq!(
        store.get_setting("accept_mode").unwrap(),
        Some(serde_json::json!("auto"))
    );
}

#[test]
fn provisions_executable_worker_shims_and_gitconfig() {
    let tmp_dir = std::env::temp_dir().join(format!("nexus-shims-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);

    let shims = provision_worker_shims(&ShimConfig {
        state_dir: tmp_dir.clone(),
        github_token: Some("ghp_test_token_123".to_owned()),
        github_config_dir: None,
        lark_profile: Some("bot_prod".to_owned()),
        gws_account: Some("agent@company.com".to_owned()),
        nexus_bin_path: None,
    })
    .unwrap();

    for path in [
        &shims.gh_agent_path,
        &shims.lark_bot_path,
        &shims.gws_agent_path,
        &shims.dws_agent_path,
        &shims.nexus_worker_path,
    ] {
        let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }

    let lark_content = std::fs::read_to_string(&shims.lark_bot_path).unwrap();
    assert!(lark_content.contains("lark-cli --profile 'bot_prod'"));

    let gitconfig = std::fs::read_to_string(&shims.gitconfig_path).unwrap();
    assert!(gitconfig.contains("insteadOf = git@github.com:"));
    assert!(gitconfig.contains("auth git-credential"));

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

#[tokio::test]
async fn watchdog_detects_worker_loop_via_jev_and_escalates_think_effort() {
    struct LoopingJev;
    impl JevClassifier for LoopingJev {
        async fn classify(&self, _ctx: &ClassifierContext) -> anyhow::Result<ClassifierResult> {
            Ok(ClassifierResult {
                model: "jev-latest".to_owned(),
                answers: BTreeMap::from([
                    (
                        "is_looping".to_owned(),
                        ClassifierAnswer::Bool { probability: 0.94 },
                    ),
                    (
                        "is_drifting".to_owned(),
                        ClassifierAnswer::Bool { probability: 0.10 },
                    ),
                ]),
            })
        }
    }

    let mut engine = Engine::new("/tmp/nexus-wt");
    engine.receive_trigger(
        Trigger {
            id: "trig-w1".to_owned(),
            source_id: "feishu-main".to_owned(),
            source_kind: SourceKindId::Feishu,
            external_id: "om_w1".to_owned(),
            thread_key: "feishu:topic:rw1".to_owned(),
            actor_id: "ou_1".to_owned(),
            actor_is_bot: false,
            addressed_agent: true,
            text: "@nexus fix flaky test".to_owned(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: None,
            at: "2026-11-03T03:10:00Z".to_owned(),
            meta: BTreeMap::new(),
        },
        AdmissionStrength::Explicit,
    );

    let ticket_id = engine
        .accept_ticket(
            "trig-w1",
            "Fix flaky test",
            Some("nexus".to_owned()),
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
                think_effort: ThinkEffort::Low,
            },
            None,
            "2026-11-03T03:10:05Z",
        )
        .unwrap();

    // Drain initial effects and verify backoff helper
    assert_eq!(compute_backoff_ms(0), 1_000);
    assert_eq!(compute_backoff_ms(3), 8_000);
    if let Some(eff) = pop_next_effect(&mut engine) {
        record_effect_failure(&mut engine, eff, "transient network error");
    }

    let verdict = inspect_and_recover_worker(
        &mut engine,
        &LoopingJev,
        &ticket_id,
        "cargo test failed 5 times with identical assertion error",
        "M tests/flaky.rs",
        "2026-11-03T03:15:00Z",
    )
    .await
    .unwrap();

    assert!(matches!(
        verdict,
        WatchdogVerdict::EscalatedRecovery { ref new_target, .. }
        if new_target.think_effort == ThinkEffort::High
    ));
    assert_eq!(
        engine.state.tickets[&ticket_id]
            .current_target()
            .think_effort,
        ThinkEffort::High
    );
}
