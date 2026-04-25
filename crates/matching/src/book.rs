//! Single-symbol order book.
//!
//! Stores resting orders in a sorted price index per side
//! (`BTreeMap<Price, Arc<pricelevel::PriceLevel>>`) plus a sidecar
//! `HashMap<OrderId, (Side, Price)>` for O(log n) cancel. The
//! `pricelevel` per-level container handles FIFO + atomic qty
//! tracking; iteration order on the sidecar is never observable from
//! this crate's outputs (CLAUDE.md § Architecture).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use domain::{AccountId, IdGenerator, OrderId, Price, Qty, Side};
use pricelevel::{
    Hash32, Id as PlId, OrderType as PlOrderType, OrderUpdate as PlOrderUpdate, Price as PlPrice,
    PriceLevel, Quantity as PlQuantity, Side as PlSide, TimeInForce as PlTif, TimestampMs,
};

use crate::error::BookError;
use crate::fill::{AggressiveOrder, Fill, MatchResult, StpCancellation};

/// Sidecar entry per resting order: every field the cancel /
/// match-walk paths read in O(1) without iterating the price index.
#[derive(Debug, Clone, Copy)]
struct OrderMeta {
    side: Side,
    price: Price,
    account_id: AccountId,
}

/// A resting-order request handed to [`Book::add_resting`].
///
/// Carries the `account_id` so self-trade prevention can detect a
/// same-account maker / taker pairing inside the fill loop without
/// having to bounce through a separate lookup table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingOrder {
    /// Client-assigned identifier.
    pub order_id: OrderId,
    /// Account binding.
    pub account_id: AccountId,
    /// Buy / sell.
    pub side: Side,
    /// Limit price.
    pub price: Price,
    /// Order quantity.
    pub qty: Qty,
}

/// Single-symbol order book.
///
/// Single-writer per the engine's pipeline; the public surface takes
/// `&mut self`. Internally each `pricelevel::PriceLevel` exposes
/// interior mutability via atomics and a `DashMap`, so an `Arc<_>` is
/// safe to share with read-only consumers (top-of-book emitter, L2
/// snapshot generator) once those crates land.
pub struct Book {
    bids: BTreeMap<Price, Arc<PriceLevel>>,
    asks: BTreeMap<Price, Arc<PriceLevel>>,
    /// Lookup-only — never iterated into outputs. Maps every resting
    /// `OrderId` to its `OrderMeta` (side / price / account_id) so
    /// cancel is O(log n) on the price index plus O(1) inside the
    /// level, and STP detection inside the fill loop is also O(1).
    index: HashMap<OrderId, OrderMeta>,
    /// Strictly increasing per-order arrival counter, used as the
    /// `pricelevel::TimestampMs` for each inserted order. Internal
    /// monotonic — does NOT read the wall clock. CLAUDE.md forbids
    /// any wall-clock read inside `crates/matching/`.
    seq: u64,
}

