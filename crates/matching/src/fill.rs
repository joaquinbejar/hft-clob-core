//! Aggressive-order request / fill output / match summary types.

use domain::{OrderId, Price, Qty, Side, TradeId};

/// Request handed to [`crate::Book::match_aggressive`].
///
/// `side` is the **taker** side. The fill loop walks the opposite side
/// of the book (bids walk asks ascending, asks walk bids descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggressiveOrder {
    /// Client-assigned identifier for the aggressor.
    pub order_id: OrderId,
    /// Taker side.
    pub side: Side,
    /// Limit price cap. `None` means market order — always crosses.
    pub price: Option<Price>,
    /// Order quantity.
    pub qty: Qty,
}

/// One fill event produced by the fill loop.
///
/// Each [`Fill`] corresponds to exactly one maker / taker pairing at a
/// single price (CLAUDE.md core invariants: every trade has one maker
/// and one taker; `maker.price == trade.price`). The trade id comes
/// from the injected `IdGenerator`, never from `pricelevel`'s internal
/// generator (which the engine discards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fill {
    /// Engine-internal trade id, allocated by the injected
    /// `IdGenerator`.
    pub trade_id: TradeId,
    /// Resting order on the book that filled.
    pub maker_order_id: OrderId,
    /// Aggressive incoming order.
    pub taker_order_id: OrderId,
    /// Maker side. The taker is on the opposite side.
    pub maker_side: Side,
    /// Trade price = maker's resting price (CLAUDE.md invariant
    /// `maker.price == trade.price`).
    pub price: Price,
    /// Filled quantity.
    pub qty: Qty,
    /// `true` when this fill fully consumed the maker. The maker has
    /// been removed from the book; the engine pipeline emits a
    /// terminal `Filled` exec report for it.
    ///
    /// `false` means the maker has a positive residual qty resting
    /// on the book; the exact residual lives inside `pricelevel`'s
    /// per-order atomic and is read by the engine pipeline (#12)
    /// when emitting the maker's `PartiallyFilled` exec report —
    /// not carried here to avoid duplicating state and to keep this
    /// type honest about what it actually knows.
    pub maker_fully_filled: bool,
}

/// Summary of one [`crate::Book::match_aggressive`] call.
///
/// `fills_count` is the number of [`Fill`] entries the call **wrote
/// into the caller's `out_buf`**, starting at the buffer's existing
/// length. `taker_remaining` is the leftover qty after the walk:
/// `0` means the taker was fully consumed (terminal `Filled` from the
/// engine's perspective); a non-zero value lets the engine apply TIF
/// semantics (rest as `Gtc`, cancel for `Ioc`, etc.) — that policy
/// lives in issue #8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchResult {
    /// Number of fills appended to the caller's `out_buf`.
    pub fills_count: usize,
    /// Taker's remaining qty after the walk, in lots. `0` =
    /// fully consumed.
    pub taker_remaining: u64,
}
