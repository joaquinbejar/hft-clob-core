//! Core domain types and invariants.
//!
//! Pure types and value objects for the matching engine. No I/O, no
//! async, no wall-clock. This crate depends on nothing and is depended
//! on by every other crate in the workspace.
//!
//! Invariants:
//! - All prices and quantities are represented as fixed-point scaled
//!   integers (`i64` for `Price`, `u64` for `Qty`). No `f32` / `f64`
//!   anywhere in this crate.
//! - Named constants for tick size, lot size, price band, and account
//!   limits — see [`consts`]. No magic numbers anywhere else in the
//!   workspace.
//! - Domain newtypes (`Price`, `Qty`, `OrderId`, `AccountId`,
//!   `EngineSeq`, `TradeId`, `ClientTs`, `RecvTs`) enforce type safety
//!   at every boundary.
//! - Wire-stable enums (`Side`, `OrderType`, `Tif`, `RejectReason`,
//!   `CancelReason`) carry explicit numeric discriminants that never
//!   change across schema versions.

#![warn(missing_docs)]

pub mod consts;
pub mod error;
pub mod types;

pub use error::DomainError;
pub use types::{
    AccountId, AccountIdError, CancelReason, CancelReasonError, ClientTs, EngineSeq,
    EngineSeqError, OrderId, OrderIdError, OrderType, OrderTypeError, Price, PriceError, Qty,
    QtyError, RecvTs, RejectReason, RejectReasonError, Side, SideError, Tif, TifError, TradeId,
    TradeIdError,
};
