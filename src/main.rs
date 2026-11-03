use clap::{Parser, Subcommand};
use nexus::config::{NexusConfig, probe_environment};
use nexus::core::engine::Engine;
use nexus::core::model::{HarnessKind, ReportStatus, ThinkEffort};
use nexus::http::{
    SharedAppState, WorkerHandbackPayload, WorkerReportPayload, WorkerSwitchTargetPayload,
    build_http_router,
};
use nexus::infra::shims::{ShimConfig, provision_worker_shims};
use nexus::infra::store::SqliteStore;
use nexus::router::jev::{ClassifierContext, ClassifierResult, JevClassifier};
use nexus::router::jev_http::{HeuristicJevClassifier, HttpJevClient};
use nexus::router::llm::HttpLlmClient;
use nexus::runtime::NexusRuntime;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[command(
    name = "nexus",
    version,
    about = "Nexus Super-Agent Control Plane (Dual-Engine Router + Polymorphic Worker Pool)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Probe installed CLI tools (`lark-cli`, `gh`, `claude`, `codex`, `pi`, `opencode`),
    /// provision `<state_dir>/worker-bin` shims, and write `config.json`.
    Setup {
        #[arg(long, default_value = ".nexus")]
        state_dir: PathBuf,
        #[arg(long)]
        feishu_chat: Vec<String>,
        #[arg(long)]
        lark_profile: Option<String>,
        #[arg(long)]
        github_repo: Vec<String>,
    },
    /// Start the resident Nexus daemon (`127.0.0.1:3082` + Feishu/GitHub polling + Worker pool).
    Start {
        #[arg(long, default_value = ".nexus")]
        state_dir: PathBuf,
        /// Run a single deterministic poll + effect drain cycle and exit.
        #[arg(long)]
        once: bool,
    },
    /// Query the live status of a running Nexus daemon.
    Status {
        #[arg(long, default_value = "http://127.0.0.1:3082")]
        api_url: String,
    },
    /// Subcommands invoked by Worker Harnesses (`nexus worker report|read|handback|switch-target`).
    Worker {
        #[command(subcommand)]
        action: WorkerCommands,
    },
}

#[derive(Subcommand, Debug)]
enum WorkerCommands {
    /// Report structured worker outcome (`ready`, `waiting`, `blocked`, `failed`, `dropped`).
    Report {
        #[arg(long)]
        ticket_id: Option<String>,
        #[arg(long, default_value = "ready")]
        status: String,
        #[arg(long)]
        summary: String,
        #[arg(long)]
        question: Option<String>,
        #[arg(long)]
        pr_thread: Option<String>,
        #[arg(long)]
        pr_url: Option<String>,
        #[arg(long)]
        api_url: Option<String>,
    },
    /// Read a Chain of Recall resource URI (`trigger/<id>`, `thread/<key>`, `ticket/<id>`).
    Read {
        uri: String,
        #[arg(long)]
        ticket_id: Option<String>,
        #[arg(long)]
        api_url: Option<String>,
    },
    /// Hand back an unrelated follow-up Trigger to the Router evaluation queue.
    Handback {
        trigger_id: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        ticket_id: Option<String>,
        #[arg(long)]
        api_url: Option<String>,
    },
    /// Switch the Ticket's execution target (`Harness × Model × ThinkEffort`) mid-flight.
    SwitchTarget {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        #[arg(long, default_value = "high")]
        think: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        ticket_id: Option<String>,
        #[arg(long)]
        api_url: Option<String>,
    },
}

enum RuntimeJev {
    Http(HttpJevClient),
    Heuristic(HeuristicJevClassifier),
}

impl JevClassifier for RuntimeJev {
    async fn classify(&self, context: &ClassifierContext) -> anyhow::Result<ClassifierResult> {
        match self {
            Self::Http(c) => c.classify(context).await,
            Self::Heuristic(h) => h.classify(context).await,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup {
            state_dir,
            feishu_chat,
            lark_profile,
            github_repo,
        } => {
            let mut cfg = NexusConfig::load_or_default(&state_dir)?;
            if !feishu_chat.is_empty() {
                cfg.feishu.chat_ids = feishu_chat;
            }
            if lark_profile.is_some() {
                cfg.feishu.profile = lark_profile.clone();
            }
            if !github_repo.is_empty() {
                let parent_dir = std::env::current_dir()
                    .ok()
                    .and_then(|d| d.parent().map(|p| p.to_path_buf()));
                for r in &github_repo {
                    let short = r.rsplit('/').next().unwrap_or(r);
                    if let Some(base) = &parent_dir {
                        let candidate = base.join(short);
                        if candidate.exists() {
                            cfg.repositories.insert(r.clone(), candidate.clone());
                            cfg.repositories.insert(short.to_owned(), candidate);
                        }
                    }
                }
                cfg.github.repos = github_repo;
            }

            // Auto-detect Feishu profile & bot info via `lark-cli` if available
            if lark_profile.is_none()
                && let Ok(out) = std::process::Command::new(&cfg.feishu.lark_bin)
                    .arg("whoami")
                    .output()
                && out.status.success()
                && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                && let Some(prof) = val.get("profile").and_then(|v| v.as_str())
                && !prof.is_empty()
            {
                cfg.feishu.profile = Some(prof.to_owned());
            }

            if let Ok(out) = std::process::Command::new(&cfg.feishu.lark_bin)
                .args(["api", "GET", "/open-apis/bot/v3/info"])
                .output()
                && out.status.success()
                && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
            {
                if let Some(open_id) = val.pointer("/data/open_id").and_then(|v| v.as_str()) {
                    cfg.feishu.bot_open_id = Some(open_id.to_owned());
                }
                if let Some(app_name) = val.pointer("/data/app_name").and_then(|v| v.as_str()) {
                    cfg.feishu.bot_name = app_name.to_owned();
                }
            }

            // Auto-detect GitHub login via `gh api user`
            if let Ok(out) = std::process::Command::new(&cfg.github.gh_bin)
                .args(["api", "user"])
                .output()
                && out.status.success()
                && let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
                && let Some(login) = val.get("login").and_then(|v| v.as_str())
            {
                cfg.github.bot_login = login.to_owned();
            }

            let cfg_path = cfg.save()?;
            let shims = provision_worker_shims(&ShimConfig {
                state_dir: cfg.state_dir.clone(),
                github_token: std::env::var("GITHUB_TOKEN")
                    .or_else(|_| std::env::var("GH_TOKEN"))
                    .ok(),
                github_config_dir: None,
                lark_profile: cfg.feishu.profile.clone(),
                gws_account: None,
                nexus_bin_path: std::env::current_exe().ok(),
            })?;

            println!("=== Nexus Environment Probe ===");
            for p in probe_environment() {
                let mark = if p.installed { "OK" } else { "--" };
                println!("  [{mark}] {:<10} {}", p.binary, p.version_or_note);
            }
            println!("\n=== Bound Identities ===");
            println!(
                "  Feishu Bot : {} (open_id: {}, profile: {})",
                cfg.feishu.bot_name,
                cfg.feishu.bot_open_id.as_deref().unwrap_or("unconfigured"),
                cfg.feishu.profile.as_deref().unwrap_or("default")
            );
            println!("  GitHub CLI : {}", cfg.github.bot_login);
            println!("\nConfig saved to: {}", cfg_path.display());
            println!(
                "Worker shims provisioned in: {}",
                shims.worker_bin_dir.display()
            );
            println!(
                "Worker gitconfig written to: {}",
                shims.gitconfig_path.display()
            );
        }
        Commands::Start { state_dir, once } => {
            let cfg = NexusConfig::load_or_default(&state_dir)?;
            let db_path = cfg.state_dir.join("nexus.db");
            let store = Arc::new(SqliteStore::open(&db_path)?);
            let mut engine = match store.load_engine_state()? {
                Some(saved) => Engine::from_state(saved),
                None => Engine::new(cfg.worktree_root.to_string_lossy()),
            };
            engine.state.accept_mode = Some(cfg.accept_mode);

            let jev = match &cfg.jev {
                Some(jcfg) => RuntimeJev::Http(HttpJevClient::new(jcfg.clone())),
                None => RuntimeJev::Heuristic(HeuristicJevClassifier),
            };
            let llm = HttpLlmClient::from_env();

            let shared_engine = Arc::new(Mutex::new(engine));
            let shared_cfg = Arc::new(Mutex::new(cfg.clone()));

            let mut runtime = NexusRuntime::new(
                Arc::clone(&shared_engine),
                Arc::clone(&store),
                Arc::clone(&shared_cfg),
                jev,
                llm,
            )?;

            let app_state = SharedAppState {
                engine: shared_engine,
                store,
                config: shared_cfg,
            };
            let router = build_http_router(app_state);
            let listener = tokio::net::TcpListener::bind(&cfg.http_addr).await?;
            println!("Nexus Control Plane listening on http://{}", cfg.http_addr);

            if once {
                let server = tokio::spawn(async move {
                    let _ = axum::serve(listener, router).await;
                });
                let now = chrono::Utc::now().to_rfc3339();
                let summary = runtime.tick(&now).await?;
                server.abort();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "polled_triggers": summary.polled_triggers,
                        "closed_threads": summary.closed_threads,
                        "evaluated_batches": summary.evaluated_batches,
                        "launched_assignments": summary.launched_assignments,
                        "delivered_outbox_messages": summary.delivered_outbox_messages,
                    }))?
                );
                return Ok(());
            }

            tokio::spawn(async move {
                loop {
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = runtime.tick(&now).await;
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            });

            axum::serve(listener, router).await?;
        }
        Commands::Status { api_url } => {
            let url = format!("{}/api/state", api_url.trim_end_matches('/'));
            let resp: serde_json::Value = reqwest::get(&url).await?.json().await?;
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
        Commands::Worker { action } => {
            let client = reqwest::Client::new();
            match action {
                WorkerCommands::Report {
                    ticket_id,
                    status,
                    summary,
                    question,
                    pr_thread,
                    pr_url,
                    api_url,
                } => {
                    let tid = ticket_id
                        .or_else(|| std::env::var("NEXUS_TICKET_ID").ok())
                        .ok_or_else(|| anyhow::anyhow!("Missing --ticket-id or NEXUS_TICKET_ID"))?;
                    let base = api_url
                        .or_else(|| std::env::var("NEXUS_API_URL").ok())
                        .unwrap_or_else(|| "http://127.0.0.1:3082".to_owned());
                    let rep_status = match status.as_str() {
                        "waiting" => ReportStatus::Waiting,
                        "blocked" => ReportStatus::Blocked,
                        "failed" => ReportStatus::Failed,
                        "dropped" => ReportStatus::Dropped,
                        "progress" => ReportStatus::Progress,
                        _ => ReportStatus::Ready,
                    };
                    let payload = WorkerReportPayload {
                        ticket_id: tid,
                        status: rep_status,
                        summary,
                        question,
                        pull_request_thread_key: pr_thread,
                        pull_request_url: pr_url,
                    };
                    let resp: serde_json::Value = client
                        .post(format!("{}/api/worker/report", base.trim_end_matches('/')))
                        .json(&payload)
                        .send()
                        .await?
                        .json()
                        .await?;
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
                WorkerCommands::Read {
                    uri,
                    ticket_id,
                    api_url,
                } => {
                    let tid = ticket_id.or_else(|| std::env::var("NEXUS_TICKET_ID").ok());
                    let base = api_url
                        .or_else(|| std::env::var("NEXUS_API_URL").ok())
                        .unwrap_or_else(|| "http://127.0.0.1:3082".to_owned());
                    let resp: serde_json::Value = client
                        .get(format!("{}/api/worker/read", base.trim_end_matches('/')))
                        .query(&[
                            ("uri", uri.as_str()),
                            ("ticket_id", tid.as_deref().unwrap_or("")),
                        ])
                        .send()
                        .await?
                        .json()
                        .await?;
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
                WorkerCommands::Handback {
                    trigger_id,
                    reason,
                    ticket_id,
                    api_url,
                } => {
                    let tid = ticket_id
                        .or_else(|| std::env::var("NEXUS_TICKET_ID").ok())
                        .ok_or_else(|| anyhow::anyhow!("Missing --ticket-id or NEXUS_TICKET_ID"))?;
                    let base = api_url
                        .or_else(|| std::env::var("NEXUS_API_URL").ok())
                        .unwrap_or_else(|| "http://127.0.0.1:3082".to_owned());
                    let payload = WorkerHandbackPayload {
                        ticket_id: tid,
                        trigger_id,
                        reason,
                    };
                    let resp: serde_json::Value = client
                        .post(format!(
                            "{}/api/worker/handback",
                            base.trim_end_matches('/')
                        ))
                        .json(&payload)
                        .send()
                        .await?
                        .json()
                        .await?;
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
                WorkerCommands::SwitchTarget {
                    harness,
                    provider,
                    model,
                    think,
                    reason,
                    ticket_id,
                    api_url,
                } => {
                    let tid = ticket_id
                        .or_else(|| std::env::var("NEXUS_TICKET_ID").ok())
                        .ok_or_else(|| anyhow::anyhow!("Missing --ticket-id or NEXUS_TICKET_ID"))?;
                    let base = api_url
                        .or_else(|| std::env::var("NEXUS_API_URL").ok())
                        .unwrap_or_else(|| "http://127.0.0.1:3082".to_owned());
                    let h = HarnessKind::parse(&harness)
                        .ok_or_else(|| anyhow::anyhow!("Invalid harness `{harness}`"))?;
                    let t = ThinkEffort::parse(&think)
                        .ok_or_else(|| anyhow::anyhow!("Invalid think effort `{think}`"))?;
                    let payload = WorkerSwitchTargetPayload {
                        ticket_id: tid,
                        harness: h,
                        provider,
                        model,
                        think_effort: t,
                        reason,
                        git_status_diff: None,
                    };
                    let resp: serde_json::Value = client
                        .post(format!(
                            "{}/api/worker/switch-target",
                            base.trim_end_matches('/')
                        ))
                        .json(&payload)
                        .send()
                        .await?
                        .json()
                        .await?;
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
            }
        }
    }

    Ok(())
}
