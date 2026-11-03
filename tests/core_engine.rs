use nexus::core::engine::{Engine, IntakeDisposition};
use nexus::core::model::{
    AdmissionStrength, ChannelAddress, ExecutionTarget, HarnessKind, OutboxPurpose, ReportStatus,
    SourceKindId, ThinkEffort, TicketStage, Trigger,
};
use nexus::core::recall::RecallCaller;
use std::collections::BTreeMap;

fn make_feishu_trigger(id: &str, ext: &str, thread: &str, text: &str, addressed: bool) -> Trigger {
    Trigger {
        id: id.to_owned(),
        source_id: "feishu-main".to_owned(),
        source_kind: SourceKindId::Feishu,
        external_id: ext.to_owned(),
        thread_key: thread.to_owned(),
        actor_id: "ou_alice".to_owned(),
        actor_is_bot: false,
        addressed_agent: addressed,
        text: text.to_owned(),
        reply_to_trigger_id: None,
        inline_target: None,
        reply_channel: Some(ChannelAddress::FeishuTopic {
            source_id: "feishu-main".to_owned(),
            chat_id: "oc_chat_1".to_owned(),
            root_message_id: thread.to_owned(),
            reply_to_message_id: ext.to_owned(),
        }),
        at: "2026-11-03T01:00:00Z".to_owned(),
        meta: BTreeMap::new(),
    }
}

#[test]
fn m_to_n_subscription_fanout_and_human_wakeup() {
    let mut engine = Engine::new("/tmp/nexus-worktrees");

    // 1. First trigger enters Router candidate batch
    let t1 = make_feishu_trigger(
        "trig-1",
        "om_1001",
        "feishu:topic:root-1",
        "@nexus investigate memory leak in parser",
        true,
    );
    let outcome = engine.receive_trigger(t1, AdmissionStrength::Explicit);
    assert_eq!(
        outcome,
        IntakeDisposition::QueuedForEval {
            thread_key: "feishu:topic:root-1".to_owned(),
            batch_size: 1,
        }
    );

    // 2. Router accepts ticket on thread feishu:topic:root-1
    let ticket_id = engine
        .accept_ticket(
            "trig-1",
            "Investigate parser memory leak",
            Some("nexus".to_owned()),
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            Some("Looking into the parser memory leak now."),
            "2026-11-03T01:00:05Z",
        )
        .expect("accept ticket");

    // 3. Worker reports Waiting (blocked on repro question)
    engine
        .record_worker_report(
            &ticket_id,
            ReportStatus::Waiting,
            "Asked Alice which input file triggers the leak.",
            Some("Could you share the sample input file size and format?".to_owned()),
            None,
            None,
            "2026-11-03T01:01:00Z",
        )
        .expect("worker report");

    assert_eq!(
        engine.state.tickets[&ticket_id].stage(),
        TicketStage::PersonAnswer
    );
    assert_eq!(engine.state.outbox.len(), 2); // Ack + Attention question

    // 4. Human replies in the same Feishu topic WITHOUT @mentioning the bot.
    // Because the thread is subscribed by ticket_id, it bypasses Router eval and wakes the ticket!
    let t2 = make_feishu_trigger(
        "trig-2",
        "om_1002",
        "feishu:topic:root-1",
        "It happens on 50MB CSV files with UTF-8 BOM.",
        false,
    );
    let outcome2 = engine.receive_trigger(t2, AdmissionStrength::Inferred);
    assert_eq!(
        outcome2,
        IntakeDisposition::FannedOutToTickets {
            ticket_ids: vec![ticket_id.clone()],
        }
    );
    assert_eq!(
        engine.state.tickets[&ticket_id].stage(),
        TicketStage::WorkingQueued
    );
    assert_eq!(engine.state.tickets[&ticket_id].assignments.len(), 2);
}

#[test]
fn mid_ticket_cross_harness_switch_creates_handoff_input() {
    let mut engine = Engine::new("/tmp/nexus-worktrees");
    let t1 = make_feishu_trigger(
        "trig-1",
        "om_2001",
        "feishu:topic:root-2",
        "@nexus fix lock contention in dispatcher",
        true,
    );
    engine.receive_trigger(t1, AdmissionStrength::Explicit);

    let ticket_id = engine
        .accept_ticket(
            "trig-1",
            "Fix lock contention",
            Some("nexus".to_owned()),
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::ClaudeCode,
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            None,
            "2026-11-03T01:10:00Z",
        )
        .unwrap();

    // Switch to Codex xhigh with a git diff snapshot
    let new_assignment_id = engine
        .switch_execution_target(
            &ticket_id,
            ExecutionTarget {
                harness: HarnessKind::Codex,
                provider: "openai-codex".to_owned(),
                model: "gpt-5.6-sol".to_owned(),
                think_effort: ThinkEffort::Xhigh,
            },
            "Escalating deep lock ordering analysis to Codex xhigh",
            "M src/workers/dispatcher.rs (+14 -5)",
            "2026-11-03T01:15:00Z",
        )
        .unwrap();

    let ticket = &engine.state.tickets[&ticket_id];
    assert_eq!(ticket.assignments.len(), 2);
    assert_eq!(ticket.assignments[1].id, new_assignment_id);
    assert_eq!(ticket.current_target().harness, HarnessKind::Codex);
    assert_eq!(ticket.current_target().think_effort, ThinkEffort::Xhigh);
    assert_eq!(
        ticket.assignments[0].worktree_dir,
        ticket.assignments[1].worktree_dir
    );
}

