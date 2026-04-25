//! NewOrder limit / PostOnly, would-cross rejection.
//! Spec: Required API Surface > Order entry > PostOnly TIF.
//!
//! Asserts: ExecReport{Rejected, PostOnlyWouldCross} when a
//! post-only would cross at arrival.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{ExecState, RejectReason, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Resting ask at 100.
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
        .expect("send maker");
    let _ = client
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await;

    // PostOnly bid at 100 would cross — must be rejected.
    let post_only_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            post_only_id,
            7,
            Side::Bid,
            100,
            5,
            Tif::PostOnly,
        )))
        .await
        .expect("send post-only");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let rejected = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == post_only_id
                    && matches!(r.state, ExecState::Rejected)
                    && r.reject_reason == Some(RejectReason::PostOnlyWouldCross)
        )
    });
    if !rejected {
        eprintln!("FAIL: PostOnly crossing order not rejected");
        std::process::exit(1);
    }
    println!("OK: PostOnly would-cross rejected with PostOnlyWouldCross");
}
