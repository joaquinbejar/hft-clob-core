//! SnapshotRequest → SnapshotResponse round-trip.
//! Spec: Required API Surface > Control plane > Snapshot.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Populate a small book.
    for &(side, price, qty) in &[
        (Side::Bid, 99, 5),
        (Side::Bid, 100, 3),
        (Side::Ask, 101, 4),
        (Side::Ask, 102, 6),
    ] {
        client
            .send(&Inbound::NewOrder(common::limit(
                common::next_order_id(),
                7,
                side,
                price,
                qty,
                Tif::Gtc,
            )))
            .await
            .expect("send");
    }
    let _ = client
        .recv_until_timeout(Duration::from_millis(400), 64)
        .await;

    let req_id = 7;
    client
        .send(&Inbound::SnapshotRequest(common::snapshot_request(req_id)))
        .await
        .expect("send snapshot req");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 64)
        .await
        .expect("recv");

    let snap = events.iter().find_map(|o| match o {
        Outbound::SnapshotResponse(s) => Some(s),
        _ => None,
    });
    let snap = match snap {
        Some(s) => s,
        None => {
            eprintln!("FAIL: no SnapshotResponse received");
            std::process::exit(1);
        }
    };

    if snap.request_id != req_id {
        eprintln!(
            "FAIL: snapshot request_id mismatch — sent {req_id}, got {}",
            snap.request_id
        );
        std::process::exit(1);
    }
    if snap.bids.is_empty() || snap.asks.is_empty() {
        eprintln!("FAIL: snapshot empty (expected at least 2 levels per side)");
        std::process::exit(1);
    }
    println!(
        "OK: SnapshotResponse with request_id={} bids={} asks={}",
        snap.request_id,
        snap.bids.len(),
        snap.asks.len()
    );
}
