//! Engine-injected dependency traits.
//!
//! The matching core (`crates/matching/`) and the engine pipeline
//! (`crates/engine/`) consume these traits rather than calling the
//! wall clock or a random source directly. Production wraps
//! `SystemTime::now` and a counter / UUID generator; the replay
//! harness swaps in deterministic stubs seeded from the recorded
//! input stream.
//!
//! Concrete prod / stub impls live in `crates/engine/src/`. Defining
//! the traits in `domain` keeps `matching` from having to depend on
//! `engine` (which would invert the dependency edge).

use crate::types::{EngineSeq, RecvTs, TradeId};

/// Engine-stamped time source.
///
/// CLAUDE.md bans `SystemTime` / `Instant::now` / `chrono::Utc::now`
/// inside `crates/matching/` and `crates/risk/`. The matching core
/// reads timestamps via this trait so the replay harness can swap in
/// a `StubClock` that yields recorded values, producing a
/// byte-identical outbound stream across runs.
pub trait Clock {
    /// Engine-stamped receive / emit timestamp in nanoseconds since
    /// the documented epoch.
    fn now(&self) -> RecvTs;
}

/// Engine-internal id source for trade identifiers and outbound
/// sequence numbers.
///
/// `next_trade_id` is called by the matching core's fill loop, once
/// per emitted trade. `next_engine_seq` is called by the engine for
/// every outbound message (exec reports, trade prints, book
/// updates). Both must increase strictly monotonically.
///
/// `EngineSeq::next` already enforces strict monotonicity with
/// checked arithmetic; this trait is the engine's hook to swap a
/// `CounterIdGenerator` (replay) for the production generator
/// without touching the matching core.
pub trait IdGenerator {
    /// Allocate the next trade id.
    fn next_trade_id(&mut self) -> TradeId;

    /// Allocate the next engine sequence number. The returned value
    /// is the new (post-increment) value, so two consecutive calls
    /// always yield strictly increasing `EngineSeq`s.
    fn next_engine_seq(&mut self) -> EngineSeq;
}
