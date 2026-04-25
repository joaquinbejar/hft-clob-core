//! Gateway — wire protocol parsing and session management.
//!
//! Listens on TCP for inbound order-entry messages, decodes them using
//! the binary wire protocol, and forwards typed [`wire::inbound::Inbound`]
//! commands to the engine via a tokio mpsc channel.
//!
//! **Only crate in the workspace that uses tokio and async.** All other
//! crates remain pure blocking and deterministic.

#![warn(missing_docs)]

/// TCP listener and per-connection inbound decoder.
pub mod listener;
/// Append-only ingress recorder for deterministic replay.
pub mod recorder;
/// Outbound channel sink — engine emits, async writer drains.
pub mod sink;

pub use listener::{DEFAULT_READ_BUFFER, handle_connection, run};
pub use recorder::{RECV_TS_BYTES, Record, Recorder, read_records};
pub use sink::ChannelSink;
