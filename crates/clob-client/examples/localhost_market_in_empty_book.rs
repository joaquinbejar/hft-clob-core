//! Market order against an empty opposite side — rejected with
//! MarketOrderInEmptyBook. Spec: Required API Surface > Order entry.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{ExecState, RejectReason, Side};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Drain any prior state — we depend on the bid side being empty.
    let _ = client
        .recv_until_timeout(Duration::from_millis(100), 64)
        .await;

    let id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::market(id, 9, Side::Ask, 5)))
        .await
        .expect("send market");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let rejected = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == id
                    && matches!(r.state, ExecState::Rejected)
                    && r.reject_reason == Some(RejectReason::MarketOrderInEmptyBook)
        )
    });
    if !rejected {
        eprintln!(
            "FAIL: market order in empty book not rejected; got {} events. \
             Note: this example assumes a fresh engine with no resting bids on the contra side.",
            events.len()
        );
        std::process::exit(1);
    }
    println!("OK: market order in empty book rejected with MarketOrderInEmptyBook");
}
