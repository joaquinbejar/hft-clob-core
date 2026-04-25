//! Outbound (engine → client / market-data) message types.
//!
//! Each fixed-size submodule exposes a
//! `parse(payload: &[u8]) -> Result<X, WireError>` and
//! `encode(msg: &X, out: &mut Vec<u8>)` pair plus the
//! `XxxWire` `#[repr(C, packed)]` layout struct.
//!
//! [`snapshot_response`] is the only outbound message with a
//! variable-length payload; its layout is documented in that
//! module. The framing prefix is identical for every kind.
//!
//! [`Outbound`] is the dispatched union the marketdata sink consumes.

pub mod book_update_l2_delta;
pub mod book_update_top;
pub mod exec_report;
pub mod snapshot_response;
pub mod trade_print;

pub use book_update_l2_delta::BookUpdateL2Delta;
pub use book_update_top::BookUpdateTop;
pub use exec_report::ExecReport;
pub use snapshot_response::SnapshotResponse;
pub use trade_print::TradePrint;

use crate::WireError;
use crate::framing::{Frame, MessageKind};

/// Tagged union of every outbound message after wire decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    /// `ExecReport` — order lifecycle transition.
    ExecReport(ExecReport),
    /// `TradePrint` — public trade emission.
    TradePrint(TradePrint),
    /// `BookUpdateTop` — top-of-book on every book change.
    BookUpdateTop(BookUpdateTop),
    /// `BookUpdateL2Delta` — per-level depth delta.
    BookUpdateL2Delta(BookUpdateL2Delta),
    /// `SnapshotResponse` — variable-length book + engine state dump.
    SnapshotResponse(SnapshotResponse),
}

/// Parse an outbound frame into its typed variant.
///
/// # Errors
/// Propagates any [`WireError`] from the per-message decoder. Returns
/// [`WireError::UnknownKind`] when the frame carries an inbound kind
/// (those discriminants are valid in [`MessageKind`] but not legal on
/// the outbound stream).
#[inline]
pub fn parse_frame(frame: Frame<'_>) -> Result<Outbound, WireError> {
    match frame.kind {
        MessageKind::ExecReport => exec_report::parse(frame.payload).map(Outbound::ExecReport),
        MessageKind::TradePrint => trade_print::parse(frame.payload).map(Outbound::TradePrint),
        MessageKind::BookUpdateTop => {
            book_update_top::parse(frame.payload).map(Outbound::BookUpdateTop)
        }
        MessageKind::BookUpdateL2Delta => {
            book_update_l2_delta::parse(frame.payload).map(Outbound::BookUpdateL2Delta)
        }
        MessageKind::SnapshotResponse => {
            snapshot_response::parse(frame.payload).map(Outbound::SnapshotResponse)
        }
        inbound @ (MessageKind::NewOrder
        | MessageKind::CancelOrder
        | MessageKind::CancelReplace
        | MessageKind::MassCancel
        | MessageKind::KillSwitchSet
        | MessageKind::SnapshotRequest) => Err(WireError::UnknownKind(inbound.as_u8())),
    }
}
