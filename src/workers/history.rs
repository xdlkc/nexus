//! Conversation identities for automatic history injection. Delivery targets are not
//! always thread identities (notably Feishu, DingTalk, and GitHub review replies).

use crate::core::engine::EngineState;
use crate::core::model::{ChannelAddress, SourceKindId, Ticket, TicketInputKind, Trigger};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Hash)]
struct DingTalkAddress {
    source: String,
    kind: SourceKindId,
    conversation: String,
    delivery_thread: Option<String>,
}

impl DingTalkAddress {
    fn from_channel(address: &ChannelAddress) -> Option<Self> {
        match address {
            ChannelAddress::GenericIm {
                source_id,
                source_kind: SourceKindId::Dingtalk,
                conversation_id,
                thread_id,
            } => Some(Self {
                source: source_id.clone(),
                kind: SourceKindId::Dingtalk,
                conversation: conversation_id.clone(),
                delivery_thread: thread_id.clone(),
            }),
            _ => None,
        }
    }
}

/// Built once per HistoryScope. None permanently marks an ambiguous address;
/// another duplicate of either thread must never make it usable again.
struct DingTalkThreads {
    by_address: HashMap<DingTalkAddress, Option<String>>,
}

impl DingTalkThreads {
    fn new<'a>(triggers: impl IntoIterator<Item = &'a Trigger>) -> Self {
        #[cfg(test)]
        tests::INDEX_BUILDS.with(|count| count.set(count.get() + 1));
        let mut by_address: HashMap<DingTalkAddress, Option<String>> = HashMap::new();
        for trigger in triggers {
            #[cfg(test)]
            tests::TRIGGER_VISITS.with(|count| count.set(count.get() + 1));
            let Some(key) = trigger
                .reply_channel
                .as_ref()
                .and_then(DingTalkAddress::from_channel)
            else {
                continue;
            };
            if key.source != trigger.source_id
                || key.kind != trigger.source_kind
                || !present(&trigger.thread_key)
            {
                continue;
            }
            by_address
                .entry(key)
                .and_modify(|thread| {
                    if thread.as_deref() != Some(trigger.thread_key.as_str()) {
                        *thread = None;
                    }
                })
                .or_insert_with(|| Some(trigger.thread_key.clone()));
        }
        Self { by_address }
    }

    fn thread(&self, address: &ChannelAddress) -> Option<&str> {
        self.by_address
            .get(&DingTalkAddress::from_channel(address)?)?
            .as_deref()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Conversation {
    source: String,
    kind: SourceKindId,
    // Separate components avoid collisions when external IDs contain separators.
    path: Vec<String>,
}

fn present(value: &str) -> bool {
    !value.trim().is_empty()
}

fn conversation(address: &ChannelAddress, dingtalk: &DingTalkThreads) -> Option<Conversation> {
    let (source, kind, path) = match address {
        ChannelAddress::FeishuTopic {
            source_id,
            chat_id,
            root_message_id,
            ..
        } => (
            source_id,
            SourceKindId::Feishu,
            vec!["topic".into(), chat_id.clone(), root_message_id.clone()],
        ),
        ChannelAddress::GithubIssueComment {
            source_id,
            repo,
            number,
        } if *number > 0 => (
            source_id,
            SourceKindId::Github,
            vec!["issue".into(), repo.clone(), number.to_string()],
        ),
        // Domain Thread semantics group all top-level comments and reviews on
        // the same Issue/PR. comment_id selects delivery, not conversation scope.
        ChannelAddress::GithubReviewReply {
            source_id,
            repo,
            pull_number,
            ..
        } if *pull_number > 0 => (
            source_id,
            SourceKindId::Github,
            vec!["issue".into(), repo.clone(), pull_number.to_string()],
        ),
        ChannelAddress::GwsGmailDraft {
            source_id,
            thread_id,
            ..
        } => (
            source_id,
            SourceKindId::Gws,
            vec!["gmail".into(), thread_id.clone()],
        ),
        ChannelAddress::GenericIm {
            source_id,
            source_kind,
            conversation_id,
            thread_id,
        } => {
            if thread_id.as_ref().is_some_and(|id| !present(id)) {
                return None;
            }
            let thread = if *source_kind == SourceKindId::Dingtalk {
                // DingTalk's thread_id is message_id|sender_id for quoted delivery.
                // Recover its stable topic from the exact originating address.
                dingtalk.thread(address)?.to_owned()
            } else {
                match thread_id {
                    Some(id) if present(id) => id.clone(),
                    Some(_) => return None,
                    None => "unthreaded".into(),
                }
            };
            // Keep None distinct from an actual thread named "unthreaded".
            let mode = if thread_id.is_none() {
                "chat"
            } else {
                "thread"
            };
            (
                source_id,
                *source_kind,
                vec!["im".into(), conversation_id.clone(), mode.into(), thread],
            )
        }
        _ => return None,
    };
    if !present(source) || path.iter().any(|part| !present(part)) {
        return None;
    }
    Some(Conversation {
        source: source.clone(),
        kind,
        path,
    })
}

pub(super) struct HistoryScope {
    dingtalk: DingTalkThreads,
    conversations: Vec<Conversation>,
    // Only used for triggers without a reply address. Thread keys alone are not
    // globally namespaced by configured source (e.g. two Gmail accounts).
    addressless_threads: Vec<(String, SourceKindId, String)>,
}

impl HistoryScope {
    pub(super) fn new(ticket: &Ticket, state: &EngineState) -> Self {
        let dingtalk = DingTalkThreads::new(state.triggers.values());
        let mut conversations: Vec<_> = ticket
            .channels
            .iter()
            .filter_map(|ch| conversation(ch, &dingtalk))
            .collect();
        let mut addressless_threads = Vec::new();
        let mut rejected_input_threads = std::collections::BTreeSet::new();
        let matches_origin = |tr: &Trigger| {
            tr.source_id == ticket.mandate.source_id
                && tr.thread_key == ticket.origin_thread_key
                && (!conversations.iter().any(|ch| ch.source == tr.source_id)
                    || conversations
                        .iter()
                        .any(|ch| ch.source == tr.source_id && ch.kind == tr.source_kind))
        };
        let mut add_trigger = |tr: &Trigger| {
            if present(&tr.source_id) && present(&tr.thread_key) {
                addressless_threads.push((
                    tr.source_id.clone(),
                    tr.source_kind,
                    tr.thread_key.clone(),
                ));
            }
        };
        // Adapter trigger IDs can collide across configured accounts. The
        // ticket's durable origin/source bindings outrank a current ID lookup.
        if let Some(origin) = state.triggers.get(&ticket.origin_trigger_id)
            && matches_origin(origin)
        {
            add_trigger(origin);
        }
        for input in &ticket.inputs {
            if input.handed_back_reason.is_none()
                && let TicketInputKind::TriggerFanOut {
                    trigger_id,
                    thread_key,
                    ..
                } = &input.kind
            {
                let is_origin = trigger_id == &ticket.origin_trigger_id
                    || thread_key == &ticket.origin_thread_key;
                // Context fan-out does not add a reply channel. Its explicit
                // Thread binding supplies the configured source instead.
                let context_thread = state.threads.get(thread_key).filter(|thread| {
                    thread.thread_key == *thread_key
                        && (ticket.context_subscriptions.contains(thread_key)
                            || ticket
                                .follows
                                .iter()
                                .any(|follow| follow.thread_key == *thread_key))
                });
                let valid = state.triggers.get(trigger_id).filter(|tr| {
                    tr.thread_key == *thread_key
                        && if is_origin {
                            thread_key == &ticket.origin_thread_key && matches_origin(tr)
                        } else {
                            tr.reply_channel
                                .as_ref()
                                .and_then(|ch| conversation(ch, &dingtalk))
                                .is_some_and(|identity| {
                                    identity.source == tr.source_id
                                        && identity.kind == tr.source_kind
                                        && (conversations.contains(&identity)
                                            || (!conversations.iter().any(|known| {
                                                known.kind == identity.kind
                                                    && known.path == identity.path
                                            }) && context_thread.is_some_and(|thread| {
                                                thread.source_id == tr.source_id
                                                    && thread.source_kind == tr.source_kind
                                            })))
                                })
                        }
                });
                if let Some(tr) = valid {
                    add_trigger(tr);
                } else if !is_origin {
                    // A rejected mutable lookup cannot be reauthorized through
                    // a same-key Thread record. Explicit ticket channels still
                    // provide their independently verified conversation context.
                    rejected_input_threads.insert(thread_key.clone());
                }
            }
        }
        for key in ticket
            .subscriptions
            .iter()
            .chain(&ticket.context_subscriptions)
            .chain(ticket.follows.iter().map(|follow| &follow.thread_key))
        {
            if rejected_input_threads.contains(key) {
                continue;
            }
            if let Some(thread) = state.threads.get(key)
                && present(&thread.source_id)
                && present(key)
            {
                // Thread storage is keyed by an external thread_key alone. Do
                // not let a same-key record from another configured account
                // override identities already bound by this ticket's inputs.
                let bound: Vec<_> = addressless_threads
                    .iter()
                    .filter(|(_, _, thread_key)| thread_key == key)
                    .collect();
                if (!bound.is_empty()
                    && !bound.iter().any(|(source, kind, _)| {
                        source == &thread.source_id && kind == &thread.source_kind
                    }))
                    || (key == &ticket.origin_thread_key
                        && thread.source_id != ticket.mandate.source_id)
                {
                    continue;
                }
                addressless_threads.push((
                    thread.source_id.clone(),
                    thread.source_kind,
                    key.clone(),
                ));
                // Context-only and followed threads may have no outbound channel
                // on the ticket. Resolve their addresses from source-qualified events.
                for trigger in state.triggers.values().filter(|tr| {
                    tr.thread_key == *key
                        && tr.source_id == thread.source_id
                        && tr.source_kind == thread.source_kind
                }) {
                    if let Some(address) = &trigger.reply_channel
                        && let Some(identity) = conversation(address, &dingtalk)
                        && identity.source == trigger.source_id
                        && identity.kind == trigger.source_kind
                        && !conversations.contains(&identity)
                    {
                        conversations.push(identity);
                    }
                }
            }
        }
        Self {
            dingtalk,
            conversations,
            addressless_threads,
        }
    }

    pub(super) fn contains_address(&self, address: &ChannelAddress) -> bool {
        conversation(address, &self.dingtalk).is_some_and(|key| self.conversations.contains(&key))
    }

    pub(super) fn contains_trigger(&self, trigger: &Trigger) -> bool {
        match &trigger.reply_channel {
            Some(address) => conversation(address, &self.dingtalk).is_some_and(|key| {
                key.source == trigger.source_id
                    && key.kind == trigger.source_kind
                    && self.conversations.contains(&key)
            }),
            None => self
                .addressless_threads
                .iter()
                .any(|(source, kind, thread)| {
                    source == &trigger.source_id
                        && kind == &trigger.source_kind
                        && thread == &trigger.thread_key
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::Engine;
    use crate::core::model::{
        AdmissionStrength, OutboxMessage, OutboxPurpose, OutboxStatus, TaskKind,
    };
    use crate::workers::dispatcher::build_worker_prompt_with_memory;
    use std::cell::Cell;

    thread_local! {
        pub(super) static INDEX_BUILDS: Cell<usize> = const { Cell::new(0) };
        pub(super) static TRIGGER_VISITS: Cell<usize> = const { Cell::new(0) };
    }

    fn address(source: &str, conversation: &str, delivery: Option<&str>) -> ChannelAddress {
        ChannelAddress::GenericIm {
            source_id: source.into(),
            source_kind: SourceKindId::Dingtalk,
            conversation_id: conversation.into(),
            thread_id: delivery.map(str::to_owned),
        }
    }

    fn trigger(id: &str, address: ChannelAddress, thread: &str) -> Trigger {
        let (source, kind) = match &address {
            ChannelAddress::GenericIm {
                source_id,
                source_kind,
                ..
            } => (source_id.clone(), *source_kind),
            ChannelAddress::GwsGmailDraft { source_id, .. } => {
                (source_id.clone(), SourceKindId::Gws)
            }
            _ => unreachable!(),
        };
        Trigger {
            id: id.into(),
            source_id: source,
            source_kind: kind,
            external_id: id.into(),
            thread_key: thread.into(),
            actor_id: "human".into(),
            actor_is_bot: false,
            addressed_agent: true,
            text: id.into(),
            reply_to_trigger_id: None,
            inline_target: None,
            reply_channel: Some(address),
            at: "2026-11-05T00:00:00Z".into(),
            meta: Default::default(),
        }
    }

    #[test]
    fn duplicate_threads_stay_valid_and_conflicts_stay_ambiguous() {
        let ch = address("source", "chat", Some("msg|sender"));
        let mut events = vec![
            trigger("a1", ch.clone(), "topic-a"),
            trigger("a2", ch.clone(), "topic-a"),
        ];
        events.push(trigger("blank", ch.clone(), "  "));
        let mut wrong_source = trigger("wrong-source", ch.clone(), "topic-b");
        wrong_source.source_id = "another-account".into();
        events.push(wrong_source);
        let mut wrong_kind = trigger("wrong-kind", ch.clone(), "topic-b");
        wrong_kind.source_kind = SourceKindId::Slack;
        events.push(wrong_kind);
        assert_eq!(DingTalkThreads::new(&events).thread(&ch), Some("topic-a"));
        events.push(trigger("b", ch.clone(), "topic-b"));
        events.push(trigger("a3", ch.clone(), "topic-a"));
        let index = DingTalkThreads::new(&events);
        assert_eq!(index.thread(&ch), None);
        assert!(conversation(&ch, &index).is_none());
        assert_eq!(
            index.thread(&address("source", "chat", Some("missing|sender"))),
            None
        );
    }

    #[test]
    fn index_keys_keep_all_address_components_distinct() {
        let addresses = [
            address("source", "chat", Some("msg|sender")),
            address("another-source", "chat", Some("msg|sender")),
            address("source", "another-chat", Some("msg|sender")),
            address("source", "chat", Some("another-msg|sender")),
            address("source", "chat", None),
            address("source", "chat", Some("unthreaded")),
            address("source:chat", "conversation", Some("msg|sender")),
            address("source", "chat:conversation", Some("msg|sender")),
        ];
        let events: Vec<_> = addresses
            .iter()
            .enumerate()
            .map(|(i, ch)| trigger(&format!("event-{i}"), ch.clone(), &format!("topic-{i}")))
            .collect();
        let index = DingTalkThreads::new(&events);
        assert_eq!(index.by_address.len(), addresses.len());
        for (i, ch) in addresses.iter().enumerate() {
            assert_eq!(index.thread(ch), Some(format!("topic-{i}").as_str()));
        }
        let mut other_kind = addresses[0].clone();
        if let ChannelAddress::GenericIm { source_kind, .. } = &mut other_kind {
            *source_kind = SourceKindId::Slack;
        }
        assert_eq!(index.thread(&other_kind), None);
        let mut invalid = trigger(
            "blank-only",
            address("source", "chat", Some("blank|sender")),
            "",
        );
        let empty = DingTalkThreads::new([&invalid]);
        assert_eq!(empty.thread(invalid.reply_channel.as_ref().unwrap()), None);
        invalid.thread_key = "topic".into();
        invalid.source_id = "wrong-envelope".into();
        assert_eq!(
            DingTalkThreads::new([&invalid]).thread(invalid.reply_channel.as_ref().unwrap()),
            None
        );
    }

    #[test]
    fn full_history_builds_one_index_and_queries_never_revisit_retained_triggers() {
        for size in [1, 32, 512] {
            for unrelated_ticket in [false, true] {
                let origin_address = if unrelated_ticket {
                    ChannelAddress::GwsGmailDraft {
                        source_id: "mail".into(),
                        thread_id: "mail-thread".into(),
                        to: "test@example.test".into(),
                        subject: "test".into(),
                    }
                } else {
                    address("source", "chat", Some("origin|sender"))
                };
                let mut engine = Engine::new("/tmp/history-index-test");
                engine.receive_trigger(
                    trigger("origin", origin_address.clone(), "origin-thread"),
                    AdmissionStrength::Explicit,
                );
                let id = engine
                    .accept_ticket(
                        "origin",
                        "Index test",
                        None,
                        AdmissionStrength::Explicit,
                        vec![],
                        TaskKind::QuickCode.default_target(),
                        None,
                        "2026-11-05T00:00:00Z",
                    )
                    .unwrap();
                let mut ticket = engine.state.tickets[&id].clone();
                // Exercise repeated normalization during scope construction too.
                ticket
                    .channels
                    .extend([origin_address.clone(), origin_address]);
                for i in 0..size {
                    let channel = address("source", "chat", Some(&format!("message-{i}|sender")));
                    let event = trigger(
                        &format!("history-{i}"),
                        channel.clone(),
                        if unrelated_ticket {
                            "unrelated-topic"
                        } else {
                            "origin-thread"
                        },
                    );
                    engine.state.triggers.insert(event.id.clone(), event);
                    engine.state.outbox.insert(
                        format!("outbox-{i}"),
                        OutboxMessage {
                            id: format!("outbox-{i}"),
                            ticket_id: None,
                            channel,
                            purpose: OutboxPurpose::Answer,
                            body: format!("outbox-{i}"),
                            status: OutboxStatus::Sent,
                            attempts: 1,
                            last_error: None,
                            created_at: "2026-11-05T00:01:00Z".into(),
                        },
                    );
                }
                INDEX_BUILDS.set(0);
                TRIGGER_VISITS.set(0);
                let prompt = build_worker_prompt_with_memory(
                    &ticket,
                    &ticket.assignments[0],
                    Some(&engine.state),
                    None,
                );
                assert_eq!(
                    INDEX_BUILDS.get(),
                    1,
                    "size {size}, unrelated {unrelated_ticket}"
                );
                assert_eq!(TRIGGER_VISITS.get(), size + 1);
                assert_eq!(prompt.contains("outbox-"), !unrelated_ticket);

                let scope = HistoryScope::new(&ticket, &engine.state);
                assert_eq!(INDEX_BUILDS.get(), 2);
                assert_eq!(TRIGGER_VISITS.get(), 2 * (size + 1));
                // Membership APIs cannot access EngineState: all full-history
                // trigger/outbox queries below reuse this one snapshot index.
                for _ in 0..4 {
                    for event in engine
                        .state
                        .triggers
                        .values()
                        .filter(|event| event.id != "origin")
                    {
                        assert_eq!(scope.contains_trigger(event), !unrelated_ticket);
                    }
                    for message in engine.state.outbox.values() {
                        assert_eq!(scope.contains_address(&message.channel), !unrelated_ticket);
                    }
                }
                assert_eq!(INDEX_BUILDS.get(), 2);
                assert_eq!(TRIGGER_VISITS.get(), 2 * (size + 1));
            }
        }
    }
}
