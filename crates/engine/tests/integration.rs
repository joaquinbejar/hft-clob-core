//! End-to-end smoke test: 100 inbound orders → engine → encoded
//! outbound stream. Verifies the contract from #14:
//!
//! - `engine_seq` strictly monotonic across the full stream.
//! - Top-of-book emits exactly once per `step` (de-duped across
//!   multi-fill steps).
//! - Each fill produces exactly one `TradePrint`.
//! - Bytes round-trip cleanly through the marketdata encoder /
//!   decoder.

use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};
use engine::{CounterIdGenerator, Engine, StubClock, VecSink};
use marketdata::encoder;
use wire::inbound::{Inbound, NewOrder};
use wire::outbound::Outbound;

fn limit_order(id: u64, account: u32, side: Side, price: i64, qty: u64) -> NewOrder {
    NewOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(id).expect("ok"),
        account_id: AccountId::new(account).expect("ok"),
        side,
        order_type: OrderType::Limit,
        tif: Tif::Gtc,
        price: Some(Price::new(price).expect("ok")),
        qty: Qty::new(qty).expect("ok"),
    }
}

#[test]
fn smoke_100_orders_engine_seq_is_strictly_monotonic_after_decode() {
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );

    // 50 sell makers at increasing prices on account 2, then 50 buy
    // takers at saturating prices on account 7. Mixed crosses,
    // partial fills, and rests across the run.
    for i in 0..50u64 {
        let id = i + 1;
        let price = 100 + (i as i64);
        engine.step(Inbound::NewOrder(limit_order(id, 2, Side::Ask, price, 5)));
    }
    for i in 0..50u64 {
        let id = 1000 + i;
        // Some buys cross (price >= 100), some don't (rest at 50..).
        let price = if i % 2 == 0 { 200 } else { 50 };
        let qty = (i % 5) + 1;
        engine.step(Inbound::NewOrder(limit_order(id, 7, Side::Bid, price, qty)));
    }

    // Drain the engine's sink and re-encode through the marketdata
    // wire encoder.
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    assert!(!events.is_empty(), "engine emitted nothing");

    let mut bytes = Vec::with_capacity(events.len() * 64);
    for ev in &events {
        encoder::encode(ev, &mut bytes).expect("encode");
    }

    // Decode the byte stream back into Outbound and assert
    // engine_seq strict monotonicity over the wire.
    let mut cursor = 0;
    let mut last_seq = 0u64;
    let mut decoded_count = 0;
    while cursor < bytes.len() {
        let (msg, total) = encoder::decode(&bytes[cursor..]).expect("decode");
        cursor += total;
        decoded_count += 1;
        let seq = match msg {
            Outbound::ExecReport(r) => r.engine_seq.as_raw(),
            Outbound::TradePrint(t) => t.engine_seq.as_raw(),
            Outbound::BookUpdateTop(b) => b.engine_seq.as_raw(),
            Outbound::BookUpdateL2Delta(d) => d.engine_seq.as_raw(),
            Outbound::SnapshotResponse(s) => s.engine_seq.as_raw(),
        };
        assert!(
            seq > last_seq,
            "engine_seq must be strictly monotonic: prev={last_seq}, current={seq}"
        );
        last_seq = seq;
    }
    assert_eq!(cursor, bytes.len());
    assert_eq!(decoded_count, events.len());
}

#[test]
fn smoke_book_update_top_emits_exactly_once_per_step() {
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );

    // One inbound — one BookUpdateTop on the wire.
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 100, 5)));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let tob_count = events
        .iter()
        .filter(|o| matches!(o, Outbound::BookUpdateTop(_)))
        .count();
    assert_eq!(tob_count, 1);
}

#[test]
fn smoke_trade_print_emits_exactly_once_per_fill() {
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );

    // Two resting asks at price 100 (qty 5 each); one bid taker for
    // 8 — produces 2 fills (5 + 3).
    engine.step(Inbound::NewOrder(limit_order(1, 2, Side::Ask, 100, 5)));
    engine.step(Inbound::NewOrder(limit_order(2, 2, Side::Ask, 100, 5)));
    // Drain so we count only the cross-step output.
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    engine.step(Inbound::NewOrder(limit_order(3, 7, Side::Bid, 100, 8)));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let trade_count = events
        .iter()
        .filter(|o| matches!(o, Outbound::TradePrint(_)))
        .count();
    assert_eq!(trade_count, 2);
}

