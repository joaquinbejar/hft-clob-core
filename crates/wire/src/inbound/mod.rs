//! Inbound (client → engine) message types.
//!
//! Each submodule exposes a `parse(payload: &[u8]) -> Result<X, WireError>`
//! and `encode(msg: &X, out: &mut Vec<u8>)` pair plus the
//! `XxxWire` `#[repr(C, packed)]` layout struct.
//!
//! [`Inbound`] is the dispatched union the gateway hands to the engine.

pub mod cancel_order;
pub mod cancel_replace;
pub mod kill_switch;
pub mod mass_cancel;
pub mod new_order;
pub mod snapshot_request;

pub use cancel_order::CancelOrder;
pub use cancel_replace::CancelReplace;
pub use kill_switch::{KillSwitchSet, KillSwitchState};
pub use mass_cancel::MassCancel;
pub use new_order::NewOrder;
pub use snapshot_request::SnapshotRequest;

use crate::WireError;
use crate::framing::{Frame, MessageKind};

/// Tagged union of every inbound message after wire decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    /// `NewOrder` — limit or market order entry.
    NewOrder(NewOrder),
    /// `CancelOrder` — explicit cancel of a resting order.
    CancelOrder(CancelOrder),
    /// `CancelReplace` — modify a resting order in place.
    CancelReplace(CancelReplace),
    /// `MassCancel` — cancel every resting order on an account.
    MassCancel(MassCancel),
    /// `KillSwitchSet` — admin halt / resume of the engine.
    KillSwitchSet(KillSwitchSet),
    /// `SnapshotRequest` — operator dump of book + engine state.
    SnapshotRequest(SnapshotRequest),
}

/// Parse an inbound frame into its typed variant.
///
/// # Errors
/// Propagates any [`WireError`] from the per-message decoder. Returns
/// [`WireError::UnknownKind`] when the frame carries an outbound kind
/// (those discriminants are valid in [`MessageKind`] but not legal on
/// the inbound stream).
#[inline]
pub fn parse_frame(frame: Frame<'_>) -> Result<Inbound, WireError> {
    match frame.kind {
        MessageKind::NewOrder => new_order::parse(frame.payload).map(Inbound::NewOrder),
        MessageKind::CancelOrder => cancel_order::parse(frame.payload).map(Inbound::CancelOrder),
        MessageKind::CancelReplace => {
            cancel_replace::parse(frame.payload).map(Inbound::CancelReplace)
        }
        MessageKind::MassCancel => mass_cancel::parse(frame.payload).map(Inbound::MassCancel),
        MessageKind::KillSwitchSet => kill_switch::parse(frame.payload).map(Inbound::KillSwitchSet),
        MessageKind::SnapshotRequest => {
            snapshot_request::parse(frame.payload).map(Inbound::SnapshotRequest)
        }
        outbound @ (MessageKind::ExecReport
        | MessageKind::TradePrint
        | MessageKind::BookUpdateTop
        | MessageKind::BookUpdateL2Delta
        | MessageKind::SnapshotResponse) => Err(WireError::UnknownKind(outbound.as_u8())),
    }
}
