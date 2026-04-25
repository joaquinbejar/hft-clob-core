//! Outbound sink trait — the engine's only path to publish events.
//!
//! Every emission goes through this trait; the matching core never
//! emits directly. Sink implementations live downstream:
//! `crates/marketdata/` for the public stream, `crates/gateway/` for
//! per-session client streams, and the replay harness uses an
//! in-memory `VecSink` to capture the byte stream for diff against
//! the golden file.

use wire::outbound::Outbound;

/// Sink for engine-emitted [`Outbound`] events.
///
/// Implementations are expected to be infallible from the engine's
/// perspective — backpressure / I/O failure is handled by the sink
/// (drop, retry, log) so the engine's `step` loop never blocks. The
/// engine is single-writer per CLAUDE.md and cannot observe failures
/// without breaking `engine_seq` monotonicity.
pub trait OutboundSink {
    /// Publish one outbound event.
    fn emit(&mut self, msg: Outbound);
}

/// In-memory sink backed by a `Vec<Outbound>`. The intended consumer
/// is the unit-test harness and the replay diff. Production paths
/// install the marketdata SPSC channel sink instead.
#[derive(Debug, Default)]
pub struct VecSink {
    /// Captured events, in emission order.
    pub events: Vec<Outbound>,
}

impl VecSink {
    /// Construct an empty `VecSink`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain the captured events.
    pub fn take(&mut self) -> Vec<Outbound> {
        std::mem::take(&mut self.events)
    }

    /// Number of events captured so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// `true` when no events have been captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl OutboundSink for VecSink {
    #[inline]
    fn emit(&mut self, msg: Outbound) {
        self.events.push(msg);
    }
}
