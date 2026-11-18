use nexus::config::NexusConfig;
use nexus::core::engine::Engine;
use nexus::core::model::{
    AdmissionStrength, AssignmentReport, AssignmentStatus, ChannelAddress, ExecutionTarget,
    HarnessKind, ReportStatus, SourceKindId, ThinkEffort, Trigger,
};
use nexus::http::{SharedAppState, build_http_router};
use nexus::infra::store::SqliteStore;
use nexus::router::jev_http::HeuristicJevClassifier;
use nexus::router::llm::{LlmClient, LlmCompletionRequest, LlmCompletionResponse, LlmToolCall};
use nexus::runtime::{NexusRuntime, RuntimeTickSummary};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

const NOW: &str = "2026-11-18T04:00:00Z";

#[test]
fn an_interrupted_issue_can_resume_from_a_follow_up_after_restart() {
    let cfg = NexusConfig::default_with_state_dir(
        std::env::temp_dir().join(format!("nexus-recover-issue-{}", std::process::id())),
    );
    let (mut engine, ticket_id) = accept_issue(&cfg);
    engine.state.effects.clear();
    let current = engine
        .state
        .tickets
        .get_mut(&ticket_id)
        .unwrap()
        .latest_assignment_mut()
        .unwrap();
    current.status = AssignmentStatus::Running;
    current.pending_report = Some(AssignmentReport {
        status: ReportStatus::Ready,
        summary: "Unverified result".to_owned(),
        question: None,
        pull_request_url: Some("https://github.com/acme/widget/pull/999".to_owned()),
        at: NOW.to_owned(),
    });
    let worktree = current.worktree_dir.clone();
    let store = Arc::new(SqliteStore::open(Path::new(":memory:")).unwrap());
    store.save_engine_state(&engine.state, NOW).unwrap();
    let engine = Arc::new(Mutex::new(Engine::from_state(
        store.load_engine_state().unwrap().unwrap(),
    )));
    let _runtime = NexusRuntime::new(
        Arc::clone(&engine),
        Arc::clone(&store),
        Arc::new(Mutex::new(cfg.clone())),
        HeuristicJevClassifier,
        IssueWorker {
            mode: "publish",
            launches: Mutex::new(Vec::new()),
        },
    )
    .unwrap();
    let mut engine = engine.lock().unwrap();
    let ticket = &store.load_engine_state().unwrap().unwrap().tickets[&ticket_id];
    let recovered = ticket.latest_assignment().unwrap();
    assert!(ticket.is_open());
    assert!(ticket.follows.is_empty());
    assert_eq!(recovered.status, AssignmentStatus::Ended);
    assert_eq!(
        recovered.latest_report.as_ref().unwrap().status,
        ReportStatus::Blocked
    );
    assert!(recovered.pending_report.is_none());

    let mut follow_up = engine.state.triggers["issue-8"].clone();
    follow_up.id = "follow-up".to_owned();
    follow_up.external_id = "follow-up".to_owned();
    follow_up.thread_key = "github:issue:acme/widget#19".to_owned();
    engine.receive_trigger(follow_up, AdmissionStrength::Explicit);
    engine
        .attach_trigger_to_ticket("follow-up", &ticket_id, NOW)
        .unwrap();
    let resumed = engine.state.tickets[&ticket_id]
        .latest_assignment()
        .unwrap();
    assert_eq!(resumed.status, AssignmentStatus::Pending);
    assert_eq!(resumed.worktree_dir, worktree);
}

struct IssueWorker {
    mode: &'static str,
    launches: Mutex<Vec<String>>,
}

