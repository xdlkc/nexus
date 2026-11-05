//! Conversation identities for automatic history injection. Delivery targets are not
//! always thread identities (notably Feishu, DingTalk, and GitHub review replies).

use crate::core::engine::EngineState;
use crate::core::model::{ChannelAddress, SourceKindId, Ticket, TicketInputKind, Trigger};

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

fn conversation(address: &ChannelAddress, state: &EngineState) -> Option<Conversation> {
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
                let mut matches = state.triggers.values().filter(|tr| {
                    tr.source_id == *source_id
                        && tr.source_kind == *source_kind
                        && tr.reply_channel.as_ref() == Some(address)
                        && present(&tr.thread_key)
                });
                let key = matches.next()?.thread_key.clone();
                if matches.any(|tr| tr.thread_key != key) {
                    return None;
                }
                key
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
    conversations: Vec<Conversation>,
    // Only used for triggers without a reply address. Thread keys alone are not
    // globally namespaced by configured source (e.g. two Gmail accounts).
    addressless_threads: Vec<(String, SourceKindId, String)>,
}

impl HistoryScope {
    pub(super) fn new(ticket: &Ticket, state: &EngineState) -> Self {
        let mut conversations: Vec<_> = ticket
            .channels
            .iter()
            .filter_map(|ch| conversation(ch, state))
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
                let valid = state.triggers.get(trigger_id).filter(|tr| {
                    tr.thread_key == *thread_key
                        && if is_origin {
                            thread_key == &ticket.origin_thread_key && matches_origin(tr)
                        } else {
                            tr.reply_channel
                                .as_ref()
                                .and_then(|ch| conversation(ch, state))
                                .is_some_and(|identity| {
                                    identity.source == tr.source_id
                                        && identity.kind == tr.source_kind
                                        && conversations.contains(&identity)
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
                        && let Some(identity) = conversation(address, state)
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
            conversations,
            addressless_threads,
        }
    }

    pub(super) fn contains_address(&self, address: &ChannelAddress, state: &EngineState) -> bool {
        conversation(address, state).is_some_and(|key| self.conversations.contains(&key))
    }

    pub(super) fn contains_trigger(&self, trigger: &Trigger, state: &EngineState) -> bool {
        match &trigger.reply_channel {
            Some(address) => conversation(address, state).is_some_and(|key| {
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
