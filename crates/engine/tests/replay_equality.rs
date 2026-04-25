//! Determinism property: two independent replay runs over the same
//! recorded inbound stream produce a byte-identical outbound stream
//! (under `--no-timestamps`). This is the contract behind the
//! `fixtures/outbound.golden` diff.
//!
//! The proptest synthesises a random valid inbound recording, then
//! invokes the replay binary's pure `replay_bytes` entry point twice
//! and asserts equality. Because the replay path uses
//! `engine::StubClock` + `engine::CounterIdGenerator` and consumes
//! the recorded `recv_ts` deterministically, the only sources of
//! per-run divergence would be hash-based iteration leaking into
//! outputs (CLAUDE.md core invariant) or any `SystemTime::now` call
//! escaping the matching / engine boundary.

use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};
use proptest::prelude::*;
use wire::framing::{Frame, MessageKind};
use wire::inbound::new_order;

#[path = "../src/bin/replay.rs"]
#[allow(dead_code, unused_imports, clippy::all)]
mod replay;

#[derive(Debug, Clone)]
struct InboundCase {
    side: Side,
    price: i64,
    qty: u64,
    account: u32,
    recv_ts: i64,
}

fn arb_inbound_case() -> impl Strategy<Value = InboundCase> {
    (
        prop_oneof![Just(Side::Bid), Just(Side::Ask)],
        50i64..=200i64,
        1u64..=20u64,
        1u32..=8u32,
        1i64..=10_000i64,
    )
        .prop_map(|(side, price, qty, account, recv_ts)| InboundCase {
            side,
            price,
            qty,
            account,
            recv_ts,
        })
}

/// Encode a sequence of cases into the recorder's on-disk format.
fn encode_recording(cases: &[InboundCase]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let msg = wire::inbound::NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new((i as u64) + 1).expect("ok"),
            account_id: AccountId::new(c.account).expect("ok"),
            side: c.side,
            order_type: OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(Price::new(c.price).expect("ok")),
            qty: Qty::new(c.qty).expect("ok"),
        };
        let mut payload = Vec::new();
        new_order::encode(&msg, &mut payload);
        let mut frame = Vec::new();
        Frame::write(MessageKind::NewOrder, &payload, &mut frame).expect("fits");
        bytes.extend_from_slice(&frame);
        bytes.extend_from_slice(&(c.recv_ts as u64).to_le_bytes());
    }
    bytes
}

proptest! {
    /// Determinism: two replays produce byte-identical outbound under
    /// `--no-timestamps`.
    #[test]
    fn proptest_replay_byte_identical_no_timestamps(
        cases in prop::collection::vec(arb_inbound_case(), 1..=30)
    ) {
        let inbound = encode_recording(&cases);
        let a = replay::replay_bytes(&inbound, true).expect("replay a");
        let b = replay::replay_bytes(&inbound, true).expect("replay b");
        prop_assert_eq!(&a, &b);
    }

    /// Determinism with timestamps: when `--no-timestamps` is OFF the
    /// outbound is still byte-identical across runs because the
    /// engine reads time via `StubClock` which returns the recorded
    /// `recv_ts` deterministically. (Production runs use `WallClock`,
    /// which would differ; the timestamp suppression flag exists for
    /// the upstream `pricelevel::Trade::new` wall-clock leak path —
    /// not actually exercised today since matching never reads
    /// pricelevel `Trade::timestamp`.)
    #[test]
    fn proptest_replay_byte_identical_with_timestamps_under_stub_clock(
        cases in prop::collection::vec(arb_inbound_case(), 1..=20)
    ) {
        let inbound = encode_recording(&cases);
        let a = replay::replay_bytes(&inbound, false).expect("replay a");
        let b = replay::replay_bytes(&inbound, false).expect("replay b");
        prop_assert_eq!(&a, &b);
    }
}
