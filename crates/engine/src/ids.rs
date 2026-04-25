//! `IdGenerator` implementations for trade ids and engine sequence numbers.
//!
//! Both prod and replay paths use a strictly monotonic counter — the
//! "uuid" naming in the original spec is informal; integer ids let us
//! keep wire layouts fixed-size and the replay golden file
//! byte-identical without uuid-generator state.
//!
//! [`CounterIdGenerator`] starts both counters from `1` and bumps via
//! checked arithmetic. Overflow surfaces as a panic on the next
//! allocation — CLAUDE.md forbids `wrapping_*` / `saturating_*` on
//! protocol counters; in practice neither counter can overflow inside
//! a session.

use domain::{EngineSeq, IdGenerator, TradeId};

/// Strictly monotonic counter for trade ids and engine sequence
/// numbers. `next_trade_id` and `next_engine_seq` increment
/// independently.
///
/// Construct via [`CounterIdGenerator::new`] for the engine's normal
/// start-of-day path or [`CounterIdGenerator::resume`] for the replay
/// harness, which seeds from the recorded snapshot.
#[derive(Debug, Clone, Copy)]
pub struct CounterIdGenerator {
    next_trade: u64,
    next_seq: u64,
}

impl CounterIdGenerator {
    /// Construct fresh counters starting at `1` for both streams.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_trade: 1,
            next_seq: 1,
        }
    }

    /// Resume from a recorded `(next_trade_id, next_engine_seq)` pair.
    /// Used by the replay harness so the byte-identical outbound
    /// stream picks up exactly where the recording left off.
    #[must_use]
    pub const fn resume(next_trade_id: u64, next_engine_seq: u64) -> Self {
        Self {
            next_trade: next_trade_id,
            next_seq: next_engine_seq,
        }
    }
}

impl Default for CounterIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator for CounterIdGenerator {
    #[inline]
    fn next_trade_id(&mut self) -> TradeId {
        let v = self.next_trade;
        self.next_trade = v.checked_add(1).expect("trade_id overflow");
        TradeId::new(v)
    }

    #[inline]
    fn next_engine_seq(&mut self) -> EngineSeq {
        let v = self.next_seq;
        self.next_seq = v.checked_add(1).expect("engine_seq overflow");
        EngineSeq::new(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_starts_at_one_for_both_streams() {
        let mut ids = CounterIdGenerator::new();
        assert_eq!(ids.next_trade_id().as_raw(), 1);
        assert_eq!(ids.next_engine_seq().as_raw(), 1);
    }

    #[test]
    fn test_counter_increments_strictly_monotonically() {
        let mut ids = CounterIdGenerator::new();
        let a = ids.next_engine_seq();
        let b = ids.next_engine_seq();
        let c = ids.next_engine_seq();
        assert!(a < b && b < c);
    }

    #[test]
    fn test_counter_streams_are_independent() {
        let mut ids = CounterIdGenerator::new();
        let _ = ids.next_engine_seq();
        let _ = ids.next_engine_seq();
        assert_eq!(ids.next_trade_id().as_raw(), 1);
    }

    #[test]
    fn test_resume_preserves_counters() {
        let mut ids = CounterIdGenerator::resume(50, 100);
        assert_eq!(ids.next_trade_id().as_raw(), 50);
        assert_eq!(ids.next_engine_seq().as_raw(), 100);
    }
}
