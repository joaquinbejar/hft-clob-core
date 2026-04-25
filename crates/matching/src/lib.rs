#![warn(missing_docs)]

//! Single-writer matching core.
//!
//! The hot path of the system. Consumes orders that have passed risk validation,
//! matches them against the book using price-time priority, and emits
//! `OutEvent` to the outbound stream.
//!
//! Invariants enforced:
//! - No wall-clock reads (no `SystemTime`, `Instant::now`). Timestamps
//!   enter via injected `Clock` trait for deterministic replay.
//! - No randomness (no `rand::*`). ID generation via injected `IdGenerator`.
//! - No unordered iteration into outputs. Book index is `BTreeMap<Price, _>`
//!   sorted by price; iteration order is deterministic.
//! - `engine_seq` is strictly monotonic across all outbound events.
//!   Checked arithmetic on every increment.
//! - No tokio, no async. Pure blocking single-writer thread.

/// Book data structure and operations.
pub mod book;
/// Error types.
pub mod error;
/// Aggressive-order request and fill output types.
pub mod fill;

pub use book::{Book, RestingOrder};
pub use error::BookError;
pub use fill::{AggressiveOrder, Fill, MatchResult};
