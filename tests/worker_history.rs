use nexus::core::engine::{Engine, EngineState};
use nexus::core::model::*;
use nexus::workers::dispatcher::build_worker_prompt_with_memory;
use serde_json::{Value, json};

fn address(value: Value) -> ChannelAddress {
    serde_json::from_value(value).unwrap()
}

fn kind(ch: &ChannelAddress) -> SourceKindId {
    match ch {
        ChannelAddress::FeishuTopic { .. } => SourceKindId::Feishu,
        ChannelAddress::GithubIssueComment { .. } | ChannelAddress::GithubReviewReply { .. } => {
            SourceKindId::Github
        }
        ChannelAddress::GwsGmailDraft { .. } => SourceKindId::Gws,
        ChannelAddress::GenericIm { source_kind, .. } => *source_kind,
    }
}

fn trigger(id: &str, channel: ChannelAddress, thread: &str) -> Trigger {
    let source = serde_json::to_value(&channel).unwrap()["source_id"]
        .as_str()
        .unwrap()
        .to_owned();
    Trigger {
        id: id.into(),
        source_id: source,
        source_kind: kind(&channel),
        external_id: id.into(),
        thread_key: thread.into(),
        actor_id: "human".into(),
        actor_is_bot: false,
        addressed_agent: true,
        text: id.into(),
        reply_to_trigger_id: None,
        inline_target: None,
        reply_channel: Some(channel),
        at: "2026-11-05T00:00:00Z".into(),
        meta: Default::default(),
    }
}

fn fixture(channel: ChannelAddress) -> (Ticket, EngineState) {
    let mut engine = Engine::new("/tmp/history-test");
    engine.receive_trigger(
        trigger("origin", channel, "origin-thread"),
        AdmissionStrength::Explicit,
    );
    let id = engine
        .accept_ticket(
            "origin",
            "History test",
            None,
            AdmissionStrength::Explicit,
            vec![],
            TaskKind::QuickCode.default_target(),
            None,
            "2026-11-05T00:00:00Z",
        )
        .unwrap();
    (engine.state.tickets[&id].clone(), engine.state)
}

fn outbox(state: &mut EngineState, id: &str, channel: ChannelAddress) {
    state.outbox.insert(
        id.into(),
        OutboxMessage {
            id: id.into(),
            ticket_id: Some("another-ticket".into()),
            channel,
            purpose: OutboxPurpose::Answer,
            body: id.into(),
            status: OutboxStatus::Sent,
            attempts: 1,
            last_error: None,
            created_at: "2026-11-05T00:01:00Z".into(),
        },
    );
}

fn history(ticket: &Ticket, state: &EngineState) -> String {
    let prompt = build_worker_prompt_with_memory(ticket, &ticket.assignments[0], Some(state), None);
    prompt
        .split("## Recent Conversation History in This Chat\n")
        .nth(1)
        .map(|s| s.split("\n\n## ").next().unwrap().to_owned())
        .unwrap_or_default()
}

fn addresses() -> Vec<Value> {
    vec![
        json!({"kind":"feishu_topic","source_id":"source","chat_id":"chat","root_message_id":"root","reply_to_message_id":"msg"}),
        json!({"kind":"github_issue_comment","source_id":"source","repo":"owner/repo","number":1}),
        json!({"kind":"github_review_reply","source_id":"source","repo":"owner/repo","pull_number":1,"comment_id":10}),
        json!({"kind":"gws_gmail_draft","source_id":"source","thread_id":"thread","to":"a@example.test","subject":"subject"}),
        json!({"kind":"generic_im","source_id":"source","source_kind":"slack","conversation_id":"chat","thread_id":"thread"}),
        json!({"kind":"generic_im","source_id":"source","source_kind":"dingtalk","conversation_id":"chat","thread_id":"msg|human"}),
    ]
}