impl LlmClient for IssueWorker {
    async fn complete(
        &self,
        request: &LlmCompletionRequest,
    ) -> anyhow::Result<LlmCompletionResponse> {
        let mut launches = self.launches.lock().unwrap();
        let tool = if request.messages.len() == 2 {
            launches.push(request.messages[1].content.clone());
            let command = if self.mode == "read_only" {
                "git status --short"
            } else if launches.len() > 1 && self.mode != "unpublished" {
                "git add fix.txt && git -c user.name=Nexus -c user.email=nexus@example.com commit -qm fix"
            } else {
                "printf 'fix\n' > fix.txt"
            };
            ("run_command", serde_json::json!({ "command": command }))
        } else if self.mode == "read_only" {
            (
                "worker_report",
                serde_json::json!({
                    "status": "ready", "summary": "Read-only: The login setting is documented."
                }),
            )
        } else if self.mode == "question" {
            (
                "worker_report",
                serde_json::json!({
                    "status": "waiting", "summary": "Need the failing fixture",
                    "question": "Could you upload the failing fixture?"
                }),
            )
        } else if self.mode == "unpublished" {
            (
                "worker_report",
                serde_json::json!({
                    "status": "blocked", "summary": "Implementation is not finished",
                    "question": "Should I keep going?"
                }),
            )
        } else {
            (
                "worker_report",
                serde_json::json!({
                    "status": "ready", "summary": "The fix is complete",
                    "pull_request_url": "https://github.com/acme/widget/pull/999"
                }),
            )
        };
        Ok(LlmCompletionResponse {
            content: String::new(),
            tool_calls: vec![LlmToolCall {
                id: "test-tool".to_owned(),
                name: tool.0.to_owned(),
                arguments: tool.1,
            }],
        })
    }
}

