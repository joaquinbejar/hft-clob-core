//! Aggregated domain error.
//!
//! Every domain primitive owns its own `thiserror` enum (`PriceError`,
//! `QtyError`, …). [`DomainError`] re-emits them through `#[from]` so
//! upstream crates can carry a single error variant when the underlying
//! validation source is incidental.

use thiserror::Error;

use crate::types::{
    AccountIdError, CancelReasonError, EngineSeqError, ExecStateError, OrderIdError,
    OrderTypeError, PriceError, QtyError, RejectReasonError, SideError, TifError, TradeIdError,
};

/// Aggregator over every per-type validation / decode error in the
/// `domain` crate.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DomainError {
    /// `Price` validation.
    #[error(transparent)]
    Price(#[from] PriceError),
    /// `Qty` validation.
    #[error(transparent)]
    Qty(#[from] QtyError),
    /// `OrderId` validation.
    #[error(transparent)]
    OrderId(#[from] OrderIdError),
    /// `AccountId` validation.
    #[error(transparent)]
    AccountId(#[from] AccountIdError),
    /// `EngineSeq` arithmetic overflow.
    #[error(transparent)]
    EngineSeq(#[from] EngineSeqError),
    /// `TradeId` arithmetic overflow.
    #[error(transparent)]
    TradeId(#[from] TradeIdError),
    /// `Side` decode.
    #[error(transparent)]
    Side(#[from] SideError),
    /// `OrderType` decode.
    #[error(transparent)]
    OrderType(#[from] OrderTypeError),
    /// `Tif` decode.
    #[error(transparent)]
    Tif(#[from] TifError),
    /// `RejectReason` decode.
    #[error(transparent)]
    RejectReason(#[from] RejectReasonError),
    /// `CancelReason` decode.
    #[error(transparent)]
    CancelReason(#[from] CancelReasonError),
    /// `ExecState` decode.
    #[error(transparent)]
    ExecState(#[from] ExecStateError),
}
