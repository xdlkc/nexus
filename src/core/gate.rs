//! Tier-0 deterministic Gate: allowlist checks, admission strength, and inline directive parsing.

use crate::core::model::{
    AcceptMode, AdmissionStrength, HarnessKind, PartialExecutionTarget, ThinkEffort,
};
use std::collections::HashSet;

/// Parses an inline execution directive from message text, e.g.:
/// - `@nexus [codex:openai-codex/gpt-5.6-sol:xhigh] debug the deadlock`
/// - `[claude-code:claude-opus-4-6:high] refactor module`
/// - `[pi-native:low] check my calendar`
pub fn parse_inline_target_directive(text: &str) -> (String, Option<PartialExecutionTarget>) {
    let trimmed = text.trim_start();
    let after_mention = if let Some(rest) = trimmed.strip_prefix('@') {
        match rest.find(char::is_whitespace) {
            Some(idx) => rest[idx..].trim_start(),
            None => return (text.to_owned(), None),
        }
    } else {
        trimmed
    };

    let Some(bracket_rest) = after_mention.strip_prefix('[') else {
        return (text.to_owned(), None);
    };
    let Some(close_idx) = bracket_rest.find(']') else {
        return (text.to_owned(), None);
    };

    let spec = &bracket_rest[..close_idx];
    if spec.is_empty() || spec.contains(char::is_whitespace) {
        return (text.to_owned(), None);
    }

    let mut target = PartialExecutionTarget::default();
    for raw_part in spec.split(':').take(3) {
        let part = raw_part.trim();
        if part.is_empty() {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        if let Some(harness) = HarnessKind::parse(&lower) {
            target.harness = Some(harness);
        } else if let Some(think) = ThinkEffort::parse(&lower) {
            target.think_effort = Some(think);
        } else if let Some((provider, model)) = part.split_once('/') {
            if !provider.is_empty() && !model.is_empty() {
                target.provider = Some(provider.to_owned());
                target.model = Some(model.to_owned());
            }
        } else {
            target.model = Some(part.to_owned());
        }
    }

    if target.is_empty() {
        return (text.to_owned(), None);
    }

    let remaining = bracket_rest[close_idx + 1..].trim();
    (remaining.to_owned(), Some(target))
}

/// Deterministic Tier-0 admission policy for one Source scope.
#[derive(Debug, Clone)]
pub struct SourceAdmissionPolicy {
    pub allowed_actors: HashSet<String>,
    pub allowed_bots: HashSet<String>,
    pub unlisted_addressed: AdmissionStrength,
    pub listed_unaddressed: AdmissionStrength,
    pub unlisted_unaddressed: AdmissionStrength,
}

/// Computes Tier-0 admission strength without invoking any model.
pub fn evaluate_gate_strength(
    actor_id: &str,
    actor_is_bot: bool,
    addressed_agent: bool,
    policy: &SourceAdmissionPolicy,
    accept_mode: AcceptMode,
) -> AdmissionStrength {
    if actor_is_bot {
        if !policy.allowed_bots.contains(actor_id) {
            return AdmissionStrength::NotAdmitted;
        }
        return match accept_mode {
            AcceptMode::Off => AdmissionStrength::NotAdmitted,
            AcceptMode::Observe | AcceptMode::Auto => AdmissionStrength::Inferred,
        };
    }

    let is_listed = policy.allowed_actors.contains(actor_id);
    let strength = match (is_listed, addressed_agent) {
        (true, true) => AdmissionStrength::Explicit,
        (true, false) => policy.listed_unaddressed,
        (false, true) => policy.unlisted_addressed,
        (false, false) => policy.unlisted_unaddressed,
    };

    if strength == AdmissionStrength::Inferred && accept_mode == AcceptMode::Off {
        return AdmissionStrength::NotAdmitted;
    }

    strength
}
