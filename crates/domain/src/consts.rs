//! Workspace-wide named constants.
//!
//! Every constant that appears in domain validation, risk policy, or
//! matching logic lives here — `TICK_SIZE`, `LOT_SIZE`, `PRICE_BAND_BPS`,
//! account limits, and the venue symbol. Buffer sizes, frame headers,
//! and other purely-mechanical literals stay close to the code that
//! uses them; they are not policy and do not belong here.

/// Smallest legal price increment, expressed as a raw tick count.
///
/// `Price` values must be a positive multiple of this constant.
pub const TICK_SIZE: i64 = 1;

/// Smallest legal quantity increment, expressed as a raw lot count.
///
/// `Qty` values must be a positive multiple of this constant.
pub const LOT_SIZE: u64 = 1;

/// Price-band tolerance vs the reference price, in basis points.
///
/// `1 bp = 0.01 %`, so `PRICE_BAND_BPS = 500` means an inbound order is
/// rejected when its price differs from the reference by more than ±5 %.
pub const PRICE_BAND_BPS: u32 = 500;

/// Hard ceiling on the count of resting open orders per account.
pub const MAX_OPEN_ORDERS_PER_ACCT: u32 = 1_000;

/// Hard ceiling on aggregate notional exposure per account.
///
/// Stored as `u128` so a market order's worst-case far-touch fill price
/// cannot overflow during the pre-trade check.
pub const MAX_NOTIONAL_PER_ACCT: u128 = 10_000_000_000;

/// Single-symbol venue identifier. Hardcoded per CLAUDE.md scope; the
/// engine is single-symbol by design.
pub const SYMBOL: &str = "BTC-USDC";