#[test]
fn smoke_l2_delta_emits_for_new_resting_level() {
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );

    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 100, 5)));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let l2_deltas: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateL2Delta(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(l2_deltas.len(), 1);
    assert_eq!(l2_deltas[0].side, Side::Bid);
    assert_eq!(l2_deltas[0].price.as_ticks(), 100);
    assert_eq!(l2_deltas[0].new_qty.map(|q| q.as_lots()), Some(5));
}

#[test]
fn smoke_l2_delta_emits_for_level_removed() {
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 100, 5)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    use wire::inbound::CancelOrder;
    engine.step(Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(7).expect("ok"),
    }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let l2_deltas: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateL2Delta(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(l2_deltas.len(), 1);
    assert_eq!(l2_deltas[0].side, Side::Bid);
    assert_eq!(l2_deltas[0].price.as_ticks(), 100);
    assert!(
        l2_deltas[0].new_qty.is_none(),
        "level removed = None new_qty"
    );
}

#[test]
fn smoke_l2_snapshot_plus_deltas_reconstruct_book_state() {
    use std::collections::BTreeMap;
    use wire::inbound::SnapshotRequest;
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );

    // Start with a populated book. Drain — these emissions are
    // not part of the recovery test.
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 99, 5)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 100, 3)));
    engine.step(Inbound::NewOrder(limit_order(3, 2, Side::Ask, 101, 4)));
    engine.step(Inbound::NewOrder(limit_order(4, 2, Side::Ask, 102, 6)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Snapshot the book — this is what a fresh subscriber would
    // ingest at connect time.
    engine.step(Inbound::SnapshotRequest(SnapshotRequest { request_id: 1 }));
    let snap_events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let snap = snap_events
        .iter()
        .find_map(|o| match o {
            Outbound::SnapshotResponse(s) => Some(s.clone()),
            _ => None,
        })
        .expect("snapshot present");

    // Build a `BTreeMap<(Side, Price), Qty>` from the snapshot.
    let mut mirror: BTreeMap<(Side, i64), u64> = BTreeMap::new();
    for level in &snap.bids {
        mirror.insert((Side::Bid, level.price.as_ticks()), level.qty.as_lots());
    }
    for level in &snap.asks {
        mirror.insert((Side::Ask, level.price.as_ticks()), level.qty.as_lots());
    }

    // Drive a few more steps (cross + cancel + new resting). Apply
    // the captured L2 deltas to the mirror.
    engine.step(Inbound::NewOrder(limit_order(99, 9, Side::Bid, 102, 2)));
    let cross_events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    apply_deltas(&cross_events, &mut mirror);

    use wire::inbound::CancelOrder;
    engine.step(Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(7).expect("ok"),
    }));
    let cancel_events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    apply_deltas(&cancel_events, &mut mirror);

    engine.step(Inbound::NewOrder(limit_order(5, 7, Side::Bid, 95, 8)));
    let add_events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    apply_deltas(&add_events, &mut mirror);

    // Compare the mirror against a fresh snapshot of the engine.
    engine.step(Inbound::SnapshotRequest(SnapshotRequest { request_id: 2 }));
    let post_events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let post_snap = post_events
        .iter()
        .find_map(|o| match o {
            Outbound::SnapshotResponse(s) => Some(s.clone()),
            _ => None,
        })
        .expect("post snapshot");
    let mut post_mirror: BTreeMap<(Side, i64), u64> = BTreeMap::new();
    for level in &post_snap.bids {
        post_mirror.insert((Side::Bid, level.price.as_ticks()), level.qty.as_lots());
    }
    for level in &post_snap.asks {
        post_mirror.insert((Side::Ask, level.price.as_ticks()), level.qty.as_lots());
    }

    assert_eq!(
        mirror, post_mirror,
        "snapshot + N deltas must reconstruct the engine's book state"
    );
}

fn apply_deltas(events: &[Outbound], mirror: &mut std::collections::BTreeMap<(Side, i64), u64>) {
    for ev in events {
        if let Outbound::BookUpdateL2Delta(d) = ev {
            let key = (d.side, d.price.as_ticks());
            match d.new_qty {
                Some(q) => {
                    mirror.insert(key, q.as_lots());
                }
                None => {
                    mirror.remove(&key);
                }
            }
        }
    }
}