#[test]
fn isolates_every_channel_source_and_conversation_component() {
    for value in addresses() {
        let channel = address(value.clone());
        let (ticket, mut state) = fixture(channel.clone());
        let valid = trigger("allowed-user", channel.clone(), "origin-thread");
        state.triggers.insert(valid.id.clone(), valid);
        outbox(&mut state, "allowed-assistant", channel);

        let mut negatives = Vec::new();
        for field in [
            "source_id",
            "chat_id",
            "root_message_id",
            "repo",
            "number",
            "pull_number",
            "thread_id",
            "conversation_id",
            "source_kind",
        ] {
            if value.get(field).is_some() {
                let mut changed = value.clone();
                changed[field] = if value[field].is_number() {
                    json!(99)
                } else if field == "source_kind" {
                    json!("telegram")
                } else {
                    json!("other")
                };
                negatives.push(address(changed));
            }
        }
        negatives.extend(
            addresses()
                .into_iter()
                .filter(|other| {
                    other["kind"] != value["kind"]
                        && !(other["kind"].as_str().unwrap().starts_with("github_")
                            && value["kind"].as_str().unwrap().starts_with("github_"))
                })
                .map(address),
        );
        for (i, other) in negatives.into_iter().enumerate() {
            let tr = trigger(
                &format!("forbidden-user-{i}"),
                other.clone(),
                "other-thread",
            );
            state.triggers.insert(tr.id.clone(), tr);
            outbox(&mut state, &format!("forbidden-assistant-{i}"), other);
        }
        let result = history(&ticket, &state);
        assert!(result.contains("allowed-user"), "{value}: {result}");
        assert!(result.contains("allowed-assistant"), "{value}: {result}");
        assert!(!result.contains("forbidden"), "{value}: {result}");
    }
}

#[test]
fn preserves_stable_topics_and_explicit_multiple_channels() {
    let variants = addresses();
    let (mut ticket, mut state) = fixture(address(variants[0].clone()));
    let mut feishu_reply = variants[0].clone();
    feishu_reply["reply_to_message_id"] = json!("new-message");
    let reply = trigger(
        "feishu-followup",
        address(feishu_reply.clone()),
        "origin-thread",
    );
    state.triggers.insert(reply.id.clone(), reply);
    outbox(&mut state, "feishu-answer", address(feishu_reply));
    let mut second_chat = variants[0].clone();
    second_chat["chat_id"] = json!("explicitly-linked-chat");
    ticket.channels.push(address(second_chat.clone()));
    outbox(&mut state, "second-feishu-answer", address(second_chat));

    // These channels are explicitly attached to the same ticket, not just the first chat.
    for i in [1, 3, 5] {
        let channel = address(variants[i].clone());
        ticket.channels.push(channel.clone());
        let tr = trigger(
            &format!("linked-{i}"),
            channel.clone(),
            &format!("linked-thread-{i}"),
        );
        state.triggers.insert(tr.id.clone(), tr);
        outbox(&mut state, &format!("linked-answer-{i}"), channel);
    }
    let mut ding_reply = variants[5].clone();
    ding_reply["thread_id"] = json!("new-msg|another-human");
    let reply = trigger(
        "ding-followup",
        address(ding_reply.clone()),
        "linked-thread-5",
    );
    state.triggers.insert(reply.id.clone(), reply);
    outbox(&mut state, "ding-answer", address(ding_reply));
    let mut gmail_reply = variants[3].clone();
    gmail_reply["to"] = json!("another@example.test");
    gmail_reply["subject"] = json!("Re: subject");
    outbox(&mut state, "gmail-answer", address(gmail_reply));

    let result = history(&ticket, &state);
    for expected in [
        "feishu-followup",
        "feishu-answer",
        "second-feishu-answer",
        "linked-1",
        "linked-3",
        "linked-5",
        "linked-answer-1",
        "linked-answer-3",
        "linked-answer-5",
        "ding-followup",
        "ding-answer",
        "gmail-answer",
    ] {
        assert!(result.contains(expected), "missing {expected}: {result}");
    }
}

