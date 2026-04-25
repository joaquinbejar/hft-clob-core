//! NewOrder limit / IOC, partial-fill-then-cancel.
//! Spec: Required API Surface > Order entry > NewOrder + TIF behavior.
//!
//! Asserts: ExecReport{PartiallyFilled} + ExecReport{Cancelled,
//! IocRemaining} for the IOC remainder.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{CancelReason, ExecState, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Resting ask at 100, qty 3.
    client
        .send(&Inbound::NewOrder(common::limit(
            common::next_order_id(),
            2,
            Side::Ask,
            100,
            3,
            Tif::Gtc,
        )))
        .await
        .expect("send maker");
    let _ = client
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await;

    // IOC bid at 100, qty 10 — partial fill 3, remainder 7 cancelled.
    let taker_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            taker_id,
            7,
            Side::Bid,
            100,
            10,
            Tif::Ioc,
        )))
        .await
        .expect("send taker");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let partial = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == taker_id && matches!(r.state, ExecState::PartiallyFilled)
        )
    });
    let cancelled = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == taker_id
                    && matches!(r.state, ExecState::Cancelled)
                    && r.cancel_reason == Some(CancelReason::IocRemaining)
        )
    });

    if !partial {
        eprintln!("FAIL: no PartiallyFilled for IOC taker");
        std::process::exit(1);
    }
    if !cancelled {
        eprintln!("FAIL: no Cancelled{{IocRemaining}} for IOC taker remainder");
        std::process::exit(1);
    }
    println!("OK: IOC partial fill + remainder cancellation observed");
}
