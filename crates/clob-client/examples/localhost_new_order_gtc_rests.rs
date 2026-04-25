//! NewOrder limit / GTC, no immediate cross — rests on the book.
//! Spec: Required API Surface > Order entry > NewOrder.
//!
//! Asserts: ExecReport{Accepted} + BookUpdateTop arrive on the same
//! TCP connection.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use clob_client::DEFAULT_RECV_TIMEOUT;
use domain::{ExecState, Side, Tif};
use wire::inbound::Inbound;
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    let id = common::next_order_id();
    let order = common::limit(id, 7, Side::Bid, 100, 5, Tif::Gtc);
    client
        .send(&Inbound::NewOrder(order))
        .await
        .expect("send NewOrder");

    let events = client
        .recv_until_timeout(Duration::from_millis(800), 16)
        .await
        .expect("recv");

    let accepted = events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if matches!(r.state, ExecState::Accepted) && r.order_id == id
        )
    });
    let tob = events
        .iter()
        .any(|o| matches!(o, Outbound::BookUpdateTop(_)));

    if !accepted {
        eprintln!("FAIL: ExecReport{{Accepted}} not observed for order {id}");
        std::process::exit(1);
    }
    if !tob {
        eprintln!("FAIL: BookUpdateTop not observed");
        std::process::exit(1);
    }
    println!("OK: NewOrder GTC rests; ExecReport+BookUpdateTop received");
    let _ = DEFAULT_RECV_TIMEOUT; // touch the export
}
