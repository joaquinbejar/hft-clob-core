//! Strict monotonicity of `engine_seq` across all outbound streams
//! interleaved (`ExecReport` / `TradePrint` / `BookUpdateTop` /
//! `BookUpdateL2Delta`). Spec: Invariants to Enforce.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

fn engine_seq(o: &Outbound) -> u64 {
    match o {
        Outbound::ExecReport(r) => r.engine_seq.as_raw(),
        Outbound::TradePrint(t) => t.engine_seq.as_raw(),
        Outbound::BookUpdateTop(b) => b.engine_seq.as_raw(),
        Outbound::BookUpdateL2Delta(d) => d.engine_seq.as_raw(),
        Outbound::SnapshotResponse(s) => s.engine_seq.as_raw(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;
    // Drain pre-existing events.
    let _ = client
        .recv_until_timeout(Duration::from_millis(150), 64)
        .await;

    // A small sequence: rest, cross, cancel.
    client
        .send(&Inbound::NewOrder(common::limit(
            common::next_order_id(),
            2,
            Side::Ask,
            100,
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send");
    let id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            id,
            7,
            Side::Bid,
            100,
            3,
            Tif::Gtc,
        )))
        .await
        .expect("send");
    client
        .send(&Inbound::CancelOrder(common::cancel(id, 7)))
        .await
        .expect("cancel");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 64)
        .await
        .expect("recv");

    let mut last = 0u64;
    for ev in &events {
        let seq = engine_seq(ev);
        if seq <= last {
            eprintln!(
                "FAIL: engine_seq not strictly monotonic — prev={last} curr={seq} for {ev:?}"
            );
            std::process::exit(1);
        }
        last = seq;
    }
    println!(
        "OK: {} outbound events observed; engine_seq strictly monotonic (max={last})",
        events.len()
    );
}
