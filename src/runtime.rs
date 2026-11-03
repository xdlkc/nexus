//! End-to-end `NexusRuntime` control loop coordinating Sources, Gate, Dual-Engine Router,
//! Polymorphic Worker Pool, Outbox delivery, and SQLite persistence.

use crate::adapters::feishu::FeishuAdapter;
use crate::adapters::github::GithubAdapter;
use crate::adapters::traits::{ChannelAdapter, SourceAdapter};
use crate::config::NexusConfig;
use crate::core::engine::Engine;
use crate::core::gate::evaluate_gate_strength;
use crate::core::model::{EffectKind, HarnessKind};
use crate::infra::effects::{mark_outbox_sent, pop_next_effect, record_effect_failure};
use crate::infra::shims::{ShimConfig, provision_worker_shims};
use crate::infra::store::SqliteStore;
use crate::router::jev::JevClassifier;
use crate::router::llm::LlmClient;
use crate::router::orchestrator::evaluate_thread_batch;
use crate::workers::dispatcher::{
    WorkerSlots, build_worker_prompt_with_memory, infer_launch_priority,
};
use crate::workers::driver::{
    WorkerLaunchContext, run_cli_harness_worker, run_pi_native_worker,
};
use crate::workers::worktree::prepare_worktree;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Summary of work performed during a single `NexusRuntime::tick` cycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeTickSummary {
    pub polled_triggers: usize,
    pub closed_threads: usize,
    pub evaluated_batches: usize,
    pub launched_assignments: usize,
    pub delivered_outbox_messages: usize,
}

/// Complete Nexus runtime orchestrator parameterized over `JevClassifier` and `LlmClient`
/// so both production HTTP clients and deterministic end-to-end tests share the exact same pipeline.
pub struct NexusRuntime<J, L> {
    pub engine: Arc<Mutex<Engine>>,
    pub store: Arc<SqliteStore>,
    pub config: Arc<Mutex<NexusConfig>>,
    pub slots: WorkerSlots,
    pub jev: J,
    pub llm: L,
}

