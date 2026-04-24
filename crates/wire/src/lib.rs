//! Binary wire protocol — SBE-based schema, encoders, decoders.
//!
//! Defines the on-wire message format for order entry (inbound) and
//! market data / execution reports (outbound). Uses IronSBE for zero-copy
//! schema and code generation. All lengths are little-endian; framing is
//! length-prefixed TCP (first 4 bytes = message length in bytes).
//!
//! Roundtrip tests verify encode → decode preserves all fields byte-identically.
