//! Account identifier.

use std::fmt;

/// Per-session account identifier.
///
/// # Invariants
/// - Inner value is non-zero. `0` is reserved as a sentinel for
///   "anonymous" / "no account" and never legal as a payload.
/// - The engine binds an `AccountId` to a session at connection
///   establishment; this type does not enforce that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AccountId(u32);

impl AccountId {
    /// Construct from a raw `u32`.
    ///
    /// # Errors
    /// Returns [`AccountIdError::Zero`] when `id == 0`.
    #[inline]
    pub const fn new(id: u32) -> Result<Self, AccountIdError> {
        if id == 0 {
            return Err(AccountIdError::Zero);
        }
        Ok(Self(id))
    }

    /// Raw identifier. Use only at the wire / serialization edge.
    #[inline(always)]
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for AccountId {
    type Error = AccountIdError;
    #[inline]
    fn try_from(v: u32) -> Result<Self, Self::Error> {
        Self::new(v)
    }
}

impl fmt::Display for AccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "acct:{}", self.0)
    }
}

/// Validation error for [`AccountId::new`].
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountIdError {
    /// `id == 0` (reserved sentinel).
    #[error("account_id must be non-zero")]
    Zero,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    impl Arbitrary for AccountId {
        type Parameters = ();
        type Strategy = BoxedStrategy<Self>;
        fn arbitrary_with(_: ()) -> Self::Strategy {
            (1u32..=u32::MAX)
                .prop_map(|v| AccountId::new(v).expect("strategy yields valid AccountId"))
                .boxed()
        }
    }

    #[test]
    fn test_account_id_new_zero_returns_err() {
        assert_eq!(AccountId::new(0), Err(AccountIdError::Zero));
    }

    #[test]
    fn test_account_id_new_one_returns_ok() {
        assert!(AccountId::new(1).is_ok());
    }

    proptest! {
        #[test]
        fn proptest_account_id_roundtrip(id in any::<AccountId>()) {
            prop_assert_eq!(AccountId::new(id.as_raw()).expect("roundtrip"), id);
        }
    }
}