impl Book {
    /// Construct an empty book.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            index: HashMap::new(),
            seq: 0,
        }
    }

    /// Insert a resting order at its price level. Idempotent under
    /// `add_resting → cancel`; the book is restored to its prior
    /// state when every added order is cancelled.
    ///
    /// # Errors
    /// - [`BookError::DuplicateOrderId`] if `order.order_id` is already
    ///   resting.
    /// - [`BookError::SeqOverflow`] if the internal arrival counter
    ///   would exceed [`u64::MAX`] (unreachable in realistic operation;
    ///   reported rather than wrapped per CLAUDE.md).
    #[inline]
    pub fn add_resting(&mut self, order: RestingOrder) -> Result<(), BookError> {
        if self.index.contains_key(&order.order_id) {
            return Err(BookError::DuplicateOrderId);
        }
        // Bump the arrival counter first so `TimestampMs` reflects the
        // post-add value rather than lagging by one. CLAUDE.md forbids
        // `wrapping_*` / `saturating_*` on protocol counters; surface
        // overflow as `SeqOverflow`.
        self.seq = self.seq.checked_add(1).ok_or(BookError::SeqOverflow)?;

        let levels = match order.side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let price_u128 = order.price.as_ticks() as u128;

        let pl_order = PlOrderType::Standard {
            id: PlId::Sequential(order.order_id.as_raw()),
            price: PlPrice::new(price_u128),
            quantity: PlQuantity::new(order.qty.as_lots()),
            side: domain_to_pl_side(order.side),
            user_id: Hash32::zero(),
            // Internal monotonic counter, NOT a wall-clock read.
            // CLAUDE.md bans `SystemTime` / `Instant::now` inside
            // `crates/matching/`. Pricelevel's queue is FIFO by enqueue
            // order, so this value is only used by `pricelevel` for
            // debug snapshots; the engine never reads it back.
            timestamp: TimestampMs::new(self.seq),
            time_in_force: PlTif::Gtc,
            extra_fields: (),
        };
        // Insert (or create) the level and enqueue the order in one
        // pass. `add_order` takes `&self`, so the `&mut Arc<PriceLevel>`
        // returned by `or_insert_with` auto-derefs without an extra
        // `Arc::clone` on the steady-state path.
        levels
            .entry(order.price)
            .or_insert_with(|| Arc::new(PriceLevel::new(price_u128)))
            .add_order(pl_order);

        self.index.insert(
            order.order_id,
            OrderMeta {
                side: order.side,
                price: order.price,
                account_id: order.account_id,
            },
        );
        Ok(())
    }

    /// Cancel a resting order by id. Removes the level from the
    /// sorted index when its last order leaves.
    ///
    /// # Errors
    /// [`BookError::UnknownOrderId`] when `order_id` is not present in
    /// the index — either it never rested, was already cancelled, or
    /// was fully filled.
    #[inline]
    pub fn cancel(&mut self, order_id: OrderId) -> Result<(Side, Price), BookError> {
        // Look up first without mutating. If the level is missing the
        // book is already corrupt (sidecar / price index out of sync),
        // but at least the sidecar entry stays in place so a retry can
        // see the same view rather than a partially-rolled-back state.
        let meta = *self.index.get(&order_id).ok_or(BookError::UnknownOrderId)?;
        let side = meta.side;
        let price = meta.price;
        let levels = match side {
            Side::Bid => &mut self.bids,
            Side::Ask => &mut self.asks,
        };
        let level = levels.get(&price).ok_or(BookError::UnknownOrderId)?;
        // TODO(#12): `update_order` returns the cancelled order record
        // so the engine pipeline can emit a `Cancelled{reason}` exec
        // report carrying the `leaves_qty`. Allocator behaviour of the
        // discard path needs revisiting when that wiring lands — if
        // the inner `Option<Arc<…>>` boxes per call, swap to a
        // `remove_by_id`-shaped path.
        let _ = level.update_order(PlOrderUpdate::Cancel {
            order_id: PlId::Sequential(order_id.as_raw()),
        });
        let level_empty = level.order_count() == 0;

        // Pricelevel cancel is observable; only now is it safe to
        // mutate the sidecar and prune an empty level.
        self.index.remove(&order_id);
        if level_empty {
            levels.remove(&price);
        }
        Ok((side, price))
    }

    /// Walk the opposite side of the book best-price-first, consuming
    /// resting orders FIFO at each level until the taker is filled,
    /// the next level no longer crosses, no more orders remain, or
    /// self-trade prevention fires on a same-account maker.
    ///
    /// `taker.side == Side::Bid` walks the asks ascending (best ask
    /// first). `taker.side == Side::Ask` walks the bids descending.
    /// `taker.price == None` is a market order — always crosses.
    /// `taker.price == Some(p)` stops the walk at the first level
    /// that does not cross (`asks_price > p` for a buy taker,
    /// `bids_price < p` for a sell taker).
    ///
    /// **Self-trade prevention (cancel-both, per `doc/DESIGN.md` §
    /// 5.2).** Before crossing each maker the loop reads its
    /// `account_id` from the sidecar. When the maker belongs to the
    /// taker's account, both sides are dropped: the maker is
    /// cancelled (an [`StpCancellation`] record is appended to
    /// `out_stp` and the level / sidecar are updated), the walk
    /// halts, and `MatchResult::taker_stp_cancelled` is set so the
    /// engine pipeline emits `Cancelled{SelfTradePrevented}` for the
    /// taker rather than resting it. STP fires after risk checks
    /// (CLAUDE.md § 7) and before the actual cross — fills emitted
    /// during the walk up to the STP point are kept; the STP cuts
    /// further crossing.
    ///
    /// CLAUDE.md invariants honoured by construction:
    ///
    /// - Levels are picked via `BTreeMap::first_key_value` /
    ///   `last_key_value`; iteration is sorted by price, deterministic.
    /// - Strict FIFO within a level — `pricelevel::PriceLevel`'s
    ///   internal `crossbeam::SegQueue` is iterated head-first via
    ///   `iter_orders` and mutated through `update_order`.
    /// - Every emitted [`Fill`] has `price == maker_level_price`.
    /// - Every [`Fill`] is one maker (`maker_order_id`) and one taker
    ///   (`taker_order_id`) — structural in the type.
    /// - No wall-clock reads, no randomness, no hash-based iteration
    ///   reaching outputs.
    ///
    /// Returns a [`MatchResult`] summarising how many fills + STP
    /// cancellations were appended, the taker's remaining qty, and
    /// whether the walk halted on STP.
    #[inline]
    pub fn match_aggressive<I: IdGenerator>(
        &mut self,
        taker: AggressiveOrder,
        ids: &mut I,
        out_fills: &mut Vec<Fill>,
        out_stp: &mut Vec<StpCancellation>,
    ) -> MatchResult {
        let initial_fills_len = out_fills.len();
        let initial_stp_len = out_stp.len();
        let mut remaining = taker.qty.as_lots();
        let mut taker_stp_cancelled = false;

        'walk: while remaining > 0 {
            // Pick the best opposite-side level price.
            let level_price = match taker.side {
                Side::Bid => self.asks.keys().next().copied(),
                Side::Ask => self.bids.keys().next_back().copied(),
            };
            let level_price = match level_price {
                Some(p) => p,
                None => break,
            };

            // Limit-price gate. `None` is a market order and always crosses.
            if let Some(taker_price) = taker.price {
                let crosses = match taker.side {
                    Side::Bid => taker_price >= level_price,
                    Side::Ask => taker_price <= level_price,
                };
                if !crosses {
                    break;
                }
            }

            // Fetch the level (Arc::clone so the BTreeMap can be
            // mutably re-borrowed below for empty-level pruning).
            let level_arc = match taker.side {
                Side::Bid => self.asks.get(&level_price),
                Side::Ask => self.bids.get(&level_price),
            };
            let level_arc = match level_arc {
                Some(l) => Arc::clone(l),
                None => break,
            };

            // Walk this level's queue head-first. STP is checked per
            // head: the first same-account maker we hit cancels both
            // sides and halts the walk. Otherwise the head is filled
            // (partial or full) via `update_order` and the next head
            // becomes the new front.
            'level: loop {
                if remaining == 0 {
                    break 'level;
                }
                // Peek the queue head. `iter_orders` is FIFO under
                // single-writer (the matching core is single-writer
                // per CLAUDE.md), so the first item is the oldest
                // resting order at this price.
                let head = match level_arc.iter_orders().next() {
                    Some(h) => h,
                    None => break 'level,
                };
                let head_pl_id = head.id();
                let head_order_id = pl_id_to_domain(head_pl_id);
                let head_meta = match self.index.get(&head_order_id) {
                    Some(m) => *m,
                    None => {
                        // Sidecar / level out of sync — defensive halt.
                        break 'walk;
                    }
                };

                // STP gate. Cancel-both: drop the maker, halt the
                // walk, signal taker cancel.
                if head_meta.account_id == taker.account_id {
                    let head_qty_raw = head.visible_quantity();
                    let head_qty = Qty::new(head_qty_raw).unwrap_or(Qty::MIN);
                    debug_assert!(head_qty_raw > 0, "resting order with zero qty");
                    let _ = level_arc.update_order(PlOrderUpdate::Cancel {
                        order_id: head_pl_id,
                    });
                    self.index.remove(&head_order_id);
                    out_stp.push(StpCancellation {
                        order_id: head_order_id,
                        account_id: head_meta.account_id,
                        side: head_meta.side,
                        price: level_price,
                        qty: head_qty,
                    });
                    taker_stp_cancelled = true;
                    // The STP cancel may have emptied the level. Clean
                    // up the BTreeMap entry before halting so a follow-
                    // up `best_*` / `match_aggressive` sees the right
                    // view.
                    if level_arc.order_count() == 0 {
                        let _ = match taker.side {
                            Side::Bid => self.asks.remove(&level_price),
                            Side::Ask => self.bids.remove(&level_price),
                        };
                    }
                    break 'walk;
                }

                // Normal fill against this maker.
                let head_qty_raw = head.visible_quantity();
                if head_qty_raw == 0 {
                    debug_assert!(false, "pricelevel queue head has zero qty");
                    break 'walk;
                }
                let fill_qty_raw = remaining.min(head_qty_raw);
                let new_head_qty_raw = head_qty_raw - fill_qty_raw;
                let maker_fully_filled = new_head_qty_raw == 0;

                if maker_fully_filled {
                    let _ = level_arc.update_order(PlOrderUpdate::Cancel {
                        order_id: head_pl_id,
                    });
                    self.index.remove(&head_order_id);
                } else {
                    let _ = level_arc.update_order(PlOrderUpdate::UpdateQuantity {
                        order_id: head_pl_id,
                        new_quantity: PlQuantity::new(new_head_qty_raw),
                    });
                }

                let Ok(fill_qty) = Qty::new(fill_qty_raw) else {
                    debug_assert!(false, "fill_qty_raw==0 should be unreachable");
                    break 'walk;
                };
                let trade_id = ids.next_trade_id();
                out_fills.push(Fill {
                    trade_id,
                    maker_order_id: head_order_id,
                    taker_order_id: taker.order_id,
                    maker_side: taker.side.opposite(),
                    price: level_price,
                    qty: fill_qty,
                    maker_fully_filled,
                });
                remaining -= fill_qty_raw;
            }

            // If the level is now empty, remove it from the price
            // index so `best_*` and the next walk see the right view.
            if level_arc.order_count() == 0 {
                let _ = match taker.side {
                    Side::Bid => self.asks.remove(&level_price),
                    Side::Ask => self.bids.remove(&level_price),
                };
            }
        }

        MatchResult {
            fills_count: out_fills.len() - initial_fills_len,
            stp_cancellations: out_stp.len() - initial_stp_len,
            taker_remaining: Qty::new(remaining).ok(),
            taker_stp_cancelled,
        }
    }

    /// Best bid price, or `None` when the bid side is empty.
    /// O(log n) on the BTreeMap; never iterates the sidecar.
    #[must_use]
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next_back().copied()
    }

    /// Best ask price, or `None` when the ask side is empty.
    /// O(log n) on the BTreeMap; never iterates the sidecar.
    #[must_use]
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// Sum of resting quantity on a side, in lots. O(n) over the price
    /// levels; intended for tests and snapshots, not the hot path.
    #[must_use]
    pub fn side_qty(&self, side: Side) -> u64 {
        let levels = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        levels
            .values()
            .map(|level| level.total_quantity().unwrap_or(0))
            .sum()
    }

    /// Number of resting orders on a side. O(n) over the price levels.
    #[must_use]
    pub fn side_order_count(&self, side: Side) -> usize {
        let levels = match side {
            Side::Bid => &self.bids,
            Side::Ask => &self.asks,
        };
        levels.values().map(|level| level.order_count()).sum()
    }
}

