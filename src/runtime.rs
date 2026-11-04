//! End-to-end `NexusRuntime` control loop coordinating Sources, Gate, Dual-Engine Router,
//! Polymorphic Worker Pool, Outbox delivery, and SQLite persistence.

use crate::adapters::feishu::FeishuAdapter;
use crate::adapters::github::GithubAdapter;
use crate::adapters::im_channels::DingtalkAdapter;
use crate::adapters::traits::{ChannelAdapter, SourceAdapter};
use crate::config::NexusConfig;
use crate::core::engine::Engine;
use crate::core::gate::evaluate_gate_strength;
use crate::core::model::{EffectKind, HarnessKind};
use crate::infra::effects::{mark_outbox_sent, pop_next_effect, record_effect_failure};
use crate::infra::shims::provision_agent_workspace_from_config;
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
            let _ = provision_agent_workspace_from_config(&cfg);
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
        let (mut feishu_cfg, mut dingtalk_cfg, mut github_cfg, policy, accept_mode) = {
            let cfg = self.config.lock().unwrap();
            let eng = self.engine.lock().unwrap();
            let mut g_cfg = cfg.github.clone();
            if g_cfg.gh_bin == Path::new("gh") {
                let abs_state =
                    std::fs::canonicalize(&cfg.state_dir).unwrap_or_else(|_| cfg.state_dir.clone());
                let gh_agent = abs_state.join("worker-bin").join("gh-agent");
                if gh_agent.exists() {
                    g_cfg.gh_bin = gh_agent;
                }
            }
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
                cfg.dingtalk.clone(),
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
                    let is_subscribed_thread = eng
                        .state
                        .threads
                        .get(&trig.thread_key)
                        .is_some_and(|t| !t.subscribed_tickets.is_empty());
                    let mut strength = evaluate_gate_strength(
                        &trig.actor_id,
                        trig.actor_is_bot,
                        trig.addressed_agent || is_subscribed_thread,
                        &policy,
                        accept_mode,
                    );
                    if is_subscribed_thread
                        && !trig.actor_is_bot
                        && strength == crate::core::model::AdmissionStrength::NotAdmitted
                    {
                        strength = crate::core::model::AdmissionStrength::Explicit;
                    }
                    eng.receive_trigger(trig, strength);
                    summary.polled_triggers += 1;
                }
                for (ck, cv) in poll_out.updated_cursors {
                    let _ = self.store.set_cursor(adapter.source_id(), &ck, &cv, now);
                }
                self.store.save_engine_state(&eng.state, now)?;
            }
        }

        // 1b. Poll DingTalk (`dws`) if chats are configured
        if !dingtalk_cfg.chat_ids.is_empty() {
            let cursor = self
                .store
                .get_cursor(&dingtalk_cfg.source_id, "latest_create_time")?;
            let adapter = DingtalkAdapter::new(std::mem::take(&mut dingtalk_cfg));
            if let Ok(poll_out) = adapter.poll(cursor.as_deref()).await {
                let mut eng = self.engine.lock().unwrap();
                for trig in poll_out.triggers {
                    let is_subscribed_thread = eng
                        .state
                        .threads
                        .get(&trig.thread_key)
                        .is_some_and(|t| !t.subscribed_tickets.is_empty());
                    let mut strength = evaluate_gate_strength(
                        &trig.actor_id,
                        trig.actor_is_bot,
                        trig.addressed_agent || is_subscribed_thread,
                        &policy,
                        accept_mode,
                    );
                    if is_subscribed_thread
                        && !trig.actor_is_bot
                        && strength == crate::core::model::AdmissionStrength::NotAdmitted
                    {
                        strength = crate::core::model::AdmissionStrength::Explicit;
                    }
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
                    let is_subscribed_thread = eng
                        .state
                        .threads
                        .get(&trig.thread_key)
                        .is_some_and(|t| !t.subscribed_tickets.is_empty());
                    let mut strength = evaluate_gate_strength(
                        &trig.actor_id,
                        trig.actor_is_bot,
                        trig.addressed_agent || is_subscribed_thread,
                        &policy,
                        accept_mode,
                    );
                    if is_subscribed_thread
                        && !trig.actor_is_bot
                        && strength == crate::core::model::AdmissionStrength::NotAdmitted
                    {
                        strength = crate::core::model::AdmissionStrength::Explicit;
                    }
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
                        let mut eng = self.engine.lock().unwrap();
                        let cfg = self.config.lock().unwrap();
                        if let Some(t_mut) = eng.state.tickets.get_mut(ticket_id)
                            && t_mut.repo.is_none()
                        {
                            t_mut.repo = infer_ticket_repo(t_mut, &cfg);
                        }
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
                                    .or_else(|| {
                                        cfg.default_repo
                                            .as_ref()
                                            .and_then(|r| cfg.repositories.get(r))
                                    })
                                    .or_else(|| cfg.repositories.get("xdlkc/nexus"))
                                    .or_else(|| cfg.repositories.values().next())
                                    .cloned();
                                let reaction_msg_id = ticket
                                    .inputs
                                    .iter()
                                    .rev()
                                    .find_map(|inp| match &inp.kind {
                                        crate::core::model::TicketInputKind::TriggerFanOut {
                                            trigger_id,
                                            ..
                                        } => eng
                                            .state
                                            .triggers
                                            .get(trigger_id)
                                            .filter(|tr| {
                                                tr.source_kind
                                                    == crate::core::model::SourceKindId::Feishu
                                            })
                                            .map(|tr| tr.external_id.clone()),
                                        _ => None,
                                    })
                                    .or_else(|| {
                                        ticket.channels.iter().find_map(|ch| match ch {
                                            crate::core::model::ChannelAddress::FeishuTopic {
                                                reply_to_message_id,
                                                ..
                                            } => Some(reply_to_message_id.clone()),
                                            _ => None,
                                        })
                                    });
                                let dingtalk_reaction_target =
                                    ticket.channels.iter().find_map(|ch| match ch {
                                        crate::core::model::ChannelAddress::GenericIm {
                                            source_kind: crate::core::model::SourceKindId::Dingtalk,
                                            conversation_id,
                                            thread_id: Some(thread_id),
                                            ..
                                        } => {
                                            let msg_id =
                                                thread_id.split('|').next().unwrap_or(thread_id);
                                            Some((conversation_id.clone(), msg_id.to_owned()))
                                        }
                                        _ => None,
                                    });
                                let reply_channel_for_progress =
                                    ticket.channels.first().cloned();
                                Some((
                                    priority,
                                    ticket.short_name.clone(),
                                    assign.clone(),
                                    prompt,
                                    repo_checkout,
                                    cfg.state_dir.clone(),
                                    format!("http://{}", cfg.http_addr),
                                    cfg.feishu.clone(),
                                    cfg.dingtalk.clone(),
                                    reaction_msg_id,
                                    dingtalk_reaction_target,
                                    reply_channel_for_progress,
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
                        feishu_cfg_for_reaction,
                        dingtalk_cfg_for_reaction,
                        reaction_msg_id,
                        dingtalk_reaction_target,
                        reply_channel_for_progress,
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

                    // Immediately attach an emoji reaction (e.g. `OnIt`) to the Feishu message when processing begins!
                    if let (Some(msg_id), Some(emoji)) = (
                        reaction_msg_id.as_deref(),
                        feishu_cfg_for_reaction
                            .ack_reaction
                            .clone()
                            .filter(|s| !s.trim().is_empty()),
                    ) {
                        let adapter = FeishuAdapter::new(feishu_cfg_for_reaction.clone());
                        let _ = adapter.add_reaction(msg_id, &emoji).await;
                    }

                    // Immediately attach an emoji reaction (e.g. `OK`) to the DingTalk message when processing begins!
                    if let (Some((conv_id, msg_id)), Some(emoji)) = (
                        dingtalk_reaction_target.as_ref(),
                        dingtalk_cfg_for_reaction
                            .ack_reaction
                            .clone()
                            .filter(|s| !s.trim().is_empty()),
                    ) {
                        let adapter = DingtalkAdapter::new(dingtalk_cfg_for_reaction.clone());
                        let _ = adapter.add_reaction(conv_id, msg_id, &emoji).await;
                    }

                    let wt_path = Path::new(&assign.worktree_dir);
                    let _ = prepare_worktree(repo_checkout.as_deref(), wt_path, &short_name).await;

                    let (progress_tx, mut progress_rx) =
                        tokio::sync::mpsc::unbounded_channel::<String>();
                    let progress_engine = Arc::clone(&self.engine);
                    let progress_store = Arc::clone(&self.store);
                    let progress_ticket_id = ticket_id.clone();
                    let progress_feishu_cfg = feishu_cfg_for_reaction;
                    let progress_dingtalk_cfg = dingtalk_cfg_for_reaction;
                    let progress_channel = reply_channel_for_progress;

                    let progress_forwarder = tokio::spawn(async move {
                        let feishu_adapter = FeishuAdapter::new(progress_feishu_cfg);
                        let dingtalk_adapter = DingtalkAdapter::new(progress_dingtalk_cfg);
                        while let Some(note) = progress_rx.recv().await {
                            let ts = chrono::Utc::now().to_rfc3339();
                            {
                                let mut eng = progress_engine.lock().unwrap();
                                let _ = eng.record_worker_report(
                                    &progress_ticket_id,
                                    crate::core::model::ReportStatus::Progress,
                                    note.clone(),
                                    None,
                                    None,
                                    None,
                                    &ts,
                                );
                                let _ = progress_store.save_engine_state(&eng.state, &ts);
                            }
                            if let Some(ch) = &progress_channel {
                                let msg = crate::core::model::OutboxMessage {
                                    id: format!("prog-{}", chrono::Utc::now().timestamp_millis()),
                                    ticket_id: Some(progress_ticket_id.clone()),
                                    channel: ch.clone(),
                                    purpose: crate::core::model::OutboxPurpose::Attention,
                                    body: note,
                                    status: crate::core::model::OutboxStatus::Pending,
                                    attempts: 0,
                                    last_error: None,
                                    created_at: ts,
                                };
                                if feishu_adapter.can_deliver(ch) {
                                    let _ = feishu_adapter.send_message(&msg).await;
                                } else if dingtalk_adapter.can_deliver(ch) {
                                    let _ = dingtalk_adapter.send_message(&msg).await;
                                }
                            }
                        }
                    });

                    let ctx = WorkerLaunchContext {
                        ticket_id: ticket_id.clone(),
                        assignment_id: assignment_id.clone(),
                        target: assign.target.clone(),
                        worktree_dir: wt_path.to_path_buf(),
                        state_dir,
                        api_url,
                        prompt,
                        max_turns: 28,
                        progress_tx: Some(progress_tx),
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

                    drop(ctx);
                    let _ = progress_forwarder.await;

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
                            if let Ok(cfg) = self.config.lock() {
                                consolidate_memory_file(&cfg, &eng.state);
                            }
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

                    let (feishu_cfg, dingtalk_cfg, github_cfg) = {
                        let cfg = self.config.lock().unwrap();
                        let mut g_cfg = cfg.github.clone();
                        if g_cfg.gh_bin == Path::new("gh") {
                            let abs_state = std::fs::canonicalize(&cfg.state_dir)
                                .unwrap_or_else(|_| cfg.state_dir.clone());
                            let gh_agent = abs_state.join("worker-bin").join("gh-agent");
                            if gh_agent.exists() {
                                g_cfg.gh_bin = gh_agent;
                            }
                        }
                        (cfg.feishu.clone(), cfg.dingtalk.clone(), g_cfg)
                    };
                    let feishu_adapter = FeishuAdapter::new(feishu_cfg);
                    let dingtalk_adapter = DingtalkAdapter::new(dingtalk_cfg);
                    let github_adapter = GithubAdapter::new(github_cfg);

                    let send_res = if feishu_adapter.can_deliver(&msg.channel) {
                        feishu_adapter.send_message(&msg).await
                    } else if dingtalk_adapter.can_deliver(&msg.channel) {
                        dingtalk_adapter.send_message(&msg).await
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

/// Infers the target repository (`owner/repo`) for a Ticket from its GitHub channels,
/// subscribed threads, title/inputs text, or `cfg.default_repo`.
pub fn infer_ticket_repo(
    ticket: &crate::core::model::Ticket,
    cfg: &NexusConfig,
) -> Option<String> {
    let channel_repo = ticket.channels.iter().find_map(|ch| match ch {
        crate::core::model::ChannelAddress::GithubIssueComment { repo, .. }
        | crate::core::model::ChannelAddress::GithubReviewReply { repo, .. } => Some(repo.clone()),
        _ => None,
    });
    let sub_repo = ticket.subscriptions.iter().find_map(|sub| {
        let rest = sub
            .strip_prefix("github:issue:")
            .or_else(|| sub.strip_prefix("github:pr:"))?;
        let (repo, _) = rest.split_once('#')?;
        Some(repo.to_owned())
    });
    let combined_lower = format!(
        "{} {}",
        ticket.title,
        ticket
            .inputs
            .iter()
            .map(|inp| format!("{:?}", inp.kind))
            .collect::<Vec<_>>()
            .join(" ")
    )
    .to_lowercase();

    let combined_compact = combined_lower.replace(['-', '_'], "");
    let default_repo_lower = cfg
        .default_repo
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();

    // Order candidate repos so non-default repos are checked before `default_repo`
    // (because prompts often mention "Nexus" as the assistant's own name).
    let mut ordered_repos: Vec<&String> = cfg
        .github
        .repos
        .iter()
        .filter(|r| r.to_lowercase() != default_repo_lower)
        .collect();
    ordered_repos.extend(
        cfg.github
            .repos
            .iter()
            .filter(|r| r.to_lowercase() == default_repo_lower),
    );

    channel_repo
        .or(sub_repo)
        .or_else(|| {
            ordered_repos
                .iter()
                .find(|r| {
                    let full_lower = r.to_lowercase();
                    let short = full_lower.rsplit('/').next().unwrap_or(&full_lower);
                    let short_compact = short.replace(['-', '_'], "");
                    !short.is_empty()
                        && (combined_lower.contains(&full_lower)
                            || combined_lower.contains(short)
                            || (!short_compact.is_empty()
                                && combined_compact.contains(&short_compact)))
                })
                .map(|r| (*r).clone())
        })
        .or_else(|| cfg.default_repo.clone())
        .or_else(|| cfg.github.repos.first().cloned())
}

/// Automatically consolidates completed Ticket milestones, PR links, and active identity settings
/// into `<state_dir>/MEMORY.md` so persistent memory stays up to date after every task.
pub fn consolidate_memory_file(cfg: &NexusConfig, state: &crate::core::engine::EngineState) {
    let mem_path = cfg.state_dir.join("MEMORY.md");
    let existing = std::fs::read_to_string(&mem_path).unwrap_or_default();
    let mut updated = existing.clone();

    if updated.trim().is_empty() {
        updated = format!(
            "# Nexus Persistent Memory (MEMORY.md)\n\n\
             - **User**: 张俊博 (`ou_d59820429ccf6256556f7b9ac0c26f0f`)\n\
             - **Host GitHub Account**: `xdlkc`\n\
             - **Nexus Isolated GitHub Account**: `{}` (`{}`)\n\
             - **Default Repository**: `{}`\n",
            cfg.github.bot_login,
            cfg.agent_identity.git_user_email,
            cfg.default_repo.as_deref().unwrap_or("xdlkc/nexus")
        );
    }

    let section_header = "## 近期自动沉淀结论与工单交付记录 (Auto-Consolidated Milestones)";
    if !updated.contains(section_header) {
        updated.push_str(&format!("\n{section_header}\n"));
    }

    // Sort tickets chronologically so milestones append in order
    let mut tickets: Vec<_> = state.tickets.values().collect();
    tickets.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    for t in tickets {
        let marker = format!("**[{}]**", t.id);
        if updated.contains(&marker) {
            continue;
        }
        let Some(last_asg) = t.assignments.last() else {
            continue;
        };
        let Some(rep) = &last_asg.latest_report else {
            continue;
        };
        if rep.status != crate::core::model::ReportStatus::Ready {
            continue;
        }
        let repo_tag = t
            .repo
            .as_deref()
            .or(cfg.default_repo.as_deref())
            .unwrap_or("xdlkc/nexus");
        let summary_one_line = rep
            .summary
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ； ");
        if summary_one_line.is_empty() {
            continue;
        }
        updated.push_str(&format!(
            "- {marker} (`{repo_tag}` · {}): {}\n",
            t.title.replace('\n', " "),
            summary_one_line
        ));
    }

    if updated != existing {
        let _ = std::fs::write(&mem_path, updated);
    }
}