impl<J, L> NexusRuntime<J, L>
where
    J: JevClassifier,
    L: LlmClient,
{
    pub fn new(
        engine: Arc<Mutex<Engine>>,
        store: Arc<SqliteStore>,
        config: Arc<Mutex<NexusConfig>>,
        jev: J,
        llm: L,
    ) -> anyhow::Result<Self> {
        let max_slots = {
            let cfg = config.lock().unwrap();
            let _ = provision_worker_shims(&ShimConfig {
                state_dir: cfg.state_dir.clone(),
                github_token: std::env::var("GITHUB_TOKEN")
                    .or_else(|_| std::env::var("GH_TOKEN"))
                    .ok(),
                github_config_dir: None,
                lark_profile: cfg.feishu.profile.clone(),
                gws_account: None,
                nexus_bin_path: std::env::current_exe().ok(),
            });
            cfg.max_concurrent_workers
        };

        Ok(Self {
            engine,
            store,
            config,
            slots: WorkerSlots::new(max_slots),
            jev,
            llm,
        })
    }

    /// Polls configured Feishu (`lark-cli`) and GitHub (`gh`) sources and drains all resulting Effects.
    pub async fn tick(&mut self, now: &str) -> anyhow::Result<RuntimeTickSummary> {
        let mut summary = RuntimeTickSummary::default();
        self.poll_sources(&mut summary, now).await?;
        self.drain_effects(&mut summary, now).await?;
        Ok(summary)
    }

    /// Polls Feishu (`lark-cli`) and GitHub (`gh`) adapters, feeding events through Tier-0 Gate into `Engine`.
    pub async fn poll_sources(
        &mut self,
        summary: &mut RuntimeTickSummary,
        now: &str,
    ) -> anyhow::Result<()> {
        let ( mut feishu_cfg, mut github_cfg, policy, accept_mode) = {
            let cfg = self.config.lock().unwrap();
            let eng = self.engine.lock().unwrap();
            let mut g_cfg = cfg.github.clone();
            // Collect all open PR threads followed by active tickets so GithubAdapter polls their merge state!
            for t in eng.state.tickets.values().filter(|t| t.is_open()) {
                for f in &t.follows {
                    if f.closed.is_none()
                        && f.thread_key.starts_with("github:pr:")
                        && !g_cfg.followed_pr_threads.contains(&f.thread_key)
                    {
                        g_cfg.followed_pr_threads.push(f.thread_key.clone());
                    }
                }
            }
            (
                cfg.feishu.clone(),
                g_cfg,
                cfg.admission_policy(),
                eng.state.accept_mode.unwrap_or(cfg.accept_mode),
            )
        };

        // 1. Poll Feishu (`lark-cli`) if chats or bot identity are configured
        if !feishu_cfg.chat_ids.is_empty() || feishu_cfg.bot_open_id.is_some() {
            let cursor = self
                .store
                .get_cursor(&feishu_cfg.source_id, "latest_create_time_ms")?;
            let adapter = FeishuAdapter::new(std::mem::take(&mut feishu_cfg));
            if let Ok(poll_out) = adapter.poll(cursor.as_deref()).await {
                let mut eng = self.engine.lock().unwrap();
                for trig in poll_out.triggers {
                    let strength = evaluate_gate_strength(
                        &trig.actor_id,
                        trig.actor_is_bot,
                        trig.addressed_agent,
                        &policy,
                        accept_mode,
                    );
                    eng.receive_trigger(trig, strength);
                    summary.polled_triggers += 1;
                }
                for (ck, cv) in poll_out.updated_cursors {
                    let _ = self.store.set_cursor(adapter.source_id(), &ck, &cv, now);
                }
                self.store.save_engine_state(&eng.state, now)?;
            }
        }

        // 2. Poll GitHub (`gh`) if repositories or followed PRs are configured
        if !github_cfg.repos.is_empty() || !github_cfg.followed_pr_threads.is_empty() {
            let cursor = self.store.get_cursor(&github_cfg.source_id, "since_iso")?;
            let adapter = GithubAdapter::new(std::mem::take(&mut github_cfg));
            if let Ok(poll_out) = adapter.poll(cursor.as_deref()).await {
                let mut eng = self.engine.lock().unwrap();
                for trig in poll_out.triggers {
                    let strength = evaluate_gate_strength(
                        &trig.actor_id,
                        trig.actor_is_bot,
                        trig.addressed_agent,
                        &policy,
                        accept_mode,
                    );
                    eng.receive_trigger(trig, strength);
                    summary.polled_triggers += 1;
                }
                for (pr_thread, outcome) in poll_out.closed_threads {
                    eng.thread_closed(&pr_thread, &outcome, now);
                    summary.closed_threads += 1;
                }
                for (ck, cv) in poll_out.updated_cursors {
                    let _ = self.store.set_cursor(adapter.source_id(), &ck, &cv, now);
                }
                self.store.save_engine_state(&eng.state, now)?;
            }
        }

        Ok(())
    }

    /// Drains all queued `Effect`s (`RunEval`, `LaunchAssignment`, `DeliverInputs`, `SendOutbox`).
    pub async fn drain_effects(
        &mut self,
        summary: &mut RuntimeTickSummary,
        now: &str,
    ) -> anyhow::Result<()> {
        // Guard against unbounded cascades within a single tick
        for _ in 0..32 {
            let next_eff = {
                let mut eng = self.engine.lock().unwrap();
                pop_next_effect(&mut eng)
            };
            let Some(eff) = next_eff else {
                break;
            };

            match &eff.kind {
                EffectKind::RunEval { thread_key, .. } => {
                    let (rules, llm_cfg) = {
                        let cfg = self.config.lock().unwrap();
                        (cfg.proactive_rules.clone(), cfg.router_llm.clone())
                    };
                    let mut local_eng = {
                        let eng = self.engine.lock().unwrap();
                        Engine::from_state(eng.state.clone())
                    };
                    let res = evaluate_thread_batch(
                        &mut local_eng,
                        &self.jev,
                        &self.llm,
                        &llm_cfg,
                        thread_key,
                        &rules,
                        now,
                    )
                    .await;
                    let mut eng = self.engine.lock().unwrap();
                    eng.state = local_eng.state;
                    match res {
                        Ok(_) => {
                            summary.evaluated_batches += 1;
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                        Err(e) => {
                            record_effect_failure(&mut eng, eff, e.to_string());
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                    }
                }
                EffectKind::LaunchAssignment {
                    ticket_id,
                    assignment_id,
                }
                | EffectKind::DeliverInputs {
                    ticket_id,
                    assignment_id,
                    ..
                } => {
                    let launch_prep = {
                        let eng = self.engine.lock().unwrap();
                        let cfg = self.config.lock().unwrap();
                        if let Some(ticket) = eng.state.tickets.get(ticket_id) {
                            if let Some(assign) =
                                ticket.assignments.iter().find(|a| &a.id == assignment_id)
                            {
                                let priority = infer_launch_priority(ticket);
                                let prompt = build_worker_prompt_with_memory(
                                    ticket,
                                    assign,
                                    Some(&eng.state),
                                    Some(&cfg.state_dir),
                                );
                                let repo_checkout = ticket
                                    .repo
                                    .as_ref()
                                    .and_then(|r| cfg.repositories.get(r))
                                    .or_else(|| cfg.repositories.values().next())
                                    .cloned();
                                Some((
                                    priority,
                                    ticket.short_name.clone(),
                                    assign.clone(),
                                    prompt,
                                    repo_checkout,
                                    cfg.state_dir.clone(),
                                    format!("http://{}", cfg.http_addr),
                                ))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };

                    let Some((
                        priority,
                        short_name,
                        assign,
                        prompt,
                        repo_checkout,
                        state_dir,
                        api_url,
                    )) = launch_prep
                    else {
                        continue;
                    };

                    if !self.slots.try_acquire(assignment_id, priority) {
                        // Re-queue effect for a future tick when a slot frees up
                        let mut eng = self.engine.lock().unwrap();
                        eng.state.effects.push(eff);
                        break;
                    }

                    let wt_path = Path::new(&assign.worktree_dir);
                    let _ = prepare_worktree(repo_checkout.as_deref(), wt_path, &short_name).await;

                    let ctx = WorkerLaunchContext {
                        ticket_id: ticket_id.clone(),
                        assignment_id: assignment_id.clone(),
                        target: assign.target.clone(),
                        worktree_dir: wt_path.to_path_buf(),
                        state_dir,
                        api_url,
                        prompt,
                        max_turns: 8,
                    };

                    let exec_res = if assign.target.harness == HarnessKind::PiNative {
                        match run_pi_native_worker(
                            &self.llm,
                            &ctx,
                            std::time::Duration::from_secs(5),
                        )
                        .await
                        {
                            Ok(res) => Ok(res),
                            Err(_) => {
                                // Automatic fallback to Codex CLI (`gpt-6-astra`) when direct LLM API quota is unavailable
                                let mut fallback_ctx = ctx.clone();
                                fallback_ctx.target.harness = HarnessKind::Codex;
                                fallback_ctx.target.provider = "openai".to_owned();
                                fallback_ctx.target.model = "gpt-6-astra".to_owned();
                                run_cli_harness_worker(&fallback_ctx).await
                            }
                        }
                    } else {
                        run_cli_harness_worker(&ctx).await
                    };

                    self.slots.release(assignment_id);

                    let mut eng = self.engine.lock().unwrap();
                    match exec_res {
                        Ok(worker_out) => {
                            let already_reported = eng
                                .state
                                .tickets
                                .get(ticket_id)
                                .and_then(|t| t.assignments.iter().find(|a| &a.id == assignment_id))
                                .is_some_and(|a| {
                                    a.status == crate::core::model::AssignmentStatus::Ended
                                });
                            if !already_reported {
                                let _ = eng.record_worker_report(
                                    ticket_id,
                                    worker_out.status,
                                    worker_out.summary,
                                    worker_out.question,
                                    worker_out.pull_request_thread_key,
                                    worker_out.pull_request_url,
                                    now,
                                );
                            }
                            summary.launched_assignments += 1;
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                        Err(e) => {
                            record_effect_failure(&mut eng, eff, e.to_string());
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                    }
                }
                EffectKind::SendOutbox { outbox_id } => {
                    let msg_opt = {
                        let eng = self.engine.lock().unwrap();
                        eng.state.outbox.get(outbox_id).cloned()
                    };
                    let Some(msg) = msg_opt else {
                        continue;
                    };

                    let (feishu_cfg, github_cfg) = {
                        let cfg = self.config.lock().unwrap();
                        (cfg.feishu.clone(), cfg.github.clone())
                    };
                    let feishu_adapter = FeishuAdapter::new(feishu_cfg);
                    let github_adapter = GithubAdapter::new(github_cfg);

                    let send_res = if feishu_adapter.can_deliver(&msg.channel) {
                        feishu_adapter.send_message(&msg).await
                    } else if github_adapter.can_deliver(&msg.channel) {
                        github_adapter.send_message(&msg).await
                    } else {
                        Ok(())
                    };

                    let mut eng = self.engine.lock().unwrap();
                    match send_res {
                        Ok(()) => {
                            mark_outbox_sent(&mut eng, outbox_id);
                            summary.delivered_outbox_messages += 1;
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                        Err(e) => {
                            record_effect_failure(&mut eng, eff, e.to_string());
                            self.store.save_engine_state(&eng.state, now)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