fn git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn accept_issue(cfg: &NexusConfig) -> (Engine, String) {
    let mut engine = Engine::new(cfg.worktree_root.to_string_lossy());
    engine.receive_trigger(
        Trigger {
            id: "issue-8".to_owned(),
            source_id: cfg.github.source_id.clone(),
            source_kind: SourceKindId::Github,
            external_id: "8".to_owned(),
            thread_key: "github:issue:acme/widget#8".to_owned(),
            actor_id: "alice".to_owned(),
            actor_is_bot: false,
            addressed_agent: true,
            text: "Fix the login crash".to_owned(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: Some(ChannelAddress::GithubIssueComment {
                source_id: cfg.github.source_id.clone(),
                repo: "acme/widget".to_owned(),
                number: 8,
            }),
            at: NOW.to_owned(),
            meta: BTreeMap::new(),
        },
        AdmissionStrength::Explicit,
    );
    let ticket_id = engine
        .accept_ticket(
            "issue-8",
            "Fix the login crash",
            Some("acme/widget".to_owned()),
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "test".to_owned(),
                model: "test".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            None,
            NOW,
        )
        .unwrap();
    (engine, ticket_id)
}

#[tokio::test]
async fn github_issue_continues_unpublished_work_and_reports_only_the_verified_pr() {
    for mode in [
        "publish",
        "no_base",
        "unpublished",
        "stale_pr",
        "question",
        "read_only",
    ] {
        let scratch =
            std::env::temp_dir().join(format!("nexus-issue-{}-{mode}", std::process::id()));
        let checkout = scratch.join("repo");
        std::fs::create_dir_all(&checkout).unwrap();
        git(&checkout, &["init", "-q", "-b", "main"]);
        git(
            &checkout,
            &[
                "-c",
                "user.name=Nexus",
                "-c",
                "user.email=nexus@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "base",
            ],
        );
        if mode != "no_base" {
            git(
                &checkout,
                &["update-ref", "refs/remotes/origin/main", "HEAD"],
            );
        }

        let mut cfg = NexusConfig::default_with_state_dir(scratch.clone());
        cfg.repositories.insert("acme/widget".to_owned(), checkout);
        cfg.github.gh_bin = scratch.join("mock-gh");
        let oid = if mode == "stale_pr" {
            "old-commit"
        } else {
            "$(git rev-parse HEAD)"
        };
        std::fs::write(&cfg.github.gh_bin, format!(r#"#!/bin/sh
if [ "$1 $2" = "pr list" ]; then
  printf '[{{"url":"https://github.com/acme/widget/pull/12","state":"OPEN","headRefOid":"%s"}}]\n' "{oid}"
fi
"#)).unwrap();
        std::fs::set_permissions(&cfg.github.gh_bin, std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let store = Arc::new(SqliteStore::open(&scratch.join("nexus.db")).unwrap());
        let (engine, ticket_id) = accept_issue(&cfg);
        let engine = Arc::new(Mutex::new(engine));
        let mut runtime = NexusRuntime::new(
            Arc::clone(&engine),
            store,
            Arc::new(Mutex::new(cfg)),
            HeuristicJevClassifier,
            IssueWorker {
                mode,
                launches: Mutex::new(Vec::new()),
            },
        )
        .unwrap();
        runtime
            .drain_effects(&mut RuntimeTickSummary::default(), NOW)
            .await
            .unwrap();
        {
            let engine = engine.lock().unwrap();
            let ticket = &engine.state.tickets[&ticket_id];
            let launches = runtime.llm.launches.lock().unwrap();
            let report = ticket
                .latest_assignment()
                .unwrap()
                .latest_report
                .as_ref()
                .unwrap();
            match mode {
                "publish" | "no_base" => {
                    assert!(ticket.is_open());
                    assert_eq!(launches.len(), 2);
                    assert!(launches[1].contains("fix.txt"));
                    assert_eq!(
                        ticket.assignments[0].worktree_dir,
                        ticket.assignments[1].worktree_dir
                    );
                    assert_eq!(report.status, ReportStatus::Ready);
                    assert_eq!(
                        report.pull_request_url.as_deref(),
                        Some("https://github.com/acme/widget/pull/12")
                    );
                    assert!(
                        ticket
                            .follows
                            .iter()
                            .any(|follow| follow.thread_key == "github:pr:acme/widget#12")
                    );
                }
                "unpublished" => {
                    assert!(ticket.is_open());
                    assert_eq!(launches.len(), 2);
                    assert_eq!(report.status, ReportStatus::Blocked);
                    assert!(report.pull_request_url.is_none());
                    assert!(report.summary.contains("uncommitted"));
                }
                "stale_pr" => {
                    assert!(ticket.is_open());
                    assert_eq!(launches.len(), 2);
                    assert_eq!(report.status, ReportStatus::Blocked);
                    assert!(report.pull_request_url.is_none());
                    assert!(report.summary.contains("current commit"));
                }
                "question" => {
                    assert!(ticket.is_open());
                    assert_eq!(launches.len(), 1);
                    assert_eq!(report.status, ReportStatus::Waiting);
                    assert_eq!(
                        report.question.as_deref(),
                        Some("Could you upload the failing fixture?")
                    );
                }
                "read_only" => {
                    assert!(!ticket.is_open());
                    assert_eq!(launches.len(), 1);
                    assert_eq!(report.status, ReportStatus::Ready);
                    assert_eq!(report.summary, "The login setting is documented.");
                }
                _ => unreachable!(),
            }
        }
        drop(runtime);
        drop(engine);
        std::fs::remove_dir_all(scratch).unwrap();
    }
}

#[tokio::test]
async fn github_issue_defers_reports_only_from_the_current_attempt() {
    let config =
        NexusConfig::default_with_state_dir(std::env::temp_dir().join("nexus-issue-reports"));
    let (mut engine, ticket_id) = accept_issue(&config);
    let current = engine
        .state
        .tickets
        .get_mut(&ticket_id)
        .unwrap()
        .latest_assignment_mut()
        .unwrap();
    current.status = AssignmentStatus::Running;
    let assignment_id = current.id.clone();
    let state = SharedAppState {
        engine: Arc::new(Mutex::new(engine)),
        store: Arc::new(SqliteStore::open(Path::new(":memory:")).unwrap()),
        config: Arc::new(Mutex::new(config)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/api/worker/report",
        listener.local_addr().unwrap()
    );
    let router = build_http_router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    for (reported_id, report_status, status) in [
        (
            "previous-assignment",
            "ready",
            reqwest::StatusCode::CONFLICT,
        ),
        (&assignment_id, "progress", reqwest::StatusCode::OK),
        (&assignment_id, "ready", reqwest::StatusCode::OK),
    ] {
        let response = client
            .post(&endpoint)
            .json(&serde_json::json!({
                "ticket_id": ticket_id,
                "assignment_id": reported_id,
                "status": report_status,
                "summary": "The fix is complete",
                "pull_request_thread_key": "github:pr:acme/widget#999",
                "pull_request_url": "https://github.com/acme/widget/pull/999"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    {
        let engine = state.engine.lock().unwrap();
        let ticket = &engine.state.tickets[&ticket_id];
        let assignment = ticket.latest_assignment().unwrap();
        assert!(ticket.is_open());
        assert_eq!(assignment.status, AssignmentStatus::Running);
        let progress = assignment.latest_report.as_ref().unwrap();
        assert_eq!(progress.status, ReportStatus::Progress);
        assert!(progress.pull_request_url.is_none());
        assert_eq!(
            assignment.pending_report.as_ref().unwrap().status,
            ReportStatus::Ready
        );
        assert!(ticket.follows.is_empty());
    }
    server.abort();
}