#[test]
fn smoke_snapshot_request_returns_book_levels_in_best_first_order() {
    use wire::inbound::SnapshotRequest;
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    // Bids: 100, 99, 95 (descending = best first).
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 99, 5)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 100, 3)));
    engine.step(Inbound::NewOrder(limit_order(3, 7, Side::Bid, 95, 7)));
    // Asks: 101, 105, 110 (ascending = best first).
    engine.step(Inbound::NewOrder(limit_order(4, 2, Side::Ask, 110, 4)));
    engine.step(Inbound::NewOrder(limit_order(5, 2, Side::Ask, 101, 6)));
    engine.step(Inbound::NewOrder(limit_order(6, 2, Side::Ask, 105, 2)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    engine.step(Inbound::SnapshotRequest(SnapshotRequest { request_id: 42 }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let snap = events
        .iter()
        .find_map(|o| match o {
            Outbound::SnapshotResponse(s) => Some(s),
            _ => None,
        })
        .expect("SnapshotResponse emitted");
    assert_eq!(snap.request_id, 42);
    let bid_prices: Vec<i64> = snap.bids.iter().map(|l| l.price.as_ticks()).collect();
    let ask_prices: Vec<i64> = snap.asks.iter().map(|l| l.price.as_ticks()).collect();
    assert_eq!(bid_prices, vec![100, 99, 95], "bids best-first");
    assert_eq!(ask_prices, vec![101, 105, 110], "asks best-first");
    let bid_qtys: Vec<u64> = snap.bids.iter().map(|l| l.qty.as_lots()).collect();
    let ask_qtys: Vec<u64> = snap.asks.iter().map(|l| l.qty.as_lots()).collect();
    assert_eq!(bid_qtys, vec![3, 5, 7]);
    assert_eq!(ask_qtys, vec![6, 2, 4]);
}

#[test]
fn smoke_snapshot_request_on_empty_book_returns_empty_levels() {
    use wire::inbound::SnapshotRequest;
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::SnapshotRequest(SnapshotRequest { request_id: 1 }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let snap = events
        .iter()
        .find_map(|o| match o {
            Outbound::SnapshotResponse(s) => Some(s),
            _ => None,
        })
        .expect("SnapshotResponse emitted");
    assert!(snap.bids.is_empty());
    assert!(snap.asks.is_empty());
}

#[test]
fn smoke_snapshot_response_round_trips_through_marketdata_encoder() {
    use wire::inbound::SnapshotRequest;
    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 100, 5)));
    engine.step(Inbound::NewOrder(limit_order(2, 2, Side::Ask, 101, 3)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    engine.step(Inbound::SnapshotRequest(SnapshotRequest { request_id: 9 }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let original = events
        .iter()
        .find_map(|o| match o {
            Outbound::SnapshotResponse(s) => Some(s.clone()),
            _ => None,
        })
        .expect("snapshot present");
    let mut bytes = Vec::new();
    encoder::encode(&Outbound::SnapshotResponse(original.clone()), &mut bytes).expect("encode");
    let (decoded, total) = encoder::decode(&bytes).expect("decode");
    assert_eq!(total, bytes.len());
    match decoded {
        Outbound::SnapshotResponse(decoded) => assert_eq!(decoded, original),
        _ => panic!("decoded variant mismatch"),
    }
}

/// Helper to drain the inner `VecSink` from the typed `Engine`.
/// Avoids exposing the sink field publicly while still letting
/// the integration test inspect the captured events.
fn engine_inner_sink(engine: &mut Engine<StubClock, CounterIdGenerator, VecSink>) -> &mut VecSink {
    // The engine doesn't expose its sink mutably by design; we use
    // a tiny accessor in the engine crate's public API. Since the
    // integration test sits in the `tests/` directory it cannot
    // poke private fields, but the engine module exposes a
    // `sink_mut` helper for this purpose.
    engine.sink_mut()
}

// ---------------------------------------------------------------------
// Cancel ownership (issue #61)
// ---------------------------------------------------------------------

#[test]
fn cancel_from_non_owner_is_rejected_book_unchanged() {
    use domain::{CancelReason, ExecState, RejectReason};
    use wire::inbound::CancelOrder;

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Bid, 100, 5)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Account 9 tries to cancel account 7's order. Must reject (as
    // UnknownOrderId — no cross-account id leak) with zero mutation.
    engine.step(Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(9).expect("ok"),
    }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    let rejects: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) if r.state == ExecState::Rejected => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].reject_reason, Some(RejectReason::UnknownOrderId));
    assert!(
        !events
            .iter()
            .any(|o| matches!(o, Outbound::ExecReport(r) if r.state == ExecState::Cancelled)),
        "foreign cancel must not cancel"
    );
    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    assert_eq!(
        top.bid.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((100, 5))
    );

    // Regression: the owning account can still cancel.
    engine.step(Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(7).expect("ok"),
    }));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);
    let cancelled: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) if r.state == ExecState::Cancelled => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(cancelled.len(), 1);
    assert_eq!(
        cancelled[0].cancel_reason,
        Some(CancelReason::UserRequested)
    );
    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    assert_eq!(top.bid, None);
}

