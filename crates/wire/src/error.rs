//! Wire-protocol error type.

use thiserror::Error;

use domain::DomainError;

/// Aggregated decode / framing error for the wire crate.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WireError {
    /// Buffer was shorter than the declared frame length, or shorter
    /// than the fixed-size payload of the indicated message kind.
    #[error("frame buffer truncated")]
    Truncated,
    /// Header contained a kind discriminant outside the assigned range.
    #[error("unknown message kind: {0:#04x}")]
    UnknownKind(u8),
    /// A reserved padding byte was non-zero, indicating a malformed or
    /// version-mismatched payload.
    #[error("non-zero padding byte at offset {0}")]
    NonZeroPad(usize),
    /// Payload size did not match the fixed-size layout of the indicated
    /// message kind.
    #[error("payload size mismatch: expected {expected}, got {got}")]
    PayloadSize {
        /// Expected fixed payload size in bytes.
        expected: usize,
        /// Observed payload size in bytes.
        got: usize,
    },
    /// A field failed `domain` validation (e.g. zero `OrderId`,
    /// non-positive `Price`, unknown `Side` discriminant).
    #[error(transparent)]
    Domain(#[from] DomainError),
}

/// Lift any per-type domain validation error into a [`WireError`] via
/// [`DomainError`]. Avoids spelling out two `.into()` hops per call site.
#[inline]
pub(crate) fn to_wire<E: Into<DomainError>>(e: E) -> WireError {
    WireError::Domain(e.into())
}
