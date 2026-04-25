//! Helper binary to generate the golden inbound fixture.
//!
//! Produces `fixtures/inbound.bin` containing a small but
//! representative recording: a few resting makers on each side and
//! one aggressive cross. The shape is deliberately tiny so the
//! resulting `outbound.golden` stays diff-friendly.
//!
//! Run via `cargo run --bin gen_fixture <out_path>` from a developer
//! workstation when the inbound shape needs to change. CI never runs
//! this binary; it consumes the committed fixture.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};
use wire::framing::{Frame, MessageKind};
use wire::inbound::new_order;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: gen_fixture <out_path>");
        return ExitCode::from(2);
    }
    let out = PathBuf::from(&args[1]);

    let mut bytes = Vec::new();
    // Account 2: three resting asks at 100, 102, 105.
    for (i, (price, qty, ts)) in [(100i64, 5u64, 1_000i64), (102, 3, 1_001), (105, 4, 1_002)]
        .iter()
        .enumerate()
    {
        push_new_order(&mut bytes, (i as u64) + 1, 2, Side::Ask, *price, *qty, *ts);
    }
    // Account 7: two resting bids at 95, 97.
    for (i, (price, qty, ts)) in [(95i64, 4u64, 1_010i64), (97, 2, 1_011)].iter().enumerate() {
        push_new_order(&mut bytes, (i as u64) + 10, 7, Side::Bid, *price, *qty, *ts);
    }
    // Account 9: aggressive bid for 6 lots at 102 — sweeps the 100
    // ask (5) and partials the 102 ask (1).
    push_new_order(&mut bytes, 100, 9, Side::Bid, 102, 6, 2_000);
    // Account 9: aggressive ask for 3 lots at 95 — sweeps 97 (2)
    // and partials 95 (1).
    push_new_order(&mut bytes, 101, 9, Side::Ask, 95, 3, 2_001);
    // Account 2: cancel the 105 ask (id 3) — terminal cancel.
    push_cancel(&mut bytes, 3, 2, 2_002);

    if let Err(e) = fs::write(&out, &bytes) {
        eprintln!("gen_fixture: write {} failed: {e}", out.display());
        return ExitCode::from(2);
    }
    println!("wrote {} bytes to {}", bytes.len(), out.display());
    ExitCode::SUCCESS
}

fn push_new_order(
    out: &mut Vec<u8>,
    id: u64,
    account: u32,
    side: Side,
    price: i64,
    qty: u64,
    recv_ts: i64,
) {
    let msg = wire::inbound::NewOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(id).expect("ok"),
        account_id: AccountId::new(account).expect("ok"),
        side,
        order_type: OrderType::Limit,
        tif: Tif::Gtc,
        price: Some(Price::new(price).expect("ok")),
        qty: Qty::new(qty).expect("ok"),
    };
    let mut payload = Vec::new();
    new_order::encode(&msg, &mut payload);
    let mut frame = Vec::new();
    Frame::write(MessageKind::NewOrder, &payload, &mut frame).expect("fits");
    out.extend_from_slice(&frame);
    out.extend_from_slice(&(recv_ts as u64).to_le_bytes());
}

fn push_cancel(out: &mut Vec<u8>, id: u64, account: u32, recv_ts: i64) {
    let msg = wire::inbound::CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(id).expect("ok"),
        account_id: AccountId::new(account).expect("ok"),
    };
    let mut payload = Vec::new();
    wire::inbound::cancel_order::encode(&msg, &mut payload);
    let mut frame = Vec::new();
    Frame::write(MessageKind::CancelOrder, &payload, &mut frame).expect("fits");
    out.extend_from_slice(&frame);
    out.extend_from_slice(&(recv_ts as u64).to_le_bytes());
}
