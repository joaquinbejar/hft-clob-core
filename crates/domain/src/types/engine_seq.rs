//! Strictly monotonic engine sequence number.

use std::fmt;

/// Engine-assigned sequence number, strictly monotonic across every
/// outbound stream (exec reports + trade prints + book updates).
///
/// # Invariants
/// - Inner value increments by exactly one per outbound message via
///   [`EngineSeq::next`]. Skips and gaps indicate a bug.
/// - Construction via [`EngineSeq::INITIAL`] is the engine's normal
///   start-of-day path; [`EngineSeq::new`] is provided so the replay
///   harness can seed from a recorded value.
/// - Arithmetic is checked — overflow returns
///   [`EngineSeqError::Overflow`] rather than panicking or wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct EngineSeq(u64);

impl EngineSeq {
    /// Initial value. The engine starts every run from here.
    pub const INITIAL: Self = Self(0);

    /// Construct from a raw `u64`. Used by the replay harness to seed
    /// the counter from a recorded value.
    #[inline]
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// Advance to the next sequence number.
    ///
    /// # Errors
    /// Returns [`EngineSeqError::Overflow`] when the counter would
    /// exceed [`u64::MAX`]. CLAUDE.md forbids `saturating_*` /
    /// `wrapping_*` on protocol counters; this is the correct path.
    #[inline]
    pub const fn next(self) -> Result<Self, EngineSeqError> {
        match self.0.checked_add(1) {
            Some(v) => Ok(Self(v)),
            None => Err(EngineSeqError::Overflow),
        }
    }

    /// Raw counter value. Use only at the wire / serialization edge.
    #[inline(always)]
    #[must_use]
    pub const fn as_raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EngineSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "seq:{}", self.0)
    }
}

/// Error type for [`EngineSeq::next`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EngineSeqError {
    /// Counter would exceed [`u64::MAX`].
    #[error("engine_seq overflow")]
    Overflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for EngineSeq {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            (0u64..=u64::MAX).prop_map(EngineSeq::new).boxed()
        }
    }

    #[test]
    fn test_engine_seq_initial_is_zero() {
        assert_eq!(EngineSeq::INITIAL.as_raw(), 0);
    }

    #[test]
    fn test_engine_seq_next_increments_by_one() {
        let s = EngineSeq::new(41).next().expect("no overflow");
        assert_eq!(s.as_raw(), 42);
    }

    #[test]
    fn test_engine_seq_next_overflow_returns_err() {
        assert_eq!(
            EngineSeq::new(u64::MAX).next(),
            Err(EngineSeqError::Overflow)
        );
    }

    proptest! {
        #[test]
        fn proptest_engine_seq_strictly_monotonic(seed in 0u64..=u64::MAX - 1) {
            let s0 = EngineSeq::new(seed);
            let s1 = s0.next().expect("no overflow");
            prop_assert!(s1 > s0);
            prop_assert_eq!(s1.as_raw(), seed + 1);
        }
    }
}
