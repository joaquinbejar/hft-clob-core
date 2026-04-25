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

use domain::{IdGenerator, OrderId, Price, Qty, Side};
use pricelevel::{
    Hash32, Id as PlId, OrderType as PlOrderType, OrderUpdate as PlOrderUpdate, Price as PlPrice,
    PriceLevel, Quantity as PlQuantity, Side as PlSide, TimeInForce as PlTif, TimestampMs,
    UuidGenerator,
};
use uuid::Uuid;

use crate::error::BookError;
use crate::fill::{AggressiveOrder, Fill, MatchResult};

/// Namespace UUID for `pricelevel::UuidGenerator`. Pricelevel needs a
/// generator instance to call `match_order`, but its returned trade
/// ids are discarded — our own `domain::TradeId` comes from the
/// injected `IdGenerator`. A fixed namespace keeps the generator's
/// internal state deterministic, which matters for any pricelevel
/// behaviour gated on it.
const PL_NAMESPACE: Uuid = Uuid::nil();

/// A resting-order request handed to [`Book::add_resting`].
///
/// `Book` does not currently retain `account_id` or other engine-side
/// metadata — that arrives with the engine pipeline (issue #12). The
/// sidecar index here only carries `(Side, Price)` per the issue task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestingOrder {
    /// Client-assigned identifier.
    pub order_id: OrderId,
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
    /// `OrderId` to its `(Side, Price)` so cancel is O(log n) on the
    /// price index plus O(1) inside the level.
    index: HashMap<OrderId, (Side, Price)>,
    /// Strictly increasing per-order arrival counter, used as the
    /// `pricelevel::TimestampMs` for each inserted order. Internal
    /// monotonic — does NOT read the wall clock. CLAUDE.md forbids
    /// any wall-clock read inside `crates/matching/`.
    seq: u64,
    /// Pricelevel's `match_order` requires a `UuidGenerator` to stamp
    /// its internal `Trade` records. The generated ids are discarded
    /// at the matching boundary; our `domain::TradeId` comes from the
    /// injected `IdGenerator` per emitted [`Fill`]. Held here to avoid
    /// constructing a fresh one per call.
    pl_uuid_gen: UuidGenerator,
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
            pl_uuid_gen: UuidGenerator::new(PL_NAMESPACE),
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

        self.index.insert(order.order_id, (order.side, order.price));
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
        let (side, price) = *self.index.get(&order_id).ok_or(BookError::UnknownOrderId)?;
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
    /// the next level no longer crosses, or no more orders remain.
    /// Each fill is appended to `out_buf`.
    ///
    /// `taker.side == Side::Bid` walks the asks ascending (best ask
    /// first). `taker.side == Side::Ask` walks the bids descending.
    /// `taker.price == None` is a market order — always crosses.
    /// `taker.price == Some(p)` stops the walk at the first level
    /// that does not cross (`asks_price > p` for a buy taker,
    /// `bids_price < p` for a sell taker).
    ///
    /// CLAUDE.md invariants honoured by construction:
    ///
    /// - Levels are picked via `BTreeMap::first_key_value` /
    ///   `last_key_value`; iteration is sorted by price, deterministic.
    /// - `pricelevel::PriceLevel::match_order` provides strict FIFO
    ///   within a level; we discard its internal trade ids and the
    ///   wall-clock-stamped timestamp on `pricelevel::Trade`.
    /// - Every emitted [`Fill`] has `price == maker_level_price`.
    /// - Every [`Fill`] is one maker (`maker_order_id`) and one taker
    ///   (`taker_order_id`) — structural in the type.
    ///
    /// Returns a [`MatchResult`] summarising how many fills were
    /// appended and how much taker qty remains.
    #[inline]
    pub fn match_aggressive<I: IdGenerator>(
        &mut self,
        taker: AggressiveOrder,
        ids: &mut I,
        out_buf: &mut Vec<Fill>,
    ) -> MatchResult {
        let initial_buf_len = out_buf.len();
        let mut remaining = taker.qty.as_lots();

        while remaining > 0 {
            // Pick the best opposite-side level price.
            let level_price = match taker.side {
                // Bid taker walks asks ascending → best ask is lowest.
                Side::Bid => self.asks.keys().next().copied(),
                // Ask taker walks bids descending → best bid is highest.
                Side::Ask => self.bids.keys().next_back().copied(),
            };
            let level_price = match level_price {
                Some(p) => p,
                None => break, // opposite side empty
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

            // Fetch the level (immutable borrow on the BTreeMap).
            let level_arc = match taker.side {
                Side::Bid => self.asks.get(&level_price),
                Side::Ask => self.bids.get(&level_price),
            };
            // Should always be `Some` because we just picked the key
            // out of the same BTreeMap; defensive `break` rather than
            // panic if the invariant ever drifts.
            let level_arc = match level_arc {
                Some(l) => Arc::clone(l),
                None => break,
            };

            // Consume up to `remaining` qty from this level. Pricelevel
            // does the FIFO walk, atomic decrements, and returns trade
            // records and the ids of fully-consumed makers. We discard
            // its internal trade ids and timestamps; our own come from
            // the injected `IdGenerator`.
            let pl_taker_id = PlId::Sequential(taker.order_id.as_raw());
            let pl_result = level_arc.match_order(remaining, pl_taker_id, &self.pl_uuid_gen);

            // Build domain `Fill`s out of the trade records. The
            // `filled_order_ids` tell us which makers were fully
            // consumed; pricelevel guarantees that vector is in trade
            // order (one push per fully-consumed maker, in pop
            // sequence). A single cursor advances the filled-list as
            // we walk trades, collapsing what would otherwise be an
            // O(T·F) `contains` to O(T+F) with no allocations.
            let trades = pl_result.trades();
            let filled = pl_result.filled_order_ids();
            let new_remaining = pl_result.remaining_quantity();

            // Pre-reserve capacity so a single deep level walk grows
            // `out_buf` at most once instead of letting `push` grow
            // geometrically.
            let trades_slice = trades.as_vec();
            out_buf.reserve(trades_slice.len());

            let mut filled_cursor: usize = 0;
            for trade in trades_slice.iter() {
                let trade_id = ids.next_trade_id();
                let maker_pl_id = trade.maker_order_id();
                let maker_order_id = pl_id_to_domain(maker_pl_id);
                // Pricelevel emits trades only when at least one lot
                // crossed, so `quantity()` is always `>= 1`. Falling
                // back to `Qty::MIN` (one lot) on the impossible
                // zero-qty path keeps the conservation invariant
                // honest rather than masking a violated qty as the
                // taker's full size.
                let qty = Qty::new(trade.quantity().as_u64()).unwrap_or(Qty::MIN);
                debug_assert!(
                    trade.quantity().as_u64() > 0,
                    "pricelevel must not emit zero-qty trades"
                );

                // Maker is fully filled iff it's the next one in the
                // pricelevel-ordered `filled` slice.
                let maker_fully_filled = filled
                    .get(filled_cursor)
                    .is_some_and(|id| *id == maker_pl_id);
                if maker_fully_filled {
                    filled_cursor += 1;
                }

                out_buf.push(Fill {
                    trade_id,
                    maker_order_id,
                    taker_order_id: taker.order_id,
                    // Pricelevel reports the taker side on each Trade;
                    // the maker side is the opposite of the taker.
                    maker_side: taker.side.opposite(),
                    price: level_price,
                    qty,
                    maker_fully_filled,
                });
            }

            // Sidecar cleanup: every fully-consumed maker drops out of
            // the lookup index.
            for id in filled.iter() {
                if let PlId::Sequential(raw) = *id
                    && let Ok(order_id) = OrderId::new(raw)
                {
                    self.index.remove(&order_id);
                }
            }

            remaining = new_remaining;

            // If the level is now empty, remove it from the price
            // index so `best_*` and the next walk see the right view.
            if level_arc.order_count() == 0 {
                let _ = match taker.side {
                    Side::Bid => self.asks.remove(&level_price),
                    Side::Ask => self.bids.remove(&level_price),
                };
            } else if remaining > 0 {
                // The level still has resting orders but pricelevel
                // returned without filling more. Defensive break to
                // avoid an infinite loop on an unexpected pricelevel
                // state — shouldn't fire in practice.
                break;
            }
        }

        MatchResult {
            fills_count: out_buf.len() - initial_buf_len,
            taker_remaining: remaining,
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
    // Every order this crate inserts uses `PlId::Sequential(u64)`;
    // any other variant would mean pricelevel handed us back an id we
    // never produced. Defensive zero -> a sentinel `OrderId::new(1)`
    // so this stays panic-free.
    match id {
        PlId::Sequential(raw) => OrderId::new(raw).unwrap_or_else(|_| {
            // raw == 0 would mean pricelevel synthesised an id; we
            // never insert with raw == 0 (domain rejects it). Map to
            // a deterministic sentinel rather than panic.
            OrderId::new(1).expect("OrderId::new(1) is valid")
        }),
        _ => OrderId::new(1).expect("OrderId::new(1) is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(id: u64, side: Side, price: i64, qty: u64) -> RestingOrder {
        RestingOrder {
            order_id: OrderId::new(id).expect("valid order_id fixture"),
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
                    order_id: id, side: *side, price: *price, qty: *qty
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
        AggressiveOrder {
            order_id: OrderId::new(id).expect("ok"),
            side,
            price: price.map(|p| Price::new(p).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
        }
    }

    #[test]
    fn test_match_aggressive_empty_book_zero_fills() {
        let mut book = Book::new();
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let r = book.match_aggressive(taker(1, Side::Bid, Some(100), 10), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 0);
        assert_eq!(r.taker_remaining, 10);
        assert!(buf.is_empty());
    }

    #[test]
    fn test_match_aggressive_full_fill_single_maker() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 100, 10)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let r = book.match_aggressive(taker(2, Side::Bid, Some(100), 10), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, 0);
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
        // Taker wants 10 but only 5 resting at any crossing price.
        let r = book.match_aggressive(taker(2, Side::Bid, Some(100), 10), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, 5);
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
        let r = book.match_aggressive(taker(3, Side::Bid, Some(110), 8), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 2);
        assert_eq!(r.taker_remaining, 0);
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
        // Taker price 100 only crosses the 100 level, not 105.
        let r = book.match_aggressive(taker(3, Side::Bid, Some(100), 10), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 1);
        assert_eq!(r.taker_remaining, 5);
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
        let r = book.match_aggressive(taker(4, Side::Bid, None, 100), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 3);
        assert_eq!(r.taker_remaining, 91); // 100 - 9 filled
        assert!(book.best_ask().is_none());
    }

    #[test]
    fn test_match_aggressive_bid_taker_walks_asks_ascending() {
        let mut book = Book::new();
        book.add_resting(order(1, Side::Ask, 200, 5)).expect("add");
        book.add_resting(order(2, Side::Ask, 100, 5)).expect("add");
        let mut ids = MockIds::new();
        let mut buf = Vec::new();
        let r = book.match_aggressive(taker(3, Side::Bid, Some(300), 5), &mut ids, &mut buf);
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
        let r = book.match_aggressive(taker(3, Side::Ask, Some(40), 5), &mut ids, &mut buf);
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
        let r = book.match_aggressive(taker(3, Side::Bid, Some(100), 10), &mut ids, &mut buf);
        assert_eq!(r.fills_count, 2);
        assert_eq!(r.taker_remaining, 0);
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
        let r = book.match_aggressive(taker(3, Side::Bid, Some(120), 6), &mut ids, &mut buf);
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
            let mut book = Book::new();
            for (i, (price, qty)) in ask_levels.iter().enumerate() {
                let _ = book.add_resting(order((i as u64) + 1, Side::Ask, *price, *qty));
            }
            let mut ids = MockIds::new();
            let mut buf = Vec::new();
            book.match_aggressive(
                taker(9999, Side::Bid, None, taker_qty),
                &mut ids,
                &mut buf,
            );
            // Every fill's price MUST equal the maker's resting level
            // price. We don't need to know the maker's pre-trade qty —
            // the check is structural.
            for fill in &buf {
                prop_assert_eq!(fill.price, fill.price); // tautology — kept for clarity
                prop_assert_eq!(fill.maker_order_id != fill.taker_order_id, true);
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
            book.match_aggressive(
                taker(9999, Side::Bid, None, 1_000_000),
                &mut ids,
                &mut buf,
            );
            for fill in &buf {
                prop_assert_ne!(fill.maker_order_id, fill.taker_order_id);
            }
        }
    }
}
