//! Property: the book is never crossed (`best_bid < best_ask`
//! whenever both sides are non-empty) after any valid sequence of
//! new-order / cancel / replace operations.
//!
//! Regression coverage for issue #59: `Book::replace`'s lose-priority
//! path rested the new (price, qty) without re-running the matching
//! walk, so a reprice through the spread left `best_bid >= best_ask`.
//!
//! The command driver mirrors the engine pipeline's use of the book:
//! a new order goes through `match_aggressive` and only the
//! non-crossing residual rests; cancel and replace hit the public
//! `Book` API directly.

use domain::{AccountId, EngineSeq, IdGenerator, OrderId, Price, Qty, Side, Tif, TradeId};
use matching::{AggressiveOrder, Book, Fill, RestingOrder, StpCancellation};
use proptest::prelude::*;

/// Deterministic monotonic id source for the fill loop. Matching
/// tests cannot use `engine::CounterIdGenerator` (that would invert
/// the crate dependency), so a local stub implements the same
/// contract.
struct TestIds {
    trade: u64,
    seq: u64,
}

impl TestIds {
    fn new() -> Self {
        Self { trade: 0, seq: 0 }
    }
}

impl IdGenerator for TestIds {
    fn next_trade_id(&mut self) -> TradeId {
        self.trade += 1;
        TradeId::new(self.trade)
    }

    fn next_engine_seq(&mut self) -> EngineSeq {
        self.seq += 1;
        EngineSeq::new(self.seq)
    }
}

#[derive(Clone, Debug)]
enum Command {
    NewOrder {
        order_id: u64,
        account_id: u32,
        side: Side,
        price: i64,
        qty: u64,
        tif: Tif,
    },
    Cancel {
        order_id: u64,
    },
    Replace {
        order_id: u64,
        new_price: i64,
        new_qty: u64,
    },
}

/// Small id space so cancels / replaces hit live orders often; tight
/// price band so reprices cross the touch often. Uniform prices over
/// a wide range would leave the interesting paths cold.
fn arb_command() -> impl Strategy<Value = Command> {
    let arb_tif = prop_oneof![
        6 => Just(Tif::Gtc),
        2 => Just(Tif::PostOnly),
        1 => Just(Tif::Ioc),
    ];
    prop_oneof![
        6 => (
            1u64..60u64,
            1u32..5u32,
            prop_oneof![Just(Side::Bid), Just(Side::Ask)],
            90i64..=110i64,
            1u64..=20u64,
            arb_tif,
        )
            .prop_map(|(order_id, account_id, side, price, qty, tif)| {
                Command::NewOrder { order_id, account_id, side, price, qty, tif }
            }),
        2 => (1u64..60u64).prop_map(|order_id| Command::Cancel { order_id }),
        3 => (1u64..60u64, 90i64..=110i64, 1u64..=20u64).prop_map(
            |(order_id, new_price, new_qty)| Command::Replace { order_id, new_price, new_qty }
        ),
    ]
}

/// Mirror of `engine::handle_new_order`'s book interaction: run the
/// matching walk, then rest the residual only when the order
/// survived (no STP, no post-only reject, TIF rests).
#[allow(clippy::too_many_arguments)]
fn apply_new_order(
    book: &mut Book,
    ids: &mut TestIds,
    fills: &mut Vec<Fill>,
    stp: &mut Vec<StpCancellation>,
    order_id: u64,
    account_id: u32,
    side: Side,
    price: i64,
    qty: u64,
    tif: Tif,
) {
    let (Ok(order_id), Ok(account_id), Ok(price), Ok(qty)) = (
        OrderId::new(order_id),
        AccountId::new(account_id),
        Price::new(price),
        Qty::new(qty),
    ) else {
        return;
    };
    // NOTE: the engine rejects duplicate live order ids before the
    // walk; this driver instead lets `add_resting` fail on the
    // duplicate below. The walk still runs, which is harmless for
    // this invariant — matching can only uncross, never cross.
    let result = book.match_aggressive(
        AggressiveOrder {
            order_id,
            account_id,
            side,
            price: Some(price),
            qty,
            tif,
        },
        ids,
        fills,
        stp,
    );
    if result.taker_stp_cancelled || result.taker_post_only_rejected || tif == Tif::Ioc {
        return;
    }
    if let Some(remaining) = result.taker_remaining {
        let _ = book.add_resting(RestingOrder {
            order_id,
            account_id,
            side,
            price,
            qty: remaining,
            tif,
        });
    }
}

fn assert_not_crossed(book: &Book) -> Result<(), TestCaseError> {
    if let (Some(bid), Some(ask)) = (book.best_bid(), book.best_ask()) {
        prop_assert!(
            bid < ask,
            "book crossed: best_bid={} best_ask={}",
            bid.as_ticks(),
            ask.as_ticks()
        );
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 50_000,
        .. ProptestConfig::default()
    })]

    #[test]
    fn book_never_crossed_across_add_cancel_replace(
        commands in prop::collection::vec(arb_command(), 1..200)
    ) {
        let mut book = Book::new();
        let mut ids = TestIds::new();
        let mut fills = Vec::new();
        let mut stp = Vec::new();

        for cmd in &commands {
            match cmd {
                Command::NewOrder { order_id, account_id, side, price, qty, tif } => {
                    apply_new_order(
                        &mut book, &mut ids, &mut fills, &mut stp,
                        *order_id, *account_id, *side, *price, *qty, *tif,
                    );
                }
                Command::Cancel { order_id } => {
                    if let Ok(oid) = OrderId::new(*order_id) {
                        let _ = book.cancel(oid);
                    }
                }
                Command::Replace { order_id, new_price, new_qty } => {
                    if let (Ok(oid), Ok(p), Ok(q)) = (
                        OrderId::new(*order_id),
                        Price::new(*new_price),
                        Qty::new(*new_qty),
                    ) {
                        let _ = book.replace(oid, p, q, &mut ids, &mut fills, &mut stp);
                    }
                }
            }
            // The invariant must hold after EVERY operation, not just
            // at the end — a transiently crossed book is still a bug.
            assert_not_crossed(&book)?;
        }
    }
}
