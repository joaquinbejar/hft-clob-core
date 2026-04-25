//! Risk and operational controls.
//!
//! Pre-trade checks gating every inbound client command: kill switch,
//! price band, per-account open-orders cap, per-account aggregate
//! notional cap. Self-trade prevention lives in `crates/matching/`
//! since it fires inside the fill loop after the cross is selected;
//! see `doc/DESIGN.md` § 5.2.
//!
//! Check order matches `doc/DESIGN.md` § 7:
//!
//! 1. Kill switch.
//! 2. Message validation — domain newtypes guarantee tick / lot / range
//!    invariants at the wire-decode boundary, so any rejection here is
//!    `MalformedMessage` from `crates/wire/`. Risk does not re-check.
//! 3. Price band — `|price - last_trade_price| <= last * PRICE_BAND_BPS / 10_000`.
//! 4. Per-account limits — open-orders count + aggregate notional.
//!
//! No `tokio`, no `tracing`, no wall-clock, no randomness, no env reads.
//! `HashMap<AccountId, _>` is point-lookup only; iteration never reaches
//! an outbound message (CLAUDE.md "no unordered iteration into outputs").

#![warn(missing_docs)]

/// Pre-trade risk state and check methods.
pub mod state;

pub use state::{AccountState, RiskState, notional_of};
