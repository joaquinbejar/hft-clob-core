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
        let price = if i.is_multiple_of(2) { 200 } else { 50 };
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
