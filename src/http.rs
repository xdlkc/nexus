//! Local `axum` Management & Worker-Callback HTTP API (`127.0.0.1:3082`).

use crate::config::NexusConfig;
use crate::core::engine::Engine;
use crate::core::gate::evaluate_gate_strength;
use crate::core::model::{
    AcceptMode, ExecutionTarget, HarnessKind, ReportStatus, ThinkEffort, Trigger,
};
use crate::core::recall::RecallCaller;
use crate::infra::store::SqliteStore;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::Html,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

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

/// Builds the `axum::Router` for the management and worker-callback HTTP server.
pub fn build_http_router(state: SharedAppState) -> Router {
    Router::new()
        .route("/", get(handle_dashboard))
        .route("/api/state", get(handle_get_state))
        .route("/api/tickets", get(handle_get_tickets))
        .route("/api/triggers", post(handle_post_trigger))
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

async fn handle_dashboard(State(app): State<SharedAppState>) -> Html<String> {
    let eng = app.engine.lock().unwrap();
    let open_count = eng.state.tickets.values().filter(|t| t.is_open()).count();
    let total_tickets = eng.state.tickets.len();
    let total_triggers = eng.state.triggers.len();
    let pending_outbox = eng
        .state
        .outbox
        .values()
        .filter(|m| m.status == crate::core::model::OutboxStatus::Pending)
        .count();

    let mut rows = String::new();
    for t in eng.state.tickets.values() {
        let target = t.current_target();
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td><code>{:?}</code></td><td><code>{}:{}/{}:{}</code></td><td><code>{}</code></td></tr>",
            t.id,
            t.title,
            t.stage(),
            target.harness.as_str(),
            target.provider,
            target.model,
            target.think_effort.as_str(),
            t.origin_thread_key
        ));
    }

    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Nexus Control Plane</title>\
         <style>body{{font-family:system-ui,sans-serif;margin:2rem;background:#0f172a;color:#f8fafc}}\
         table{{width:100%;border-collapse:collapse;margin-top:1rem}}\
         th,td{{border:1px solid #334155;padding:0.6rem;text-align:left}}\
         code{{background:#1e293b;padding:0.15rem 0.35rem;border-radius:4px}}</style></head>\
         <body><h1>Nexus Super-Agent Control Plane</h1>\
         <p>Open Tickets: <strong>{open_count}</strong> / {total_tickets} | Triggers: <strong>{total_triggers}</strong> | Pending Outbox: <strong>{pending_outbox}</strong></p>\
         <table><thead><tr><th>ID</th><th>Title</th><th>Stage</th><th>Execution Target</th><th>Origin Thread</th></tr></thead>\
         <tbody>{rows}</tbody></table></body></html>"
    );
    Html(html)
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
