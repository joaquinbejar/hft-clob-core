//! Engine — single-writer matching pipeline.
//!
//! Wires together gateway, matching, risk, and marketdata into a coherent
//! single-writer event loop. The engine thread is the sole mutator of the
//! order book; no other thread touches it.
//!
//! Also defines the `Clock` and `IdGenerator` traits for deterministic
//! replay: during normal operation, these are backed by `SystemTime` and
//! `ulid`; during replay, they delegate to monotonic counter stubs so that
//! outputs are byte-identical across runs.
//!
//! **No tokio here.** The engine is pure blocking and pinnable to a CPU core.