impl Default for Book {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn domain_to_pl_side(side: Side) -> PlSide {
    match side {
        Side::Bid => PlSide::Buy,
        Side::Ask => PlSide::Sell,
    }
}

#[inline]
fn pl_id_to_domain(id: PlId) -> OrderId {
    // Every order this crate inserts uses `PlId::Sequential(raw)` where
    // raw > 0. Pricelevel must return only these ids in the trades it
    // emits — anything else would indicate a corrupted exchange state.
    match id {
        PlId::Sequential(raw) => OrderId::new(raw).expect("pricelevel must emit valid OrderIds"),
        _ => panic!("pricelevel returned unknown id variant"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(id: u64, side: Side, price: i64, qty: u64) -> RestingOrder {
        order_acct(id, /* default account */ 1, side, price, qty)
    }

    fn order_acct(id: u64, account: u32, side: Side, price: i64, qty: u64) -> RestingOrder {
        RestingOrder {
            order_id: OrderId::new(id).expect("valid order_id fixture"),
            account_id: AccountId::new(account).expect("valid account_id fixture"),
            side,
            price: Price::new(price).expect("valid price fixture"),
            qty: Qty::new(qty).expect("valid qty fixture"),
        }
    }

    #[test]
    fn test_book_new_is_empty() {
        let book = Book::new();
        assert!(book.best_bid().is_none());
        assert!(book.best_ask().is_none());
        assert_eq!(book.side_qty(Side::Bid), 0);
        assert_eq!(book.side_qty(Side::Ask), 0);
    }

    #[test]
    fn test_add_resting_records_qty_and_best_bid() {
        let mut book = Book::new();
        assert!(book.add_resting(order(1, Side::Bid, 100, 10)).is_ok());
        assert_eq!(book.best_bid(), Some(Price::new(100).expect("ok")));
        assert_eq!(book.side_qty(Side::Bid), 10);
    }

    #[test]
    fn test_add_duplicate_order_id_returns_err() {
        let mut book = Book::new();
        let o = order(1, Side::Bid, 100, 10);
        assert!(book.add_resting(o).is_ok());
        assert_eq!(book.add_resting(o), Err(BookError::DuplicateOrderId));
    }

    #[test]
    fn test_cancel_unknown_order_id_returns_err() {
        let mut book = Book::new();
        assert_eq!(
            book.cancel(OrderId::new(99).expect("ok")),
            Err(BookError::UnknownOrderId)
        );
    }

    #[test]
    fn test_add_then_cancel_restores_state() {
        let mut book = Book::new();
        let o = order(1, Side::Bid, 100, 10);
        book.add_resting(o).expect("add");
        let (side, price) = book.cancel(o.order_id).expect("cancel");
        assert_eq!(side, Side::Bid);
        assert_eq!(price, o.price);
        assert!(book.best_bid().is_none());
        assert_eq!(book.side_qty(Side::Bid), 0);
        assert_eq!(book.side_order_count(Side::Bid), 0);
    }

    #[test]
    fn test_cancel_twice_returns_err_on_second_call() {
        let mut book = Book::new();
        let o = order(1, Side::Bid, 100, 10);
        book.add_resting(o).expect("add");
        assert!(book.cancel(o.order_id).is_ok());
        assert_eq!(book.cancel(o.order_id), Err(BookError::UnknownOrderId));
    }

    #[test]
    fn test_best_bid_picks_max_price() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Bid, 100, 10)).expect("add");
        book.add_resting(order(2, Side::Bid, 105, 10)).expect("add");
        book.add_resting(order(3, Side::Bid, 102, 10)).expect("add");
        assert_eq!(book.best_bid(), Some(Price::new(105).expect("ok")));
    }

