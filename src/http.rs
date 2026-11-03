//! Local `axum` Management Console & Worker-Callback HTTP API (`127.0.0.1:3082`).

use crate::config::{NexusConfig, probe_environment};
use crate::core::engine::Engine;
use crate::core::gate::{evaluate_gate_strength, parse_inline_target_directive};
use crate::core::model::{
    AcceptMode, AdmissionStrength, ChannelAddress, ExecutionTarget, HarnessKind, ReportStatus,
    SourceKindId, ThinkEffort, Trigger,
};
use crate::core::recall::RecallCaller;
use crate::infra::store::SqliteStore;
use crate::router::jev::{
    JevClassifier, JevTriageInput, build_jev_triage_context, run_jev_triage,
};
use crate::router::jev_http::{HeuristicJevClassifier, HttpJevClient, HttpJevConfig};
use crate::router::orchestrator::RouterLlmConfig;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const DASHBOARD_HTML: &str = include_str!("dashboard.html");

/// Shared daemon state accessible by the `axum` HTTP server and the background runtime loop.
#[derive(Clone)]
pub struct SharedAppState {
    pub engine: Arc<Mutex<Engine>>,
    pub store: Arc<SqliteStore>,
    pub config: Arc<Mutex<NexusConfig>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerReportPayload {
    pub ticket_id: String,
    pub status: ReportStatus,
    pub summary: String,
    pub question: Option<String>,
    pub pull_request_thread_key: Option<String>,
    pub pull_request_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerReadQuery {
    pub ticket_id: Option<String>,
    pub uri: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerHandbackPayload {
    pub ticket_id: String,
    pub trigger_id: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerSwitchTargetPayload {
    pub ticket_id: String,
    pub harness: HarnessKind,
    pub provider: String,
    pub model: String,
    pub think_effort: ThinkEffort,
    pub reason: String,
    pub git_status_diff: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AcceptModePayload {
    pub accept_mode: AcceptMode,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StopTicketPayload {
    pub ticket_id: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryPayload {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SimulateChatPayload {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateConfigPayload {
    pub bot_name: Option<String>,
    pub reply_in_thread: Option<bool>,
    pub ack_reaction: Option<String>,
    pub wake_keywords: Option<Vec<String>>,
    pub chat_ids: Option<Vec<String>>,
    pub github_repos: Option<Vec<String>>,
    pub allowed_actors: Option<Vec<String>>,
    pub default_target: Option<ExecutionTarget>,
    pub jev_enabled: Option<bool>,
    pub jev: Option<HttpJevConfig>,
    pub router_llm: Option<RouterLlmConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestJevPayload {
    pub text: String,
}

/// Builds the `axum::Router` for the management console and worker-callback HTTP server.
pub fn build_http_router(state: SharedAppState) -> Router {
    Router::new()
        .route("/", get(handle_dashboard))
        .route("/api/overview", get(handle_get_overview))
        .route("/api/state", get(handle_get_state))
        .route("/api/tickets", get(handle_get_tickets))
        .route("/api/tickets/stop", post(handle_stop_ticket))
        .route("/api/triggers", post(handle_post_trigger))
        .route("/api/chat/simulate", post(handle_simulate_chat))
        .route("/api/jev/test", post(handle_test_jev))
        .route("/api/config", get(handle_get_config).post(handle_update_config))
        .route("/api/memory", get(handle_get_memory).post(handle_save_memory))
        .route("/api/worker/report", post(handle_worker_report))
        .route("/api/worker/read", get(handle_worker_read))
        .route("/api/worker/handback", post(handle_worker_handback))
        .route(
            "/api/worker/switch-target",
            post(handle_worker_switch_target),
        )
        .route("/api/settings/accept-mode", post(handle_set_accept_mode))
        .with_state(state)
}

async fn handle_dashboard() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

async fn handle_get_overview(State(app): State<SharedAppState>) -> Json<serde_json::Value> {
    let state_val = {
        let eng = app.engine.lock().unwrap();
        serde_json::to_value(&eng.state).unwrap_or_default()
    };
    let (cfg_val, mem_path) = {
        let cfg = app.config.lock().unwrap();
        (
            serde_json::to_value(&*cfg).unwrap_or_default(),
            cfg.state_dir.join("MEMORY.md"),
        )
    };
    let memory = std::fs::read_to_string(mem_path).unwrap_or_default();
    let toolchain = probe_environment();

    Json(serde_json::json!({
        "state": state_val,
        "config": cfg_val,
        "memory": memory,
        "toolchain": toolchain,
    }))
}

async fn handle_get_state(State(app): State<SharedAppState>) -> Json<serde_json::Value> {
    let eng = app.engine.lock().unwrap();
    Json(serde_json::to_value(&eng.state).unwrap_or_default())
}

async fn handle_get_tickets(State(app): State<SharedAppState>) -> Json<serde_json::Value> {
    let eng = app.engine.lock().unwrap();
    let list: Vec<_> = eng.state.tickets.values().cloned().collect();
    Json(serde_json::json!({ "tickets": list }))
}

async fn handle_stop_ticket(
    State(app): State<SharedAppState>,
    Json(payload): Json<StopTicketPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let mut eng = app.engine.lock().unwrap();
    match eng.stop_ticket(&payload.ticket_id, &payload.reason, &now) {
        Ok(()) => {
            let _ = app.store.save_engine_state(&eng.state, &now);
            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

async fn handle_get_config(State(app): State<SharedAppState>) -> Json<serde_json::Value> {
    let cfg = app.config.lock().unwrap();
    Json(serde_json::to_value(&*cfg).unwrap_or_default())
}

async fn handle_update_config(
    State(app): State<SharedAppState>,
    Json(payload): Json<UpdateConfigPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let mut cfg = app.config.lock().unwrap();
    if let Some(name) = payload.bot_name
        && !name.is_empty()
    {
        cfg.feishu.bot_name = name;
    }
    if let Some(reply_in_thread) = payload.reply_in_thread {
        cfg.feishu.reply_in_thread = reply_in_thread;
    }
    if let Some(ack_reaction) = payload.ack_reaction {
        cfg.feishu.ack_reaction = if ack_reaction.trim().is_empty() {
            None
        } else {
            Some(ack_reaction.trim().to_owned())
        };
    }
    if let Some(wake) = payload.wake_keywords {
        cfg.feishu.wake_keywords = wake;
    }
    if let Some(chats) = payload.chat_ids {
        cfg.feishu.chat_ids = chats;
    }
    if let Some(repos) = payload.github_repos {
        cfg.github.repos = repos;
    }
    if let Some(actors) = payload.allowed_actors {
        cfg.allowed_actors = actors.into_iter().collect();
    }
    if let Some(target) = payload.default_target {
        for rule in &mut cfg.proactive_rules {
            rule.default_target = Some(target.clone());
        }
    }
    if let Some(enabled) = payload.jev_enabled {
        if enabled {
            if let Some(jcfg) = payload.jev {
                cfg.jev = Some(jcfg);
            }
        } else {
            cfg.jev = None;
        }
    } else if let Some(jcfg) = payload.jev {
        cfg.jev = Some(jcfg);
    }
    if let Some(rllm) = payload.router_llm {
        cfg.router_llm = rllm;
    }
    let _ = cfg.save();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "config": &*cfg })),
    )
}

async fn handle_get_memory(State(app): State<SharedAppState>) -> Json<serde_json::Value> {
    let path = app.config.lock().unwrap().state_dir.join("MEMORY.md");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    Json(serde_json::json!({
        "path": path,
        "content": content,
    }))
}

async fn handle_test_jev(
    State(app): State<SharedAppState>,
    Json(payload): Json<TestJevPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let (cleaned_text, inline_target) = parse_inline_target_directive(&payload.text);
    let (maybe_jcfg, rules, accept_mode, addressed) = {
        let cfg = app.config.lock().unwrap();
        let addr = crate::adapters::feishu::is_feishu_message_addressed(
            &cfg.feishu,
            "group",
            &cleaned_text,
            false,
        );
        (
            cfg.jev.clone(),
            cfg.proactive_rules.clone(),
            cfg.accept_mode,
            addr,
        )
    };

    let trig = Trigger {
        id: "test-trig-1".to_owned(),
        source_id: "feishu-main".to_owned(),
        source_kind: SourceKindId::Feishu,
        external_id: "test_1".to_owned(),
        thread_key: "test:thread:1".to_owned(),
        actor_id: "admin".to_owned(),
        actor_is_bot: false,
        addressed_agent: addressed,
        text: cleaned_text,
        reply_to_trigger_id: None,
        inline_target,
        reply_channel: None,
        at: chrono::Utc::now().to_rfc3339(),
        meta: BTreeMap::new(),
    };

    let triggers = vec![trig];
    let strength = if addressed {
        AdmissionStrength::Explicit
    } else {
        AdmissionStrength::Inferred
    };
    let input = JevTriageInput {
        triggers: &triggers,
        strength,
        accept_mode,
        thread_messages: &[],
        open_tickets: &[],
        rules: &rules,
    };
    let ctx = build_jev_triage_context(&input);

    let (mode_used, classify_res, outcome) = match maybe_jcfg {
        Some(jcfg) if !jcfg.base_url.trim().is_empty() && !jcfg.api_key.trim().is_empty() => {
            let client = HttpJevClient::new(jcfg);
            match client.classify(&ctx).await {
                Ok(res) => {
                    let out = run_jev_triage(&client, &input).await;
                    ("remote-http-jev", Ok(res), format!("{out:?}"))
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    let h = HeuristicJevClassifier;
                    let res = h.classify(&ctx).await;
                    let out = run_jev_triage(&h, &input).await;
                    (
                        "fallback-heuristic-jev",
                        res.map_err(|_| e),
                        format!("{out:?} (remote error: {err_msg})"),
                    )
                }
            }
        }
        _ => {
            let h = HeuristicJevClassifier;
            let res = h.classify(&ctx).await;
            let out = run_jev_triage(&h, &input).await;
            ("local-heuristic-jev", res, format!("{out:?}"))
        }
    };

    match classify_res {
        Ok(res) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "mode": mode_used,
                "addressed": addressed,
                "strength": strength,
                "result": res,
                "triage_outcome": outcome,
            })),
        ),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "ok": false,
                "mode": mode_used,
                "error": e.to_string(),
            })),
        ),
    }
}

async fn handle_save_memory(
    State(app): State<SharedAppState>,
    Json(payload): Json<MemoryPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let path = app.config.lock().unwrap().state_dir.join("MEMORY.md");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &payload.content) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
        ),
    }
}

