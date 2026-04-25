//! KillSwitchSet: halt → reject → resume → accept cycle.
//! Spec: Required API Surface > Control plane > Kill switch.

#[path = "_common.rs"]
mod common;

use std::time::Duration;

use domain::{ExecState, RejectReason, Side, Tif};
use wire::inbound::{Inbound, KillSwitchState};
use wire::outbound::Outbound;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut client = common::connect().await;

    // Halt the engine.
    client
        .send(&Inbound::KillSwitchSet(common::kill_switch(
            KillSwitchState::Halt,
        )))
        .await
        .expect("halt");
    let _ = client
        .recv_until_timeout(Duration::from_millis(200), 8)
        .await;

    // Submit an order — must be rejected with KillSwitched.
    let halted_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            halted_id,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send halted");
    let halted_events = client
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    let killed = halted_events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r)
                if r.order_id == halted_id
                    && matches!(r.state, ExecState::Rejected)
                    && r.reject_reason == Some(RejectReason::KillSwitched)
        )
    });
    if !killed {
        eprintln!("FAIL: kill switch did not reject the order with KillSwitched");
        std::process::exit(1);
    }

    // Resume.
    client
        .send(&Inbound::KillSwitchSet(common::kill_switch(
            KillSwitchState::Resume,
        )))
        .await
        .expect("resume");
    let _ = client
        .recv_until_timeout(Duration::from_millis(200), 8)
        .await;

    // Same shape now accepted.
    let resumed_id = common::next_order_id();
    client
        .send(&Inbound::NewOrder(common::limit(
            resumed_id,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )))
        .await
        .expect("send resumed");
    let resumed_events = client
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    let accepted = resumed_events.iter().any(|o| {
        matches!(
            o,
            Outbound::ExecReport(r) if r.order_id == resumed_id && matches!(r.state, ExecState::Accepted)
        )
    });
    if !accepted {
        eprintln!("FAIL: order not accepted after kill switch resume");
        std::process::exit(1);
    }
    println!("OK: kill switch halt-rejects then resume-accepts");
}
