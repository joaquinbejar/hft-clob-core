//! End-to-end test: TCP client → gateway → engine → ChannelSink →
//! gateway writer → TCP client. Verifies #43's contract: a
//! connected client can submit a `NewOrder` and read back the
//! corresponding `ExecReport{Accepted}` + `BookUpdateTop` on the
//! same socket.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};
use engine::{CounterIdGenerator, Engine, StubClock};
use gateway::{ChannelSink, handle_connection};
use marketdata::encoder;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc as tmpsc;
use wire::framing::{Frame, MessageKind};
use wire::inbound::{Inbound, NewOrder, new_order};
use wire::outbound::Outbound;

#[tokio::test]
async fn end_to_end_client_round_trip() {
    // Bind a fresh listener on an ephemeral port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");

    let (sync_tx, sync_rx) = mpsc::channel::<Inbound>();
    let (tokio_tx, mut tokio_rx) = tmpsc::channel::<Inbound>(64);

    // Forwarder: tokio mpsc → sync mpsc.
    tokio::spawn(async move {
        while let Some(msg) = tokio_rx.recv().await {
            if sync_tx.send(msg).is_err() {
                return;
            }
        }
    });

    // Outbound sink: shared between engine (producer) and listener
    // (subscribe-only via Arc).
    let sink = ChannelSink::new(64);
    let listener_sink = Arc::new(sink.clone());

    // Engine thread.
    thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut engine =
                Engine::new(StubClock::new(1_000_000), CounterIdGenerator::new(), sink);
            while let Ok(msg) = sync_rx.recv() {
                engine.step(msg);
            }
        })
        .expect("engine thread");

    // Accept loop driven directly so we own lifetime cleanly and
    // avoid double-binding the listener.
    tokio::spawn(async move {
        loop {
            let (sock, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            let rx = listener_sink.subscribe();
            let tx_clone = tokio_tx.clone();
            tokio::spawn(async move {
                handle_connection(sock, peer, tx_clone, rx, gateway::DEFAULT_READ_BUFFER).await;
            });
        }
    });

    // Give the listener a moment to bind & subscribe.
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Client connects.
    let mut client = TcpStream::connect(addr).await.expect("connect");
    // The listener subscribes per-connection, but the subscribe
    // happens after `accept` returns. Sleep briefly so the
    // subscribe registers before our first send triggers an emit.
    tokio::time::sleep(Duration::from_millis(80)).await;

    // Send a NewOrder framed.
    let msg = NewOrder {
        client_ts: ClientTs::new(0),
        order_id: OrderId::new(42).expect("ok"),
        account_id: AccountId::new(7).expect("ok"),
        side: Side::Bid,
        order_type: OrderType::Limit,
        tif: Tif::Gtc,
        price: Some(Price::new(100).expect("ok")),
        qty: Qty::new(5).expect("ok"),
    };
    let mut payload = Vec::new();
    new_order::encode(&msg, &mut payload);
    let mut frame = Vec::new();
    Frame::write(MessageKind::NewOrder, &payload, &mut frame).expect("frame");
    client.write_all(&frame).await.expect("write");

    // Read at least one ExecReport + one BookUpdateTop. Reading
    // until we have decoded both is the cleanest way to span the
    // fact that the engine may emit more frames over time.
    let mut buf: Vec<u8> = Vec::new();
    let mut got_exec = false;
    let mut got_top = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline && !(got_exec && got_top) {
        let mut tmp = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_millis(500), client.read(&mut tmp))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        // Drain whatever frames we can.
        while let Ok((decoded, total)) = encoder::decode(&buf) {
            buf.drain(..total);
            match decoded {
                Outbound::ExecReport(r) => {
                    assert_eq!(r.order_id, OrderId::new(42).expect("ok"));
                    assert_eq!(r.account_id, AccountId::new(7).expect("ok"));
                    got_exec = true;
                }
                Outbound::BookUpdateTop(_) => {
                    got_top = true;
                }
                _ => {}
            }
        }
    }
    assert!(got_exec, "expected ExecReport on the same TCP connection");
    assert!(got_top, "expected BookUpdateTop on the same TCP connection");

    drop(client);
}