async fn handle_simulate_chat(
    State(app): State<SharedAppState>,
    Json(payload): Json<SimulateChatPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let ts_ms = chrono::Utc::now().timestamp_millis();
    let msg_id = format!("admin_{ts_ms}");
    let (cleaned_text, inline_target) = parse_inline_target_directive(&payload.text);

    let (source_id, chat_id, actor_id, policy) = {
        let cfg = app.config.lock().unwrap();
        let cid = cfg
            .feishu
            .chat_ids
            .first()
            .cloned()
            .unwrap_or_else(|| "admin_console".to_owned());
        let aid = cfg
            .allowed_actors
            .iter()
            .next()
            .cloned()
            .unwrap_or_else(|| "admin".to_owned());
        (cfg.feishu.source_id.clone(), cid, aid, cfg.admission_policy())
    };

    let reply_channel = if chat_id != "admin_console" {
        Some(ChannelAddress::FeishuTopic {
            source_id: source_id.clone(),
            chat_id: chat_id.clone(),
            root_message_id: msg_id.clone(),
            reply_to_message_id: msg_id.clone(),
        })
    } else {
        None
    };

    let trigger = Trigger {
        id: format!("admin-trig-{ts_ms}"),
        source_id,
        source_kind: SourceKindId::Feishu,
        external_id: msg_id.clone(),
        thread_key: format!("admin:chat:{chat_id}:{msg_id}"),
        actor_id: actor_id.clone(),
        actor_is_bot: false,
        addressed_agent: true,
        text: cleaned_text,
        reply_to_trigger_id: None,
        inline_target,
        reply_channel,
        at: now.clone(),
        meta: BTreeMap::new(),
    };

    let mut eng = app.engine.lock().unwrap();
    let accept_mode = eng.state.accept_mode.unwrap_or(AcceptMode::Auto);
    let strength = evaluate_gate_strength(&actor_id, false, true, &policy, accept_mode);
    let disp = eng.receive_trigger(trigger, strength);
    let _ = app.store.save_engine_state(&eng.state, &now);

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "disposition": format!("{disp:?}"),
        })),
    )
}