#[test]
fn github_issue_pr_comments_and_reviews_share_the_issue_or_pr_thread() {
    let value = addresses()[2].clone();
    let (ticket, mut state) = fixture(address(value.clone()));
    let mut second = value.clone();
    second["comment_id"] = json!(11);
    let mut reply = trigger("review-followup", address(second.clone()), "origin-thread");
    // A missing original comment does not break PR-wide continuity.
    reply.reply_to_trigger_id = Some("unavailable-parent".into());
    state.triggers.insert(reply.id.clone(), reply);
    outbox(&mut state, "review-answer", address(second));
    outbox(
        &mut state,
        "top-level-answer",
        address(addresses()[1].clone()),
    );
    let mut other = value;
    other["pull_number"] = json!(2);
    let tr = trigger("forbidden-pr", address(other.clone()), "other-thread");
    state.triggers.insert(tr.id.clone(), tr);
    outbox(&mut state, "forbidden-answer", address(other));
    let result = history(&ticket, &state);
    for expected in ["review-followup", "review-answer", "top-level-answer"] {
        assert!(result.contains(expected), "{result}");
    }
    assert!(!result.contains("forbidden"), "{result}");
}

#[test]
fn missing_addresses_use_explicit_source_qualified_ticket_threads_only() {
    let (mut ticket, mut state) = fixture(address(addresses()[0].clone()));
    ticket.channels.clear();
    state.triggers.get_mut("origin").unwrap().reply_channel = None;
    for (id, source, thread) in [
        ("allowed", "source", "origin-thread"),
        ("forbidden-source", "other", "origin-thread"),
        ("forbidden-thread", "source", "other-thread"),
    ] {
        let mut tr = trigger(id, address(addresses()[0].clone()), thread);
        tr.reply_channel = None;
        tr.source_id = source.into();
        state.triggers.insert(tr.id.clone(), tr);
    }
    let result = history(&ticket, &state);
    assert!(result.contains("allowed"));
    assert!(!result.contains("forbidden"));
    state.triggers.remove("origin");
    state.threads.clear();
    assert!(history(&ticket, &state).is_empty());
}

#[test]
fn explicit_context_and_followed_threads_include_only_their_configured_source() {
    for followed in [false, true] {
        let (mut ticket, mut state) = fixture(address(addresses()[0].clone()));
        let mut channel = addresses()[3].clone();
        channel["source_id"] = json!("mail-account");
        let tr = trigger("linked-user", address(channel.clone()), "linked-thread");
        let mut engine = Engine::from_state(state);
        engine.receive_trigger(tr, AdmissionStrength::Explicit);
        state = engine.state;
        if followed {
            ticket.follows.push(FollowedThread {
                thread_key: "linked-thread".into(),
                own: false,
                closed: None,
            });
        } else {
            ticket.context_subscriptions.insert("linked-thread".into());
        }
        outbox(&mut state, "linked-answer", address(channel.clone()));
        channel["source_id"] = json!("another-mail-account");
        let other = trigger(
            "forbidden-source",
            address(channel.clone()),
            "linked-thread",
        );
        state.triggers.insert(other.id.clone(), other);
        outbox(&mut state, "forbidden-answer", address(channel));
        let result = history(&ticket, &state);
        assert!(result.contains("linked-user"), "{result}");
        assert!(result.contains("linked-answer"), "{result}");
        assert!(!result.contains("forbidden"), "{result}");
    }
}

#[test]
fn trigger_envelope_and_address_must_agree_on_source_and_kind() {
    let ch = address(addresses()[0].clone());
    let (ticket, mut state) = fixture(ch.clone());
    for field in ["source", "kind"] {
        let mut tr = trigger(&format!("forbidden-{field}"), ch.clone(), "origin-thread");
        if field == "source" {
            tr.source_id = "another-source".into();
        } else {
            tr.source_kind = SourceKindId::Slack;
        }
        state.triggers.insert(tr.id.clone(), tr);
    }
    assert!(!history(&ticket, &state).contains("forbidden"));
}

