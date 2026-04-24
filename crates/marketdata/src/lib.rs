//! Outbound stream emission — market data and execution reports.
//!
//! Consumes `OutEvent` from the matching core and encodes them into
//! on-wire messages: trade prints, top-of-book updates, and execution reports.
//! Emits to TCP or file via the outbound channel.
//!
//! Maintains `engine_seq` consistency and gap-detection hooks for packet loss.
//! L2 incremental snapshot + delta recovery is a stretch goal.
