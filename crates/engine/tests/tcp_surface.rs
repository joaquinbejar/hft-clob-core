//! Integration tests driving the public TCP surface end-to-end.
//!
//! Spins up an engine + tokio listener on an ephemeral port, then
//! exercises one path per spec bullet via `clob_client::Client`.
//! Mirrors the `examples/localhost/*` binaries one-for-one so the
//! same code path survives both `cargo nextest run` and
//! `make smoke-test`.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use clob_client::Client;
use domain::{
    AccountId, CancelReason, ClientTs, ExecState, OrderId, OrderType, Price, Qty, RejectReason,
    Side, Tif,
};
use engine::{CounterIdGenerator, Engine, StubClock};
use gateway::{ChannelSink, handle_connection};
use tokio::net::TcpListener;
use tokio::sync::mpsc as tmpsc;
use wire::inbound::{
    CancelOrder, Inbound, KillSwitchSet, KillSwitchState, MassCancel, NewOrder, SnapshotRequest,
};
use wire::outbound::Outbound;

/// Spin up an engine listener on `127.0.0.1:0`, return its bound
/// address. The engine + listener tasks live on the test's tokio
/// runtime; the engine matching thread is a real OS thread.
async fn spawn_engine() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");

    let (sync_tx, sync_rx) = mpsc::channel::<Inbound>();
    let (tokio_tx, mut tokio_rx) = tmpsc::channel::<Inbound>(1024);

    tokio::spawn(async move {
        while let Some(msg) = tokio_rx.recv().await {
            if sync_tx.send(msg).is_err() {
                return;
            }
        }
    });

    let sink = ChannelSink::new(1024);
    let listener_sink = Arc::new(sink.clone());

    thread::Builder::new()
        .name("engine-test".into())
        .spawn(move || {
            let mut engine =
                Engine::new(StubClock::new(1_000_000), CounterIdGenerator::new(), sink);
            while let Ok(msg) = sync_rx.recv() {
                engine.step(msg);
            }
        })
        .expect("engine thread");

    tokio::spawn(async move {
        loop {
            let (sock, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let rx = listener_sink.subscribe();
            let tx = tokio_tx.clone();
            tokio::spawn(async move {
                handle_connection(sock, peer, tx, rx, gateway::DEFAULT_READ_BUFFER).await;
            });
        }
    });

    // Brief delay so the listener loop is in `accept().await`.
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

fn limit_order(id: u64, account: u32, side: Side, price: i64, qty: u64, tif: Tif) -> NewOrder {
    NewOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(id).expect("ok"),
        account_id: AccountId::new(account).expect("ok"),
        side,
        order_type: OrderType::Limit,
        tif,
        price: Some(Price::new(price).expect("ok")),
        qty: Qty::new(qty).expect("ok"),
    }
}

async fn fresh_client() -> (Client, std::net::SocketAddr) {
    let addr = spawn_engine().await;
    let client = Client::connect(&addr.to_string()).await.expect("connect");
    // Brief delay so the per-session subscribe lands before any emit.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (client, addr)
}

#[tokio::test]
async fn tcp_surface_new_order_gtc_rests() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("send");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    assert!(events.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r) if matches!(r.state, ExecState::Accepted)
    )));
    assert!(
        events
            .iter()
            .any(|o| matches!(o, Outbound::BookUpdateTop(_)))
    );
}

#[tokio::test]
async fn tcp_surface_new_order_gtc_crosses() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        2,
        Side::Ask,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("maker");
    let _ = c.recv_until_timeout(Duration::from_millis(200), 16).await;
    c.send(&Inbound::NewOrder(limit_order(
        2,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("taker");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    assert!(events.iter().any(|o| matches!(o, Outbound::TradePrint(_))));
    assert!(events.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r) if matches!(r.state, ExecState::Filled)
    )));
}

#[tokio::test]
async fn tcp_surface_new_order_ioc_partial() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        2,
        Side::Ask,
        100,
        3,
        Tif::Gtc,
    )))
    .await
    .expect("maker");
    let _ = c.recv_until_timeout(Duration::from_millis(200), 16).await;
    c.send(&Inbound::NewOrder(limit_order(
        2,
        7,
        Side::Bid,
        100,
        10,
        Tif::Ioc,
    )))
    .await
    .expect("ioc");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    assert!(events.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r)
            if r.order_id == OrderId::new(2).expect("ok")
                && matches!(r.state, ExecState::Cancelled)
                && r.cancel_reason == Some(CancelReason::IocRemaining)
    )));
}

#[tokio::test]
async fn tcp_surface_post_only_would_cross() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        2,
        Side::Ask,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("maker");
    let _ = c.recv_until_timeout(Duration::from_millis(200), 16).await;
    c.send(&Inbound::NewOrder(limit_order(
        2,
        7,
        Side::Bid,
        100,
        5,
        Tif::PostOnly,
    )))
    .await
    .expect("post-only");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    assert!(events.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r)
            if matches!(r.state, ExecState::Rejected)
                && r.reject_reason == Some(RejectReason::PostOnlyWouldCross)
    )));
}