#[test]
fn colliding_thread_records_cannot_broaden_the_origin_to_another_account() {
    let value = addresses()[3].clone();
    let (ticket, mut state) = fixture(address(value.clone()));
    let thread = state.threads.get_mut("origin-thread").unwrap();
    thread.source_id = "another-account".into();
    let mut other = value;
    other["source_id"] = json!("another-account");
    let mut tr = trigger("forbidden-user", address(other.clone()), "origin-thread");
    state.triggers.insert(tr.id.clone(), tr.clone());
    tr.id = "forbidden-addressless".into();
    tr.reply_channel = None;
    state.triggers.insert(tr.id.clone(), tr);
    outbox(&mut state, "forbidden-answer", address(other));
    assert!(!history(&ticket, &state).contains("forbidden"));
}

#[test]
fn overwritten_origin_cannot_authorize_another_accounts_addressless_history() {
    for keep_initial_input in [false, true] {
        for changed_field in ["source", "thread"] {
            let value = addresses()[3].clone();
            let (mut ticket, mut state) = fixture(address(value.clone()));
            if !keep_initial_input {
                ticket.inputs.clear();
            }
            let mut overwritten = state.triggers["origin"].clone();
            if changed_field == "source" {
                overwritten.source_id = "another-account".into();
                let mut other = value.clone();
                other["source_id"] = json!("another-account");
                overwritten.reply_channel = Some(address(other));
            } else {
                overwritten.thread_key = "another-thread".into();
            }
            state.triggers.insert("origin".into(), overwritten.clone());
            let mut forbidden = overwritten;
            forbidden.id = "forbidden-addressless".into();
            forbidden.text = "forbidden-addressless".into();
            forbidden.reply_channel = None;
            state.triggers.insert(forbidden.id.clone(), forbidden);

            let mut allowed = trigger("allowed-addressless", address(value), "origin-thread");
            allowed.reply_channel = None;
            state.triggers.insert(allowed.id.clone(), allowed);
            let result = history(&ticket, &state);
            assert!(
                !result.contains("forbidden"),
                "{changed_field} / initial input {keep_initial_input}: {result}"
            );
            assert!(result.contains("allowed-addressless"), "{result}");
        }
    }
}

#[test]
fn overwritten_non_origin_input_cannot_override_explicit_channel_bindings() {
    for overwrite_thread_record in [false, true] {
        for changed_field in ["source", "thread"] {
            let (ticket, state) = fixture(address(addresses()[3].clone()));
            let mut linked_address = addresses()[3].clone();
            linked_address["thread_id"] = json!("linked-gmail-thread");
            let mut engine = Engine::from_state(state);
            engine.receive_trigger(
                trigger(
                    "linked-input",
                    address(linked_address.clone()),
                    "linked-thread",
                ),
                AdmissionStrength::Explicit,
            );
            engine
                .attach_trigger_to_ticket("linked-input", &ticket.id, "2026-11-05T00:00:01Z")
                .unwrap();
            let ticket = engine.state.tickets[&ticket.id].clone();
            let mut state = engine.state;
            let mut overwritten = state.triggers["linked-input"].clone();
            if changed_field == "source" {
                overwritten.source_id = "another-account".into();
                let mut other = linked_address.clone();
                other["source_id"] = json!("another-account");
                overwritten.reply_channel = Some(address(other));
            } else {
                overwritten.thread_key = "another-thread".into();
            }
            if overwrite_thread_record {
                let thread = state.threads.get_mut("linked-thread").unwrap();
                thread.source_id = overwritten.source_id.clone();
                thread.thread_key = overwritten.thread_key.clone();
            }
            state
                .triggers
                .insert("linked-input".into(), overwritten.clone());
            overwritten.id = "forbidden-addressless".into();
            overwritten.text = "forbidden-addressless".into();
            overwritten.reply_channel = None;
            state.triggers.insert(overwritten.id.clone(), overwritten);
            outbox(
                &mut state,
                "allowed-explicit-answer",
                address(linked_address),
            );
            let result = history(&ticket, &state);
            assert!(
                !result.contains("forbidden"),
                "{changed_field} / overwritten thread {overwrite_thread_record}: {result}"
            );
            assert!(result.contains("allowed-explicit-answer"), "{result}");
        }
    }
}