async fn handle_post_trigger(
    State(app): State<SharedAppState>,
    Json(trigger): Json<Trigger>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let policy = app.config.lock().unwrap().admission_policy();
    let mut eng = app.engine.lock().unwrap();
    let accept_mode = eng.state.accept_mode.unwrap_or(AcceptMode::Auto);
    let strength = evaluate_gate_strength(
        &trigger.actor_id,
        trigger.actor_is_bot,
        trigger.addressed_agent,
        &policy,
        accept_mode,
    );
    let disp = eng.receive_trigger(trigger, strength);
    let _ = app.store.save_engine_state(&eng.state, &now);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "ok": true,
            "strength": strength,
            "disposition": format!("{disp:?}"),
        })),
    )
}

async fn handle_worker_report(
    State(app): State<SharedAppState>,
    Json(payload): Json<WorkerReportPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let mut eng = app.engine.lock().unwrap();
    match eng.record_worker_report(
        &payload.ticket_id,
        payload.status,
        payload.summary,
        payload.question,
        payload.pull_request_thread_key,
        payload.pull_request_url,
        &now,
    ) {
        Ok(()) => {
            let _ = app.store.save_engine_state(&eng.state, &now);
            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

async fn handle_worker_read(
    State(app): State<SharedAppState>,
    Query(query): Query<WorkerReadQuery>,
) -> (StatusCode, Json<serde_json::Value>) {
    let eng = app.engine.lock().unwrap();
    let caller = match query.ticket_id {
        Some(tid) if !tid.is_empty() => RecallCaller::Worker { ticket_id: tid },
        _ => RecallCaller::MainRouter,
    };
    match eng.read_recall_resource(&caller, &query.uri) {
        Ok(val) => (StatusCode::OK, Json(val)),
        Err(e) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

async fn handle_worker_handback(
    State(app): State<SharedAppState>,
    Json(payload): Json<WorkerHandbackPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let mut eng = app.engine.lock().unwrap();
    match eng.handback_trigger(
        &payload.ticket_id,
        &payload.trigger_id,
        &payload.reason,
        &now,
    ) {
        Ok(handed_back) => {
            let _ = app.store.save_engine_state(&eng.state, &now);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "handed_back": handed_back })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

async fn handle_worker_switch_target(
    State(app): State<SharedAppState>,
    Json(payload): Json<WorkerSwitchTargetPayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    let mut eng = app.engine.lock().unwrap();
    let target = ExecutionTarget {
        harness: payload.harness,
        provider: payload.provider,
        model: payload.model,
        think_effort: payload.think_effort,
    };
    match eng.switch_execution_target(
        &payload.ticket_id,
        target,
        &payload.reason,
        payload.git_status_diff.as_deref().unwrap_or(""),
        &now,
    ) {
        Ok(assignment_id) => {
            let _ = app.store.save_engine_state(&eng.state, &now);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "assignment_id": assignment_id })),
            )
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": e })),
        ),
    }
}

async fn handle_set_accept_mode(
    State(app): State<SharedAppState>,
    Json(payload): Json<AcceptModePayload>,
) -> (StatusCode, Json<serde_json::Value>) {
    let now = chrono::Utc::now().to_rfc3339();
    {
        let mut cfg = app.config.lock().unwrap();
        cfg.accept_mode = payload.accept_mode;
        let _ = cfg.save();
    }
    let mut eng = app.engine.lock().unwrap();
    eng.state.accept_mode = Some(payload.accept_mode);
    let _ = app.store.save_engine_state(&eng.state, &now);
    let _ = app.store.set_setting(
        "accept_mode",
        &serde_json::json!(payload.accept_mode),
        &now,
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "accept_mode": payload.accept_mode })),
    )
}