// ---------------------------------------------------------------------
// Cancel-replace re-match (issue #59)
// ---------------------------------------------------------------------

fn cancel_replace(id: u64, account: u32, new_price: i64, new_qty: u64) -> wire::inbound::Inbound {
    Inbound::CancelReplace(wire::inbound::CancelReplace {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(id).expect("ok"),
        account_id: AccountId::new(account).expect("ok"),
        new_price: Price::new(new_price).expect("ok"),
        new_qty: Qty::new(new_qty).expect("ok"),
    })
}

#[test]
fn cancel_replace_through_spread_emits_replaced_then_fills_and_flattens_book() {
    use domain::{CancelReason, ExecState};

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    // SELL 10@100 (acct 2), BUY 10@99 (acct 7) — non-crossing.
    engine.step(Inbound::NewOrder(limit_order(1, 2, Side::Ask, 100, 10)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 99, 10)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Reprice the buy through the spread: must trade in full.
    engine.step(cancel_replace(2, 7, 100, 10));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Exactly one trade print for the full 10 lots at 100.
    let trades: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::TradePrint(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].price.as_ticks(), 100);
    assert_eq!(trades[0].qty.as_lots(), 10);

    // Exec-report sequence on the wire: the non-terminal Replaced ack
    // (leaves = new_qty) comes FIRST, then the maker + taker fill
    // reports (both terminal Filled).
    let reports: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].state, ExecState::Replaced);
    assert_eq!(reports[0].order_id, OrderId::new(2).expect("ok"));
    assert_eq!(reports[0].cancel_reason, Some(CancelReason::Replaced));
    assert_eq!(reports[0].leaves_qty.map(|q| q.as_lots()), Some(10));
    assert_eq!(reports[1].state, ExecState::Filled);
    assert_eq!(reports[1].order_id, OrderId::new(1).expect("ok"));
    assert_eq!(reports[2].state, ExecState::Filled);
    assert_eq!(reports[2].order_id, OrderId::new(2).expect("ok"));
    assert_eq!(reports[2].leaves_qty, None);

    // The replace ack precedes the trade print in the raw stream.
    let replaced_pos = events
        .iter()
        .position(|o| matches!(o, Outbound::ExecReport(r) if r.state == ExecState::Replaced))
        .expect("replaced ack present");
    let trade_pos = events
        .iter()
        .position(|o| matches!(o, Outbound::TradePrint(_)))
        .expect("trade print present");
    assert!(replaced_pos < trade_pos);

    // engine_seq strictly monotonic across the step's emissions.
    let mut last = 0u64;
    for ev in &events {
        let seq = match ev {
            Outbound::ExecReport(r) => r.engine_seq.as_raw(),
            Outbound::TradePrint(t) => t.engine_seq.as_raw(),
            Outbound::BookUpdateTop(b) => b.engine_seq.as_raw(),
            Outbound::BookUpdateL2Delta(d) => d.engine_seq.as_raw(),
            Outbound::SnapshotResponse(s) => s.engine_seq.as_raw(),
        };
        assert!(seq > last, "engine_seq not monotonic: {last} -> {seq}");
        last = seq;
    }

    // Book flat on both sides — never crossed.
    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    assert_eq!(top.bid, None);
    assert_eq!(top.ask, None);
}

#[test]
fn cancel_replace_partial_cross_rests_residual_at_new_price() {
    use domain::ExecState;

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    // SELL 4@100 (acct 2), BUY 10@99 (acct 7).
    engine.step(Inbound::NewOrder(limit_order(1, 2, Side::Ask, 100, 4)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 99, 10)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Reprice the buy to 100: 4 lots trade, 6 rest at 100.
    engine.step(cancel_replace(2, 7, 100, 10));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    let reports: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) => Some(r),
            _ => None,
        })
        .collect();
    // Replaced ack + maker Filled + taker PartiallyFilled.
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].state, ExecState::Replaced);
    assert_eq!(reports[1].state, ExecState::Filled);
    assert_eq!(reports[2].state, ExecState::PartiallyFilled);
    assert_eq!(reports[2].leaves_qty.map(|q| q.as_lots()), Some(6));

    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    // Residual rests as best bid at 100; ask side empty.
    assert_eq!(
        top.bid.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((100, 6))
    );
    assert_eq!(top.ask, None);
}