    #[test]
    fn test_best_ask_picks_min_price() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 10)).expect("add");
        book.add_resting(order(2, Side::Ask, 95, 10)).expect("add");
        book.add_resting(order(3, Side::Ask, 98, 10)).expect("add");
        assert_eq!(book.best_ask(), Some(Price::new(95).expect("ok")));
    }

    #[test]
    fn test_level_removed_when_last_order_cancelled() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Bid, 100, 10)).expect("add");
        book.add_resting(order(2, Side::Bid, 100, 5)).expect("add");
        // Both at price 100 — level remains after the first cancel.
        book.cancel(OrderId::new(1).expect("ok")).expect("cancel");
        assert_eq!(book.best_bid(), Some(Price::new(100).expect("ok")));
        assert_eq!(book.side_qty(Side::Bid), 5);
        // Level disappears after the last cancel.
        book.cancel(OrderId::new(2).expect("ok")).expect("cancel");
        assert!(book.best_bid().is_none());
        assert_eq!(book.side_qty(Side::Bid), 0);
    }

    #[test]
    fn test_qty_conservation_across_add_cancel_sequence() {
        let mut book = Book::new();
        for i in 1..=10 {
            book.add_resting(order(i, Side::Bid, 100 + i as i64, 10))
                .expect("add");
        }
        assert_eq!(book.side_qty(Side::Bid), 100);
        assert_eq!(book.side_order_count(Side::Bid), 10);

        for i in 1..=10 {
            book.cancel(OrderId::new(i).expect("ok")).expect("cancel");
        }
        assert_eq!(book.side_qty(Side::Bid), 0);
        assert_eq!(book.side_order_count(Side::Bid), 0);
        assert!(book.best_bid().is_none());
    }

    #[test]
    fn test_two_sided_book() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Bid, 99, 10)).expect("add");
        book.add_resting(order(2, Side::Ask, 101, 5)).expect("add");
        assert_eq!(book.best_bid(), Some(Price::new(99).expect("ok")));
        assert_eq!(book.best_ask(), Some(Price::new(101).expect("ok")));
        assert_eq!(book.side_qty(Side::Bid), 10);
        assert_eq!(book.side_qty(Side::Ask), 5);
    }

    use proptest::prelude::*;

    fn arb_side() -> impl Strategy<Value = Side> {
        prop_oneof![Just(Side::Bid), Just(Side::Ask)]
    }

    fn arb_price() -> impl Strategy<Value = Price> {
        (1i64..=1_000).prop_map(|n| Price::new(n).expect("strategy"))
    }

    fn arb_qty() -> impl Strategy<Value = Qty> {
        (1u64..=1_000).prop_map(|n| Qty::new(n).expect("strategy"))
    }

    proptest! {
        /// Adding N orders then cancelling each restores `side_qty`
        /// to 0 on both sides — sum of resting qty is conserved
        /// across non-matching operations (CLAUDE.md core invariant).
        #[test]
        fn proptest_qty_conserved_across_add_cancel(
            orders in prop::collection::vec(
                (arb_side(), arb_price(), arb_qty()),
                1..=50usize,
            )
        ) {
            let mut book = Book::new();
            let mut expected_bid = 0u64;
            let mut expected_ask = 0u64;
            let mut ids = Vec::new();

            for (i, (side, price, qty)) in orders.iter().enumerate() {
                let id = OrderId::new((i as u64) + 1).expect("ok");
                let added = book.add_resting(RestingOrder {
                    order_id: id,
                    account_id: AccountId::new(1).expect("ok"),
                    side: *side, price: *price, qty: *qty
                });
                if added.is_ok() {
                    ids.push(id);
                    match side {
                        Side::Bid => expected_bid += qty.as_lots(),
                        Side::Ask => expected_ask += qty.as_lots(),
                    }
                }
            }

            prop_assert_eq!(book.side_qty(Side::Bid), expected_bid);
            prop_assert_eq!(book.side_qty(Side::Ask), expected_ask);

            for id in ids {
                let _ = book.cancel(id);
            }

            prop_assert_eq!(book.side_qty(Side::Bid), 0);
            prop_assert_eq!(book.side_qty(Side::Ask), 0);
            prop_assert!(book.best_bid().is_none());
            prop_assert!(book.best_ask().is_none());
        }
    }

    // -------------------------------------------------------------------
    // match_aggressive tests
    // -------------------------------------------------------------------

    use domain::{EngineSeq, IdGenerator, TradeId};

    /// Inline `IdGenerator` for tests. Real prod / replay impls live in
    /// `crates/engine/` (issue #12). This stub yields strictly
    /// monotonic ids starting from `1`, deterministic across runs.
    struct MockIds {
        next_trade: u64,
        next_seq: u64,
    }

    impl MockIds {
        fn new() -> Self {
            Self {
                next_trade: 1,
                next_seq: 1,
            }
        }
    }

    impl IdGenerator for MockIds {
        fn next_trade_id(&mut self) -> TradeId {
            let v = self.next_trade;
            self.next_trade += 1;
            TradeId::new(v)
        }
        fn next_engine_seq(&mut self) -> EngineSeq {
            let v = self.next_seq;
            self.next_seq += 1;
            EngineSeq::new(v)
        }
    }

    fn taker(id: u64, side: Side, price: Option<i64>, qty: u64) -> AggressiveOrder {
        taker_acct(id, /* default account */ 99, side, price, qty)
    }

    fn taker_acct(
        id: u64,
        account: u32,
        side: Side,
        price: Option<i64>,
        qty: u64,
    ) -> AggressiveOrder {
        AggressiveOrder {
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side,
            price: price.map(|p| Price::new(p).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
            tif: domain::Tif::Gtc,
        }
    }

    /// Helper to build the auxiliary STP buffer required by the
    /// 4-arg `match_aggressive` signature. Tests that don't expect
    /// STP can ignore the second `Vec` after the call.
    fn stp_buf() -> Vec<StpCancellation> {
        Vec::new()
    }

    #[test]
    fn test_match_aggressive_empty_book_zero_fills() {
        let mut book = Book::new();
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(1, Side::Bid, Some(100), 10),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 0);
        assert_eq!(r.taker_remaining, Some(Qty::new(10).expect("ok")));
        assert!(buf.is_empty());
    }

    #[test]
    fn test_match_aggressive_full_fill_single_maker() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 10)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(2, Side::Bid, Some(100), 10),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, None);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].qty, Qty::new(10).expect("ok"));
        assert_eq!(buf[0].price, Price::new(100).expect("ok"));
        assert_eq!(buf[0].maker_order_id, OrderId::new(1).expect("ok"));
        assert_eq!(buf[0].taker_order_id, OrderId::new(2).expect("ok"));
        assert_eq!(buf[0].maker_side, Side::Ask);
        assert!(buf[0].maker_fully_filled);
        // Level removed.
        assert!(book.best_ask().is_none());
        assert_eq!(book.side_qty(Side::Ask), 0);
    }

    #[test]
    fn test_match_aggressive_partial_taker_remaining() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        // Taker wants 10 but only 5 resting at any crossing price.
        let r = book.match_aggressive(
            taker(2, Side::Bid, Some(100), 10),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, Some(Qty::new(5).expect("ok")));
        assert_eq!(buf[0].qty, Qty::new(5).expect("ok"));
    }

    #[test]
    fn test_match_aggressive_walks_multiple_levels_best_first() {
        let mut book = Book::new();
        // Asks at 100 (qty 5), 105 (qty 5). Buy taker for 8 lots crosses
        // both — should consume 100 first, then 3 from 105.
        book.add_resting(order(1, Side::Ask, 105, 5)).expect("add");
        book.add_resting(order(2, Side::Ask, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(3, Side::Bid, Some(110), 8),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 2);
        assert_eq!(r.taker_remaining, None);
        // First fill at the better (lower) ask price.
        assert_eq!(buf[0].price, Price::new(100).expect("ok"));
        assert_eq!(buf[0].qty, Qty::new(5).expect("ok"));
        // Second fill at the worse ask, partial.
        assert_eq!(buf[1].price, Price::new(105).expect("ok"));
        assert_eq!(buf[1].qty, Qty::new(3).expect("ok"));
        // 100 level emptied; 105 level still has 2 lots resting.
        assert_eq!(book.best_ask(), Some(Price::new(105).expect("ok")));
        assert_eq!(book.side_qty(Side::Ask), 2);
    }

    #[test]
    fn test_match_aggressive_limit_stops_at_non_crossing_level() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 5)).expect("add");
        book.add_resting(order(2, Side::Ask, 105, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        // Taker price 100 only crosses the 100 level, not 105.
        let r = book.match_aggressive(
            taker(3, Side::Bid, Some(100), 10),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, Some(Qty::new(5).expect("ok")));
        assert_eq!(buf[0].price, Price::new(100).expect("ok"));
        // 105 level untouched.
        assert_eq!(book.side_qty(Side::Ask), 5);
    }

    #[test]
    fn test_match_aggressive_market_order_walks_all_crossing_levels() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 3)).expect("add");
        book.add_resting(order(2, Side::Ask, 110, 3)).expect("add");
        book.add_resting(order(3, Side::Ask, 200, 3)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        // None price = market order; sweeps until book empty or filled.
        let r = {
            let mut s = stp_buf();
            book.match_aggressive(taker(4, Side::Bid, None, 100), &mut ids, &mut buf, &mut s)
        };
        assert_eq!(r.fills_count, 3);
        assert_eq!(r.taker_remaining, Some(Qty::new(91).expect("ok"))); // 100 - 9 filled
        assert!(book.best_ask().is_none());
    }

    #[test]
    fn test_match_aggressive_bid_taker_walks_asks_ascending() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 200, 5)).expect("add");
        book.add_resting(order(2, Side::Ask, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(3, Side::Bid, Some(300), 5),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        // Lower ask hit first.
        assert_eq!(buf[0].price, Price::new(100).expect("ok"));
    }

    #[test]
    fn test_match_aggressive_ask_taker_walks_bids_descending() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Bid, 50, 5)).expect("add");
        book.add_resting(order(2, Side::Bid, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(3, Side::Ask, Some(40), 5),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        // Higher bid hit first.
        assert_eq!(buf[0].price, Price::new(100).expect("ok"));
    }

    #[test]
    fn test_match_aggressive_sidecar_clean_after_full_fills() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 5)).expect("add");
        book.add_resting(order(2, Side::Ask, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(3, Side::Bid, Some(100), 10),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 2);
        assert_eq!(r.taker_remaining, None);
        // Both makers fully filled — sidecar should not retain them;
        // a follow-up cancel must return UnknownOrderId.
        assert_eq!(
            book.cancel(OrderId::new(1).expect("ok")),
            Err(BookError::UnknownOrderId)
        );
        assert_eq!(
            book.cancel(OrderId::new(2).expect("ok")),
            Err(BookError::UnknownOrderId)
        );
    }

    #[test]
    fn test_match_aggressive_trade_id_monotonic_per_fill() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 3)).expect("add");
        book.add_resting(order(2, Side::Ask, 110, 3)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker(3, Side::Bid, Some(120), 6),
            &mut ids,
            &mut buf,
            &mut stp,
        );
        assert_eq!(r.fills_count, 2);
        assert_eq!(buf[0].trade_id, TradeId::new(1));
        assert_eq!(buf[1].trade_id, TradeId::new(2));
    }

    proptest! {
        /// CLAUDE.md core invariant: `maker.price == trade.price` for
        /// every emitted fill.
        #[test]
        fn proptest_maker_price_equals_trade_price(
            ask_levels in prop::collection::vec((1i64..=100, 1u64..=100u64), 1..=10usize),
            taker_qty in 1u64..=1_000u64,
        ) {
            use std::collections::HashMap;
            let mut book = Book::new();
            let mut maker_prices: HashMap<u64, i64> = HashMap::new();
            for (i, (price, qty)) in ask_levels.iter().enumerate() {
                let oid = (i as u64) + 1;
                maker_prices.insert(oid, *price);
                let _ = book.add_resting(order(oid, Side::Ask, *price, *qty));
            }
            let mut ids = MockIds::new();
            let mut buf = Vec::new();
            let mut stp = stp_buf();
            book.match_aggressive(
                taker(9999, Side::Bid, None, taker_qty),
                &mut ids,
                &mut buf,
                &mut stp,
            );
            for fill in &buf {
                let expected_price =
                    maker_prices.get(&fill.maker_order_id.as_raw()).copied();
                prop_assert_eq!(Some(fill.price), expected_price.map(|p| Price::new(p).expect("ok")));
                prop_assert_ne!(fill.maker_order_id, fill.taker_order_id);
                prop_assert_eq!(fill.maker_side, Side::Ask);
            }
        }

        /// Every fill has exactly one maker and one taker — structural
        /// in the type, but cover the run-time path against any future
        /// regression that copies the same id into both fields.
        #[test]
        fn proptest_fill_has_distinct_maker_and_taker(
            asks in prop::collection::vec((1i64..=100, 1u64..=100u64), 1..=10usize),
        ) {
            let mut book = Book::new();
            for (i, (price, qty)) in asks.iter().enumerate() {
                let _ = book.add_resting(order((i as u64) + 1, Side::Ask, *price, *qty));
            }
            let mut ids = MockIds::new();
            let mut buf = Vec::new();
            let mut stp = stp_buf();
            book.match_aggressive(
                taker(9999, Side::Bid, None, 1_000_000),
                &mut ids,
                &mut buf,
                &mut stp,
            );
            for fill in &buf {
                prop_assert_ne!(fill.maker_order_id, fill.taker_order_id);
            }
        }
    }

    // -------------------------------------------------------------------
    // Self-trade prevention (cancel-both)
    // -------------------------------------------------------------------

    #[test]
    fn test_stp_same_account_first_maker_cancels_both() {
        // Account 7 has both a resting ask at 100 (qty 5) and a buy
        // taker for 5 — the only candidate maker is the same account.
        // STP must drop the maker, halt the walk, and signal taker
        // cancel; the buy must NOT fill.
        let mut book = Book::new();
        book.add_resting(order_acct(1, /* acct */ 7, Side::Ask, 100, 5))
            .expect("add");
        let mut ids = MockIds::new();
        let mut fills = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker_acct(2, /* acct */ 7, Side::Bid, Some(100), 5),
            &mut ids,
            &mut fills,
            &mut stp,
        );
        assert_eq!(r.fills_count, 0);
        assert_eq!(r.stp_cancellations, 1);
        assert!(r.taker_stp_cancelled);
        assert_eq!(r.taker_remaining, Some(Qty::new(5).expect("ok")));
        assert_eq!(stp[0].order_id, OrderId::new(1).expect("ok"));
        assert_eq!(stp[0].account_id, AccountId::new(7).expect("ok"));
        assert_eq!(stp[0].price, Price::new(100).expect("ok"));
        assert_eq!(stp[0].qty, Qty::new(5).expect("ok"));
        // Maker dropped from book.
        assert!(book.best_ask().is_none());
        // Sidecar cleaned.
        assert_eq!(
            book.cancel(OrderId::new(1).expect("ok")),
            Err(BookError::UnknownOrderId)
        );
    }

    #[test]
    fn test_stp_after_partial_fill_against_other_account() {
        // Asks (FIFO order at price 100): order 1 acct B (qty 3),
        // order 2 acct A (qty 5). Buy taker acct A for 10.
        // Walk fills 3 from order 1 (acct B), then hits order 2
        // (acct A) → STP. Result: 1 fill, 1 STP, taker cancelled.
        let mut book = Book::new();
        book.add_resting(order_acct(1, /* acct */ 2, Side::Ask, 100, 3))
            .expect("add");
        book.add_resting(order_acct(2, /* acct */ 7, Side::Ask, 100, 5))
            .expect("add");
        let mut ids = MockIds::new();
        let mut fills = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker_acct(99, /* acct */ 7, Side::Bid, Some(100), 10),
            &mut ids,
            &mut fills,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.stp_cancellations, 1);
        assert!(r.taker_stp_cancelled);
        assert_eq!(fills[0].maker_order_id, OrderId::new(1).expect("ok"));
        assert_eq!(fills[0].qty, Qty::new(3).expect("ok"));
        assert_eq!(stp[0].order_id, OrderId::new(2).expect("ok"));
        // Taker had 10, 3 filled, 7 remaining at STP.
        assert_eq!(r.taker_remaining, Some(Qty::new(7).expect("ok")));
    }

    #[test]
    fn test_stp_no_fire_when_different_accounts() {
        let mut book = Book::new();
        book.add_resting(order_acct(1, 2, Side::Ask, 100, 5))
            .expect("add");
        let mut ids = MockIds::new();
        let mut fills = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker_acct(2, 7, Side::Bid, Some(100), 5),
            &mut ids,
            &mut fills,
            &mut stp,
        );
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.stp_cancellations, 0);
        assert!(!r.taker_stp_cancelled);
        assert!(r.taker_remaining.is_none()); // fully filled
        assert!(stp.is_empty());
    }

    #[test]
    fn test_stp_cancels_first_same_account_skips_remaining_levels() {
        // Bid taker on acct A. Asks: 100 acct A (qty 1), 105 acct B (qty 5).
        // Even though 105 has a different-account maker that would also
        // cross, STP at 100 halts the walk — cancel-both stops the taker.
        let mut book = Book::new();
        book.add_resting(order_acct(1, 7, Side::Ask, 100, 1))
            .expect("add");
        book.add_resting(order_acct(2, 2, Side::Ask, 105, 5))
            .expect("add");
        let mut ids = MockIds::new();
        let mut fills = Vec::new();
        let mut stp = stp_buf();
        let r = book.match_aggressive(
            taker_acct(3, 7, Side::Bid, Some(110), 6),
            &mut ids,
            &mut fills,
            &mut stp,
        );
        assert_eq!(r.fills_count, 0);
        assert_eq!(r.stp_cancellations, 1);
        assert!(r.taker_stp_cancelled);
        // 105 level untouched — different-account maker still resting.
        assert_eq!(book.best_ask(), Some(Price::new(105).expect("ok")));
        assert_eq!(book.side_qty(Side::Ask), 5);
    }
}
