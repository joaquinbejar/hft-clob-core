//! Core domain types and invariants.
//!
//! Pure types and value objects for the matching engine. No I/O, no async,
//! no wall-clock. This crate depends on nothing and is depended on by all
//! other crates in the workspace.
//!
//! Invariants:
//! - All prices and quantities are represented as fixed-point scaled `i64`.
//!   No `f32` or `f64` anywhere.
//! - Named constants for tick size and lot size; no magic numbers.
//! - Domain newtypes (`Price`, `Qty`, `OrderId`, `AccountId`, `EngineSeq`,
//!   `ClientTs`, `RecvTs`) enforce type safety at boundaries.
