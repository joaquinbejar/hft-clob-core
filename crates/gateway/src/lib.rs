//! Gateway — wire protocol parsing and session management.
//!
//! Listens on TCP for inbound order-entry messages, decodes them using
//! the binary wire protocol, and forwards `Command` to the matching engine
//! via SPSC ring buffer. Also handles graceful shutdown and session bookkeeping.
//!
//! **Only crate in the workspace that uses tokio and async.** All other
//! crates remain pure blocking and deterministic.
