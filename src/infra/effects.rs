//! Durable `Effects` queue backoff calculation and `Outbox` delivery state transitions.

use crate::core::engine::Engine;
use crate::core::model::{Effect, OutboxStatus};

/// Maximum number of retry attempts before a failed side effect is dropped.
pub const MAX_EFFECT_ATTEMPTS: u32 = 5;

/// Computes exponential backoff in milliseconds (`min(1000 * 2^attempts, 60_000)`).
pub fn compute_backoff_ms(attempts: u32) -> u64 {
    let shift = attempts.min(6);
    (1_000_u64.saturating_mul(1 << shift)).min(60_000)
}

/// Pops the next ready `Effect` from `EngineState.effects` (FIFO).
pub fn pop_next_effect(engine: &mut Engine) -> Option<Effect> {
    if engine.state.effects.is_empty() {
        None
    } else {
        Some(engine.state.effects.remove(0))
    }
}

/// Records a failed `Effect` attempt; re-queues it if `attempts < MAX_EFFECT_ATTEMPTS`.
pub fn record_effect_failure(engine: &mut Engine, mut effect: Effect, err: impl Into<String>) {
    effect.attempts += 1;
    let err_str = err.into();
    effect.last_error = Some(err_str.clone());

    if effect.attempts < MAX_EFFECT_ATTEMPTS {
        engine.state.effects.push(effect);
    } else if let crate::core::model::EffectKind::SendOutbox { outbox_id } = &effect.kind
        && let Some(msg) = engine.state.outbox.get_mut(outbox_id) {
            msg.status = OutboxStatus::Dropped;
            msg.last_error = Some(err_str.clone());
            if let Some(tid) = msg.ticket_id.clone()
                && let Some(ticket) = engine.state.tickets.get_mut(&tid) {
                    ticket.delivery_error = Some(err_str);
                }
        }
}

/// Marks an `OutboxMessage` as successfully delivered (`Sent`).
pub fn mark_outbox_sent(engine: &mut Engine, outbox_id: &str) {
    if let Some(msg) = engine.state.outbox.get_mut(outbox_id) {
        msg.status = OutboxStatus::Sent;
        msg.last_error = None;
    }
}
