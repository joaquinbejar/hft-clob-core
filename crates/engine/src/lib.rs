//! Engine — single-writer matching pipeline.
//!
//! Wires gateway-decoded inbound commands through risk, then matching,
//! then the outbound sink. The engine thread is the sole mutator of
//! the order book and risk state; no other thread touches them.
//!
//! Concrete `Clock` and `IdGenerator` impls ([`clock::WallClock`],
//! [`clock::StubClock`], [`ids::CounterIdGenerator`]) live here so
//! the matching core stays free of wall-clock and randomness reads
//! per CLAUDE.md core invariants. Replay swaps in [`StubClock`] +
//! a fresh [`CounterIdGenerator`] for byte-identical outbound.
//!
//! **No tokio here.** The engine is pure blocking and pinnable to a
//! CPU core.

#![warn(missing_docs)]

/// `Clock` implementations — production [`WallClock`] and replay
/// [`StubClock`].
pub mod clock;
/// Engine pipeline — wires inbound commands through risk + matching +
/// emission.
pub mod engine;
/// `IdGenerator` implementations — monotonic counters for trade ids
/// and engine sequence numbers.
pub mod ids;
/// `OutboundSink` trait + in-memory `VecSink` for tests / replay.
pub mod sink;

pub use clock::{StubClock, WallClock};
pub use engine::Engine;
pub use ids::CounterIdGenerator;
pub use sink::{OutboundSink, VecSink};
