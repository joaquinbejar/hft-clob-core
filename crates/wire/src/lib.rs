//! Binary wire protocol — bespoke, fixed-size, little-endian.
//!
//! This crate is a pure codec. It does not import `tokio` or any
//! transport library; the gateway feeds it `&[u8]` slices and consumes
//! `Vec<u8>` writes. See `docs/protocol.md` for the on-wire layout
//! tables — the prose there is the single source of truth and any
//! encoder change must update both files in the same commit.
//!
//! Inbound message kinds (this crate, issue #4):
//!
//! - `NewOrder` (0x01)
//! - `CancelOrder` (0x02)
//! - `CancelReplace` (0x03)
//! - `MassCancel` (0x04)
//! - `KillSwitchSet` (0x05)
//! - `SnapshotRequest` (0x06)
//!
//! Outbound message kinds (issue #5) live behind `crate::outbound`.

#![warn(missing_docs)]

pub mod error;
pub mod framing;
pub mod inbound;

pub use error::WireError;
pub use framing::{FRAME_HEADER_BYTES, FRAME_KIND_BYTES, FRAME_LEN_BYTES, Frame, MessageKind};

/// Wire-protocol version. Bump on any layout change to inbound or
/// outbound message bodies.
pub const WIRE_VERSION: u16 = 1;
