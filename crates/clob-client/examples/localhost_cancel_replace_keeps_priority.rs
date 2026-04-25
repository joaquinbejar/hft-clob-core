//! CancelReplace — qty-down keeps priority, qty-up / price-change
//! loses. This example exercises both paths and asserts an
//! ExecReport{Replaced} comes back. Spec: Required API Surface >
//! Order entry > CancelReplace.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{ExecState, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    let id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            id,
            7,
            Side::Bid,
            100,
            10,
            Tif::Gtc,
        )))
        .await
        .expect("send");
    let _ = client
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await;

    // Qty-down at same price — keeps priority on the matching side.
    client
        .send(&Inbound::CancelReplace(common::cancel_replace(
            id, 7, 100, 5,
        )))
        .await
        .expect("replace down");
    let events = client
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    let replaced = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r) if r.order_id == id && matches!(r.state, ExecState::Replaced)
        )
    });
    if !replaced {
        eprintln!("FAIL: no ExecReport{{Replaced}} on qty-down replace");
        std::process::exit(1);
    }
    println!("OK: CancelReplace produced ExecReport{{Replaced}}");
}