#[tokio::test]
async fn tcp_surface_cancel_order_user_requested() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("send");
    let _ = c.recv_until_timeout(Duration::from_millis(200), 16).await;
    c.send(&Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(7).expect("ok"),
    }))
    .await
    .expect("cancel");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 16)
        .await
        .expect("recv");
    assert!(events.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r)
            if matches!(r.state, ExecState::Cancelled)
                && r.cancel_reason == Some(CancelReason::UserRequested)
    )));
}

#[tokio::test]
async fn tcp_surface_mass_cancel() {
    let (mut c, _) = fresh_client().await;
    for &(id, side, price) in &[
        (1u64, Side::Bid, 99i64),
        (2, Side::Bid, 98),
        (3, Side::Ask, 110),
    ] {
        c.send(&Inbound::NewOrder(limit_order(
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
    let _ = c.recv_until_timeout(Duration::from_millis(300), 64).await;
    c.send(&Inbound::MassCancel(MassCancel {
        client_ts: ClientTs::new(0),
        account_id: AccountId::new(7).expect("ok"),
    }))
    .await
    .expect("mass");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 64)
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
    assert!(cancelled_count >= 3);
}

#[tokio::test]
async fn tcp_surface_kill_switch_halt_resume() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::KillSwitchSet(KillSwitchSet {
        client_ts: ClientTs::new(0),
        state: KillSwitchState::Halt,
        admin_token: 0,
    }))
    .await
    .expect("halt");
    let _ = c.recv_until_timeout(Duration::from_millis(100), 16).await;

    c.send(&Inbound::NewOrder(limit_order(
        1,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("rejected");
    let halted = c
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await
        .expect("recv");
    assert!(halted.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r)
            if matches!(r.state, ExecState::Rejected)
                && r.reject_reason == Some(RejectReason::KillSwitched)
    )));

    c.send(&Inbound::KillSwitchSet(KillSwitchSet {
        client_ts: ClientTs::new(0),
        state: KillSwitchState::Resume,
        admin_token: 0,
    }))
    .await
    .expect("resume");
    let _ = c.recv_until_timeout(Duration::from_millis(100), 16).await;

    c.send(&Inbound::NewOrder(limit_order(
        2,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("send");
    let resumed = c
        .recv_until_timeout(Duration::from_millis(300), 16)
        .await
        .expect("recv");
    assert!(resumed.iter().any(|o| matches!(
        o,
        Outbound::ExecReport(r) if matches!(r.state, ExecState::Accepted)
    )));
}

#[tokio::test]
async fn tcp_surface_snapshot_request_round_trip() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        7,
        Side::Bid,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("rest");
    c.send(&Inbound::NewOrder(limit_order(
        2,
        2,
        Side::Ask,
        101,
        3,
        Tif::Gtc,
    )))
    .await
    .expect("rest ask");
    let _ = c.recv_until_timeout(Duration::from_millis(200), 64).await;

    c.send(&Inbound::SnapshotRequest(SnapshotRequest {
        request_id: 42,
    }))
    .await
    .expect("snap");
    let events = c
        .recv_until_timeout(Duration::from_millis(500), 64)
        .await
        .expect("recv");
    let snap = events.iter().find_map(|o| match o {
        Outbound::SnapshotResponse(s) => Some(s),
        _ => None,
    });
    let snap = snap.expect("SnapshotResponse received");
    assert_eq!(snap.request_id, 42);
    assert!(!snap.bids.is_empty());
    assert!(!snap.asks.is_empty());
}

#[tokio::test]
async fn tcp_surface_engine_seq_strictly_monotonic() {
    let (mut c, _) = fresh_client().await;
    c.send(&Inbound::NewOrder(limit_order(
        1,
        2,
        Side::Ask,
        100,
        5,
        Tif::Gtc,
    )))
    .await
    .expect("send");
    c.send(&Inbound::NewOrder(limit_order(
        2,
        7,
        Side::Bid,
        100,
        3,
        Tif::Gtc,
    )))
    .await
    .expect("send");
    c.send(&Inbound::CancelOrder(CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(1).expect("ok"),
        account_id: AccountId::new(2).expect("ok"),
    }))
    .await
    .expect("cancel");

    let events = c
        .recv_until_timeout(Duration::from_millis(500), 64)
        .await
        .expect("recv");
    let mut last = 0u64;
    for ev in &events {
        let seq = match ev {
            Outbound::ExecReport(r) => r.engine_seq.as_raw(),
            Outbound::TradePrint(t) => t.engine_seq.as_raw(),
            Outbound::BookUpdateTop(b) => b.engine_seq.as_raw(),
            Outbound::BookUpdateL2Delta(d) => d.engine_seq.as_raw(),
            Outbound::SnapshotResponse(s) => s.engine_seq.as_raw(),
        };
        assert!(seq > last, "engine_seq must be strictly monotonic");
        last = seq;
    }
}
