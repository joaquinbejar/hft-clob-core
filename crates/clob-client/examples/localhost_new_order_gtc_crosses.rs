//! NewOrder limit / GTC, crosses spread — generates TradePrint and
//! Filled exec reports on both sides. Spec: Required API Surface >
//! Order entry > NewOrder.
//!
//! Asserts: at least one TradePrint + at least one fill exec report
//! arrive on the connection.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{ExecState, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Resting ask at 100, qty 5.
    let maker_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            maker_id,
            2,
            Side::Ask,
            100,
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send maker");
    // Drain.
    let _ = client
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await;

    // Aggressive bid at 100, qty 5 — full cross.
    let taker_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            taker_id,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send taker");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let trade_print = events.iter().any(|o| matches!(o, Outbound::TradePrint(_)));
    let filled = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r) if matches!(r.state, ExecState::Filled)
        )
    });

    if !trade_print {
        eprintln!("FAIL: no TradePrint on cross");
        std::process::exit(1);
    }
    if !filled {
        eprintln!("FAIL: no ExecReport{{Filled}} on cross");
        std::process::exit(1);
    }
    println!("OK: NewOrder GTC cross emitted TradePrint and Filled exec report");
}
