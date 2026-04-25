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

use domain::{OrderId, Price, Qty, Side};
use pricelevel::{
    Hash32, Id as PlId, OrderType as PlOrderType, OrderUpdate as PlOrderUpdate, Price as PlPrice,
    PriceLevel, Quantity as PlQuantity, Side as PlSide, TimeInForce as PlTif, TimestampMs,
};

use crate::error::BookError;

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
}
