//! Wall-clock and stub `Clock` implementations.
//!
//! `crates/matching/` and `crates/risk/` both ban any direct
//! `SystemTime` / `Instant::now` read (CLAUDE.md core invariants).
//! The engine pipeline is the single layer allowed to read the wall
//! clock; it stamps `recv_ts` on every inbound command and `emit_ts`
//! on every outbound message via this trait.
//!
//! [`WallClock`] is the production source — `SystemTime::now()`
//! relative to the UNIX epoch. [`StubClock`] yields a script of
//! recorded values for byte-identical replay.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

use domain::{Clock, RecvTs};

/// Production wall-clock source.
///
/// Reads `SystemTime::now()` and converts to nanoseconds since the
/// UNIX epoch. The conversion is exact for any timestamp in the
/// representable range of `i64` nanoseconds (~1970 → ~2262).
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    #[inline]
    fn now(&self) -> RecvTs {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        RecvTs::new(nanos)
    }
}

/// Replay-time stub clock. Yields a fixed recorded value, advanced
/// explicitly via [`StubClock::set`]. Replay drives this alongside
/// the recorded inbound stream so each `recv_ts` reproduces the
/// original engine's view byte-for-byte.
#[derive(Debug)]
pub struct StubClock {
    current: Cell<i64>,
}

impl StubClock {
    /// Construct a stub clock at `nanos` since the UNIX epoch.
    #[must_use]
    pub fn new(nanos: i64) -> Self {
        Self {
            current: Cell::new(nanos),
        }
    }

    /// Override the current timestamp — replay calls this between
    /// inbound commands to step the clock forward.
    #[inline]
    pub fn set(&self, nanos: i64) {
        self.current.set(nanos);
    }
}

impl Clock for StubClock {
    #[inline]
    fn now(&self) -> RecvTs {
        RecvTs::new(self.current.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wall_clock_returns_positive_nanos() {
        let c = WallClock;
        assert!(c.now().as_nanos() > 0);
    }

    #[test]
    fn test_stub_clock_returns_set_value() {
        let c = StubClock::new(42);
        assert_eq!(c.now().as_nanos(), 42);
        c.set(123);
        assert_eq!(c.now().as_nanos(), 123);
    }
}
