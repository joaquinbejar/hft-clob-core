//! Outbound stream emission — market data and execution reports.
//!
//! Encodes [`wire::outbound::Outbound`] events from the engine into
//! framed bytes ready for TCP / file emission. Sync — runs on the
//! engine thread or a dedicated drainer; tokio fan-out lives in
//! `crates/gateway/src/sink.rs`.

#![warn(missing_docs)]

/// Outbound encoder + decoder (round-trip-safe).
pub mod encoder;
/// `OutboundSink` trait — abstract publishing interface used by the
/// engine and tests / replay alike.
pub mod sink;

pub use encoder::{FRAME_HEADER_BYTES, decode, encode};
pub use sink::{OutboundSink, VecSink};