#[test]
fn cancel_replace_onto_own_order_stp_cancels_both() {
    use domain::{CancelReason, ExecState};

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    // Same account (7) on both sides.
    engine.step(Inbound::NewOrder(limit_order(1, 7, Side::Ask, 100, 10)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 99, 10)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Reprice the bid onto the account's own ask: cancel-both.
    engine.step(cancel_replace(2, 7, 100, 10));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    assert!(
        !events.iter().any(|o| matches!(o, Outbound::TradePrint(_))),
        "self-trade must not print"
    );
    let cancelled: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) if r.state == ExecState::Cancelled => Some(r),
            _ => None,
        })
        .collect();
    // Maker (id 1) and repriced taker (id 2), both SelfTradePrevented.
    assert_eq!(cancelled.len(), 2);
    assert!(
        cancelled
            .iter()
            .all(|r| r.cancel_reason == Some(CancelReason::SelfTradePrevented))
    );
    assert_eq!(cancelled[0].order_id, OrderId::new(1).expect("ok"));
    assert_eq!(cancelled[1].order_id, OrderId::new(2).expect("ok"));

    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    assert_eq!(top.bid, None);
    assert_eq!(top.ask, None);
}

#[test]
fn cancel_replace_from_non_owner_is_rejected_book_unchanged() {
    use domain::{ExecState, RejectReason};

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::NewOrder(limit_order(1, 2, Side::Ask, 100, 10)));
    engine.step(Inbound::NewOrder(limit_order(2, 7, Side::Bid, 99, 10)));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Account 9 tries to reprice account 7's order through the
    // spread. Must reject (as UnknownOrderId — no cross-account id
    // leak) with zero book mutation.
    engine.step(cancel_replace(2, 9, 100, 10));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    let rejects: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) if r.state == ExecState::Rejected => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].reject_reason, Some(RejectReason::UnknownOrderId));
    assert!(!events.iter().any(|o| matches!(o, Outbound::TradePrint(_))));

    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    assert_eq!(
        top.bid.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((99, 10))
    );
    assert_eq!(
        top.ask.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((100, 10))
    );
}

#[test]
fn cancel_replace_postonly_would_cross_is_rejected_book_unchanged() {
    use domain::{ExecState, RejectReason};

    let mut engine = Engine::new(
        StubClock::new(1_000_000_000),
        CounterIdGenerator::new(),
        VecSink::new(),
    );
    engine.step(Inbound::NewOrder(limit_order(1, 2, Side::Ask, 100, 10)));
    let mut post_only = limit_order(2, 7, Side::Bid, 99, 10);
    post_only.tif = Tif::PostOnly;
    engine.step(Inbound::NewOrder(post_only));
    let _ = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    // Repricing the resting PostOnly bid through the spread must be
    // rejected outright — never-take guarantee — with zero mutation.
    engine.step(cancel_replace(2, 7, 100, 10));
    let events = std::mem::take(&mut engine_inner_sink(&mut engine).events);

    let rejects: Vec<_> = events
        .iter()
        .filter_map(|o| match o {
            Outbound::ExecReport(r) if r.state == ExecState::Rejected => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(rejects.len(), 1);
    assert_eq!(rejects[0].order_id, OrderId::new(2).expect("ok"));
    assert_eq!(
        rejects[0].reject_reason,
        Some(RejectReason::PostOnlyWouldCross)
    );
    assert!(
        !events.iter().any(|o| matches!(o, Outbound::TradePrint(_))),
        "no trade may print"
    );

    let top = events
        .iter()
        .filter_map(|o| match o {
            Outbound::BookUpdateTop(b) => Some(b),
            _ => None,
        })
        .next_back()
        .expect("top-of-book emitted");
    // Original order untouched at 99; ask untouched at 100.
    assert_eq!(
        top.bid.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((99, 10))
    );
    assert_eq!(
        top.ask.map(|(p, q)| (p.as_ticks(), q.as_lots())),
        Some((100, 10))
    );
}
