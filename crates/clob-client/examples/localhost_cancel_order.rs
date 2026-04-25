//! CancelOrder happy path.
//! Spec: Required API Surface > Order entry > CancelOrder.
//!
//! Asserts: ExecReport{Cancelled, UserRequested} after sending a
//! Cancel for a previously rested order.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{CancelReason, ExecState, Side, Tif};
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
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send maker");
    let _ = client
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await;

    client
        .send(&Inbound::CancelOrder(common::cancel(id, 7)))
        .await
        .expect("send cancel");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let cancelled = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == id
                    && matches!(r.state, ExecState::Cancelled)
                    && r.cancel_reason == Some(CancelReason::UserRequested)
        )
    });
    if !cancelled {
        eprintln!("FAIL: no Cancelled{{UserRequested}} for {id}");
        std::process::exit(1);
    }
    println!("OK: CancelOrder produced Cancelled{{UserRequested}}");
}
