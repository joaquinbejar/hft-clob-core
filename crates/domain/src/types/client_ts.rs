//! Client-stamped timestamp.

use std::fmt;

/// Client-stamped timestamp in nanoseconds since the UNIX epoch.
///
/// # Invariants
/// - The inner value is opaque to the engine; the matching core never
///   reads it for control flow. It is carried on inbound messages and
///   echoed in execution reports for client-side correlation.
/// - This type does not call the wall clock — construction is purely
///   from a value the caller already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ClientTs(i64);

impl ClientTs {
    /// Zero-valued sentinel for "unset". Useful in tests and as a
    /// neutral default in proptests; not a meaningful timestamp.
    pub const ZERO: Self = Self(0);

    /// Construct from a raw nanosecond count.
    #[inline]
    #[must_use]
    pub const fn new(nanos: i64) -> Self {
        Self(nanos)
    }

    /// Raw nanosecond count.
    #[inline(always)]
    #[must_use]
    pub const fn as_nanos(self) -> i64 {
        self.0
    }
}

impl From<i64> for ClientTs {
    #[inline]
    fn from(v: i64) -> Self {
        Self::new(v)
    }
}

impl fmt::Display for ClientTs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cts:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for ClientTs {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            any::<i64>().prop_map(ClientTs::new).boxed()
        }
    }

    #[test]
    fn test_client_ts_zero_is_zero() {
        assert_eq!(ClientTs::ZERO.as_nanos(), 0);
    }

    proptest! {
        #[test]
        fn proptest_client_ts_roundtrip(ts in any::<ClientTs>()) {
            prop_assert_eq!(ClientTs::new(ts.as_nanos()), ts);
        }
    }
}
