//! MassCancel — every resting order for one account flips to
//! Cancelled{MassCancel}. Spec: Required API Surface > Order entry >
//! MassCancel.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{CancelReason, ExecState, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    let id_a = common::next_order_id();
    let id_b = common::next_order_id();
    let id_c = common::next_order_id();
    for &(id, side, price) in &[
        (id_a, Side::Bid, 99),
        (id_b, Side::Bid, 98),
        (id_c, Side::Ask, 110),
    ] {
        client
            .send(&Inbound::NewOrder(common::limit(
                id,
                7,
                side,
                price,
                3,
                Tif::Gtc,
            )))
            .await
            .expect("send");
    }
    let _ = client
        .recv_until_timeout(Duration::from_millis(400), 64)
        .await;

    client
        .send(&Inbound::MassCancel(common::mass_cancel(7)))
        .await
        .expect("mass cancel");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 64)
        .await
        .expect("recv");

    let cancelled_count = events
        .iter()
        .filter(|o| {
            matches!(
                o,
                Outbound::ExecReport(r)
                    if matches!(r.state, ExecState::Cancelled)
                        && r.cancel_reason == Some(CancelReason::MassCancel)
            )
        })
        .count();

    if cancelled_count < 3 {
        eprintln!("FAIL: expected ≥3 Cancelled{{MassCancel}} for account 7, got {cancelled_count}");
        std::process::exit(1);
    }
    println!("OK: MassCancel produced {cancelled_count} Cancelled{{MassCancel}} events");
}
