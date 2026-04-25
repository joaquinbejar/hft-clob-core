//! Aggressive-order request / fill output / match summary types.

use domain::{AccountId, OrderId, Price, Qty, Side, Tif, TradeId};

/// Request handed to [`crate::Book::match_aggressive`].
///
/// `side` is the **taker** side. The fill loop walks the opposite side
/// of the book (bids walk asks ascending, asks walk bids descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggressiveOrder {
    /// Client-assigned identifier for the aggressor.
    pub order_id: OrderId,
    /// Aggressor's account binding. Used by self-trade prevention to
    /// detect a same-account maker / taker pairing inside the fill
    /// loop (CLAUDE.md § Architecture: STP fires after risk checks
    /// and before the actual cross).
    pub account_id: AccountId,
    /// Taker side.
    pub side: Side,
    /// Limit price cap. `None` means market order — always crosses.
    pub price: Option<Price>,
    /// Order quantity.
    pub qty: Qty,
    /// Time-in-force policy. The fill loop itself does not consume
    /// `tif` — the engine pipeline reads it after `match_aggressive`
    /// returns to decide whether the leftover qty rests (`Gtc`) or
    /// cancels (`Ioc`). `PostOnly` would-cross detection lives in #10.
    pub tif: Tif,
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

/// One self-trade-prevention cancellation. Emitted by the fill loop
/// when the next maker on the queue belongs to the taker's own
/// account; both sides are dropped per the cancel-both policy
/// (`doc/DESIGN.md` § 5.2). The engine pipeline emits a
/// `Cancelled{SelfTradePrevented}` exec report for the maker carrying
/// this record, plus a separate cancel for the taker (signalled by
/// [`MatchResult::taker_stp_cancelled`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StpCancellation {
    /// Resting maker that was dropped.
    pub order_id: OrderId,
    /// Account that owns the dropped maker (always equal to the
    /// taker's account, by definition of STP).
    pub account_id: AccountId,
    /// Maker side (taker is on the opposite side).
    pub side: Side,
    /// Maker's resting price.
    pub price: Price,
    /// Maker's residual qty at the time of cancellation.
    pub qty: Qty,
}

/// One mass-cancel emission. The fill loop's mass-cancel path
/// produces one record per resting order it dropped for the target
/// account; the engine pipeline emits a `Cancelled{MassCancel}`
/// exec report carrying each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MassCancellation {
    /// Resting order id that was dropped.
    pub order_id: OrderId,
    /// Account that owns the order (always the mass-cancel target).
    pub account_id: AccountId,
    /// Resting side.
    pub side: Side,
    /// Resting price.
    pub price: Price,
    /// Residual qty at the time of cancellation.
    pub qty: Qty,
}

/// Outcome of [`crate::Book::replace`]. Kept lightweight so the engine
/// can fold it into the right exec-report shape (`Replaced{kept_priority}`)
/// without re-deriving facts the matching core already knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplaceOutcome {
    /// `true` when the order kept its FIFO position (qty-down,
    /// in-place via `pricelevel::UpdateQuantity`). `false` when the
    /// order was cancel-and-re-added at the new (price, qty) — any
    /// price change or qty-up loses priority.
    pub kept_priority: bool,
}

/// Summary of one [`crate::Book::match_aggressive`] call.
///
/// `fills_count` is the number of [`Fill`] entries the call **wrote
/// into the caller's `out_buf`**, starting at the buffer's existing
/// length. `stp_cancellations` is the count of [`StpCancellation`]
/// entries appended to the caller's `out_stp` buffer. `taker_remaining`
/// is the leftover qty after the walk: `None` means the taker was
/// fully consumed; `Some(qty)` carries the leftover. `taker_stp_cancelled`
/// is `true` when the walk halted on a self-trade — the engine emits a
/// `Cancelled{SelfTradePrevented}` for the taker rather than resting it,
/// regardless of TIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchResult {
    /// Number of fills appended to the caller's `out_fills`.
    pub fills_count: usize,
    /// Number of STP cancellations appended to the caller's `out_stp`.
    pub stp_cancellations: usize,
    /// Taker's remaining qty after the walk. `None` = fully consumed;
    /// `Some(qty)` where qty > 0 for partial consumption.
    pub taker_remaining: Option<Qty>,
    /// `true` when the walk halted because the next maker belonged
    /// to the taker's account — the taker is cancelled with
    /// `SelfTradePrevented` regardless of TIF, taking precedence
    /// over `Gtc` / `Ioc` resting policy.
    pub taker_stp_cancelled: bool,
    /// `true` when the taker's TIF was `PostOnly` and the order
    /// would have crossed at arrival. The walk performs zero fills,
    /// zero STP cancels, and zero level mutations — the engine
    /// pipeline emits `Rejected{PostOnlyWouldCross}` for the taker
    /// and the book is unchanged.
    pub taker_post_only_rejected: bool,
}