#[test]
fn invalid_or_missing_identities_never_match_globally() {
    for value in addresses() {
        for field in [
            "source_id",
            "chat_id",
            "root_message_id",
            "repo",
            "thread_id",
            "conversation_id",
        ] {
            if value.get(field).is_some() {
                let mut invalid = value.clone();
                invalid[field] = json!("");
                let ch = address(invalid);
                let (ticket, mut state) = fixture(ch.clone());
                let tr = trigger("forbidden-user", ch.clone(), "origin-thread");
                state.triggers.insert(tr.id.clone(), tr);
                outbox(&mut state, "forbidden-answer", ch);
                assert!(history(&ticket, &state).is_empty(), "{value} / {field}");
            }
        }
    }
    let ch = address(addresses()[5].clone());
    let (ticket, mut state) = fixture(ch.clone());
    state.triggers.clear();
    outbox(&mut state, "forbidden-ding", ch);
    assert!(history(&ticket, &state).is_empty());

    let ch = address(addresses()[5].clone());
    let (ticket, mut state) = fixture(ch.clone());
    let ambiguous = trigger("ambiguous", ch.clone(), "different-thread");
    state.triggers.insert(ambiguous.id.clone(), ambiguous);
    outbox(&mut state, "forbidden-ambiguous-ding", ch);
    assert!(history(&ticket, &state).is_empty());
}

#[test]
fn generic_im_unthreaded_chat_is_distinct_from_threads_and_source_kinds() {
    for source_kind in ["slack", "telegram", "wechat"] {
        let value = json!({"kind":"generic_im","source_id":"source","source_kind":source_kind,"conversation_id":"chat","thread_id":null});
        let (ticket, mut state) = fixture(address(value.clone()));
        outbox(&mut state, "allowed", address(value.clone()));
        let mut threaded = value;
        threaded["thread_id"] = json!("unthreaded");
        outbox(&mut state, "forbidden-thread", address(threaded));
        let result = history(&ticket, &state);
        assert!(result.contains("allowed"));
        assert!(!result.contains("forbidden"));
    }
}

#[test]
fn history_is_chronological_limited_to_sixteen_and_omits_unread_and_unsent() {
    let ch = address(addresses()[1].clone());
    let (ticket, mut state) = fixture(ch.clone());
    // Make the unread origin newer than every history item so the 16-message
    // cutoff cannot accidentally hide a regression in unread exclusion.
    state.triggers.get_mut("origin").unwrap().at = "2026-11-05T00:02:00Z".into();
    for i in (0..20).rev() {
        let mut tr = trigger(&format!("user-{i:02}"), ch.clone(), "origin-thread");
        tr.at = format!("2026-11-05T00:00:{i:02}Z");
        state.triggers.insert(tr.id.clone(), tr);
    }
    for (id, status) in [
        ("forbidden-pending", OutboxStatus::Pending),
        ("forbidden-dropped", OutboxStatus::Dropped),
    ] {
        outbox(&mut state, id, ch.clone());
        state.outbox.get_mut(id).unwrap().status = status;
    }
    outbox(&mut state, "assistant-latest", ch);
    let result = history(&ticket, &state);
    let lines: Vec<_> = result.lines().collect();
    assert_eq!(lines.len(), 16, "{result}");
    assert!(lines[0].contains("user-05"));
    assert!(lines[14].contains("user-19"));
    assert!(lines[15].contains("assistant-latest"));
    assert!(!result.contains("origin"));
    assert!(!result.contains("forbidden"));
}