#[test]
fn handback_and_pr_merge_auto_completion_and_recall_permissions() {
    let mut engine = Engine::new("/tmp/nexus-worktrees");

    let t1 = make_feishu_trigger(
        "trig-1",
        "om_3001",
        "feishu:topic:root-3",
        "@nexus add retry header to webhook client",
        true,
    );
    engine.receive_trigger(t1, AdmissionStrength::Explicit);

    let ticket1_id = engine
        .accept_ticket(
            "trig-1",
            "Webhook retry header",
            Some("nexus".to_owned()),
            AdmissionStrength::Explicit,
            vec![],
            ExecutionTarget {
                harness: HarnessKind::PiNative,
                provider: "anthropic".to_owned(),
                model: "claude-sonnet-4-6".to_owned(),
                think_effort: ThinkEffort::Medium,
            },
            None,
            "2026-11-03T01:20:00Z",
        )
        .unwrap();

    // Worker reports Ready with a PR -> Ticket enters PersonReview waiting for PR merge
    engine
        .record_worker_report(
            &ticket1_id,
            ReportStatus::Ready,
            "Opened PR #42 adding X-Retry-Count header.",
            None,
            Some("github:pr:owner/nexus#42".to_owned()),
            Some("https://github.com/owner/nexus/pull/42".to_owned()),
            "2026-11-03T01:25:00Z",
        )
        .unwrap();

    assert_eq!(
        engine.state.tickets[&ticket1_id].stage(),
        TicketStage::PersonReview
    );
    assert!(engine.state.tickets[&ticket1_id].is_open());

    // Unrelated message lands in feishu:topic:root-3 -> delivered to ticket1, then Worker hands it back
    let t_unrelated = make_feishu_trigger(
        "trig-unrelated",
        "om_3002",
        "feishu:topic:root-3",
        "By the way, we also need a new release tag for v0.2.0",
        false,
    );
    engine.receive_trigger(t_unrelated, AdmissionStrength::Inferred);
    let handed_back = engine
        .handback_trigger(
            &ticket1_id,
            "trig-unrelated",
            "Release tagging is unrelated to webhook retry header",
            "2026-11-03T01:26:00Z",
        )
        .unwrap();
    assert!(handed_back);
    assert!(
        engine.state.threads["feishu:topic:root-3"]
            .queued_trigger_ids
            .contains(&"trig-unrelated".to_owned())
    );

    // Worker recall permission check: ticket1 can read its own ticket and subscribed thread,
    // but cannot read another ticket (`ticket/tkt-other`) or router decisions (`decision/dec-2`).
    assert!(
        engine
            .read_recall_resource(
                &RecallCaller::Worker {
                    ticket_id: ticket1_id.clone()
                },
                &format!("ticket/{ticket1_id}"),
            )
            .is_ok()
    );
    assert!(
        engine
            .read_recall_resource(
                &RecallCaller::Worker {
                    ticket_id: ticket1_id.clone()
                },
                "ticket/tkt-other",
            )
            .is_err()
    );
    assert!(
        engine
            .read_recall_resource(
                &RecallCaller::Worker {
                    ticket_id: ticket1_id.clone()
                },
                "decision/dec-2",
            )
            .is_err()
    );

    // Outbox word limit check: >100 words rejected on Feishu
    let over_100_words = (0..105)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let channel = ChannelAddress::FeishuTopic {
        source_id: "feishu-main".to_owned(),
        chat_id: "oc_chat_1".to_owned(),
        root_message_id: "feishu:topic:root-3".to_owned(),
        reply_to_message_id: "om_3001".to_owned(),
    };
    assert!(
        engine
            .queue_outbox_message(
                Some(ticket1_id.clone()),
                channel,
                OutboxPurpose::Result,
                &over_100_words,
                "2026-11-03T01:27:00Z"
            )
            .is_err()
    );

    // PR #42 merges -> thread_closed automatically completes ticket1
    engine.thread_closed(
        "github:pr:owner/nexus#42",
        "merged",
        "2026-11-03T01:30:00Z",
    );
    assert!(!engine.state.tickets[&ticket1_id].is_open());
    assert_eq!(
        engine.state.tickets[&ticket1_id]
            .ended
            .as_ref()
            .unwrap()
            .outcome,
        "merged"
    );
}
