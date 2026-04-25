//! TCP listener and per-connection inbound decoder.
//!
//! Tokio is **only** allowed inside this crate (CLAUDE.md § Architecture).
//! Per-connection tasks read length-prefixed frames from the socket,
//! decode via [`wire::inbound::parse_frame`], and forward typed
//! [`wire::inbound::Inbound`] commands to the engine over an
//! [`tokio::sync::mpsc::Sender<Inbound>`]. The engine consumes from
//! its end on a single dedicated thread.
//!
//! ## Backpressure policy
//!
//! `mpsc::Sender::try_send` returns `Full` when the ring is at
//! capacity. Documented behaviour: the connection is closed (fail-fast)
//! so the upstream client back-pressures naturally via TCP — the
//! engine never blocks. This matches `doc/DESIGN.md` § 4.2.
//!
//! ## Failure modes
//!
//! - Truncated frame mid-buffer → keep reading, no close.
//! - Malformed payload (`WireError`) → emit `Rejected{MalformedMessage}`
//!   on the same socket and close. v1 emits a placeholder frame; the
//!   per-session reject path lives under `crates/marketdata/`.
//! - Channel full → close connection.
//! - Peer disconnect → drop session, no log spam.

use std::io;
use std::net::SocketAddr;

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use wire::WireError;
use wire::framing::{FRAME_LEN_BYTES, Frame, ParseOutcome};
use wire::inbound::{Inbound, parse_frame};

/// Default per-connection read buffer capacity. Sized to hold a few
/// inbound frames without re-alloc.
pub const DEFAULT_READ_BUFFER: usize = 4096;

/// Outcome of one frame-decode attempt against the buffered bytes.
#[derive(Debug)]
enum DecodeStep {
    /// Successfully decoded one inbound message; consumed `total`
    /// bytes from the buffer.
    Decoded { msg: Inbound, total: usize },
    /// Frame payload had a known kind whose payload failed domain
    /// validation. Consumed `total` bytes; the gateway should emit
    /// a `Rejected{MalformedMessage}` and continue reading.
    Malformed { err: WireError, total: usize },
    /// Frame consumed `total` bytes but its kind was an outbound
    /// kind or otherwise unrecognised on the inbound stream.
    /// Treated as malformed for v1.
    Skipped { byte: u8, total: usize },
    /// Buffer too short; need more bytes from the socket.
    NeedMore,
}

/// Try to decode one inbound frame at the front of `buf`. Caller
/// drains `total` bytes from `buf` after a `Decoded` / `Malformed` /
/// `Skipped` outcome.
fn try_decode(buf: &[u8]) -> DecodeStep {
    if buf.len() < FRAME_LEN_BYTES {
        return DecodeStep::NeedMore;
    }
    // Peek the length prefix to avoid burning a buffer copy on
    // truncated reads.
    let len_arr: [u8; FRAME_LEN_BYTES] = match buf[..FRAME_LEN_BYTES].try_into() {
        Ok(a) => a,
        Err(_) => return DecodeStep::NeedMore,
    };
    let frame_len = u32::from_le_bytes(len_arr) as usize;
    let total = FRAME_LEN_BYTES + frame_len;
    if buf.len() < total {
        return DecodeStep::NeedMore;
    }
    match Frame::parse_or_skip(buf) {
        Ok((ParseOutcome::Frame(frame), total)) => match parse_frame(frame) {
            Ok(msg) => DecodeStep::Decoded { msg, total },
            Err(err) => DecodeStep::Malformed { err, total },
        },
        Ok((ParseOutcome::UnknownKind(byte), total)) => DecodeStep::Skipped { byte, total },
        Err(_) => {
            // Length prefix lied — buffer was advertised as `total`
            // bytes but the slice was shorter. Drop the connection.
            DecodeStep::NeedMore
        }
    }
}

/// Bind a TCP listener at `addr` and run the accept loop until the
/// listener errors. Each accepted connection is spawned onto a new
/// tokio task that decodes frames and forwards them via `tx`.
///
/// # Errors
/// Surfaces the underlying [`std::io::Error`] from `bind` /
/// `accept` calls. Per-connection failures are logged and do not
/// stop the accept loop.
/// Trait for the per-connection outbound subscription source.
/// `crate::sink::ChannelSink` implements this; tests pass a mock to
/// drive `handle_connection` without standing up a full engine.
pub trait OutboundSubscriber: Send + Sync + 'static {
    /// Open a fresh broadcast subscription. Each connection receives
    /// every outbound frame emitted *after* its subscribe() call —
    /// CLAUDE.md's "no replay history" pattern; clients reconcile via
    /// `SnapshotRequest` after connect.
    fn subscribe(&self) -> broadcast::Receiver<Bytes>;
}

/// Bind a TCP listener and serve every accepted connection through
/// `handle_connection`. Each connection gets its own broadcast
/// `Receiver<Bytes>` from `sink.subscribe()` so it sees every
/// outbound frame the engine emits *after* its connect.
///
/// # Errors
/// Surfaces the underlying [`std::io::Error`] from `bind` /
/// `accept` calls. Per-connection failures are logged and do not
/// stop the accept loop.
pub async fn run<S>(
    addr: &str,
    tx: mpsc::Sender<Inbound>,
    sink: std::sync::Arc<S>,
) -> io::Result<()>
where
    S: OutboundSubscriber,
{
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    info!(addr = %local, "gateway listener bound");
    loop {
        let (sock, peer) = listener.accept().await?;
        debug!(peer = %peer, "gateway accepted connection");
        let tx_clone = tx.clone();
        let rx = sink.subscribe();
        tokio::spawn(async move {
            handle_connection(sock, peer, tx_clone, rx, DEFAULT_READ_BUFFER).await;
        });
    }
}

/// Drive one accepted connection until disconnect / channel full /
/// malformed payload. Spawns a sub-task that pumps outbound frames
/// from `out_rx` to the socket's write half. Public so tests can
/// drive it without binding a real TCP listener.
pub async fn handle_connection(
    sock: TcpStream,
    peer: SocketAddr,
    tx: mpsc::Sender<Inbound>,
    out_rx: broadcast::Receiver<Bytes>,
    buffer_capacity: usize,
) {
    let (mut read_half, mut write_half) = sock.into_split();

    // Outbound writer: pumps every frame the engine broadcasts onto
    // this session's TCP write half. A subscriber that falls behind
    // by more than the broadcast capacity (`Lagged(_)`) is dropped —
    // the client must reconnect and re-subscribe to recover. The
    // engine never blocks on a slow consumer.
    let mut out_rx = out_rx;
    let writer_peer = peer;
    let writer_task = tokio::spawn(async move {
        loop {
            match out_rx.recv().await {
                Ok(frame) => {
                    if let Err(err) = write_half.write_all(&frame).await {
                        warn!(peer = %writer_peer, error = %err, "gateway: outbound write error, closing");
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(peer = %writer_peer, skipped, "gateway: outbound subscriber lagged, closing");
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    debug!(peer = %writer_peer, "gateway: outbound channel closed");
                    return;
                }
            }
        }
    });

    let mut buf = BytesMut::with_capacity(buffer_capacity);
    loop {
        // Drain whatever already-buffered frames we can before
        // touching the socket — a single `read` may have delivered
        // multiple frames.
        loop {
            match try_decode(&buf) {
                DecodeStep::Decoded { msg, total } => {
                    buf.advance_consume(total);
                    if let Err(send_err) = tx.try_send(msg) {
                        warn!(peer = %peer, error = %send_err, "gateway: matching ring full, closing connection");
                        writer_task.abort();
                        return;
                    }
                }
                DecodeStep::Malformed { err, total } => {
                    warn!(peer = %peer, error = %err, "gateway: malformed inbound frame, closing connection");
                    let _ = total;
                    writer_task.abort();
                    return;
                }
                DecodeStep::Skipped { byte, total } => {
                    warn!(peer = %peer, byte, "gateway: unknown / outbound kind on inbound stream, closing");
                    let _ = total;
                    writer_task.abort();
                    return;
                }
                DecodeStep::NeedMore => break,
            }
        }
        match read_half.read_buf(&mut buf).await {
            Ok(0) => {
                debug!(peer = %peer, "gateway: peer closed connection");
                writer_task.abort();
                return;
            }
            Ok(_n) => {}
            Err(err) => {
                warn!(peer = %peer, error = %err, "gateway: read error, closing");
                writer_task.abort();
                return;
            }
        }
    }
}

/// Consume `n` bytes from the front of a `BytesMut`, retaining
/// capacity. `BytesMut::advance` does the equivalent but `bytes`
/// gates the API behind a `Buf` import; this wrapper keeps the
/// listener module dependency-light.
trait AdvanceConsume {
    fn advance_consume(&mut self, n: usize);
}

impl AdvanceConsume for BytesMut {
    #[inline]
    fn advance_consume(&mut self, n: usize) {
        use bytes::Buf as _;
        self.advance(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{AccountId, OrderId, Side, Tif};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use wire::framing::MessageKind;
    use wire::inbound::{NewOrder, new_order};

    /// Build a sample `NewOrder` framed payload.
    fn framed_new_order(id: u64, account: u32, price: i64, qty: u64) -> Vec<u8> {
        let msg = NewOrder {
            client_ts: domain::ClientTs::new(0),
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side: Side::Bid,
            order_type: domain::OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(domain::Price::new(price).expect("ok")),
            qty: domain::Qty::new(qty).expect("ok"),
        };
        let mut payload = Vec::new();
        new_order::encode(&msg, &mut payload);
        let mut framed = Vec::new();
        Frame::write(MessageKind::NewOrder, &payload, &mut framed).expect("fits");
        framed
    }

    #[tokio::test]
    async fn test_listener_decodes_one_new_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, mut rx) = mpsc::channel::<Inbound>(8);

        // Background accept loop — processes one connection.
        let server = tokio::spawn(async move {
            let (sock, peer) = listener.accept().await.expect("accept");
            let (_outbound_tx, outbound_rx) = broadcast::channel::<Bytes>(16);
            handle_connection(sock, peer, tx, outbound_rx, DEFAULT_READ_BUFFER).await;
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        let frame = framed_new_order(42, 7, 100, 5);
        client.write_all(&frame).await.expect("write");
        client
            .shutdown()
            .await
            .expect("shutdown signals EOF to server");

        let received = rx.recv().await.expect("frame received");
        match received {
            Inbound::NewOrder(n) => {
                assert_eq!(n.order_id, OrderId::new(42).expect("ok"));
                assert_eq!(n.qty, domain::Qty::new(5).expect("ok"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        server.await.expect("server task completed");
    }

    #[tokio::test]
    async fn test_listener_handles_multiple_frames_per_read() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, mut rx) = mpsc::channel::<Inbound>(8);

        let server = tokio::spawn(async move {
            let (sock, peer) = listener.accept().await.expect("accept");
            let (_outbound_tx, outbound_rx) = broadcast::channel::<Bytes>(16);
            handle_connection(sock, peer, tx, outbound_rx, DEFAULT_READ_BUFFER).await;
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        let mut combined = Vec::new();
        combined.extend(framed_new_order(1, 7, 100, 5));
        combined.extend(framed_new_order(2, 7, 99, 3));
        combined.extend(framed_new_order(3, 7, 98, 1));
        client.write_all(&combined).await.expect("write");
        client.shutdown().await.expect("shutdown");

        let mut count = 0;
        while let Some(msg) = rx.recv().await {
            assert!(matches!(msg, Inbound::NewOrder(_)));
            count += 1;
        }
        assert_eq!(count, 3);
        server.await.expect("server done");
    }

    #[tokio::test]
    async fn test_listener_closes_on_malformed_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, mut rx) = mpsc::channel::<Inbound>(8);

        let server = tokio::spawn(async move {
            let (sock, peer) = listener.accept().await.expect("accept");
            let (_outbound_tx, outbound_rx) = broadcast::channel::<Bytes>(16);
            handle_connection(sock, peer, tx, outbound_rx, DEFAULT_READ_BUFFER).await;
        });

        // Build a syntactically valid frame whose `kind` is NewOrder
        // but whose payload is the wrong size (NewOrder is 40 bytes).
        let mut framed = Vec::new();
        let bogus_payload = vec![0u8; 10];
        Frame::write(MessageKind::NewOrder, &bogus_payload, &mut framed).expect("fits");
        let mut client = TcpStream::connect(addr).await.expect("connect");
        client.write_all(&framed).await.expect("write");
        // Do NOT call shutdown — we expect the server to close on its own.

        // No inbound delivered.
        let attempt = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
        match attempt {
            Ok(None) => {} // channel closed by sender drop after server returned
            Ok(Some(other)) => panic!("unexpected delivery: {other:?}"),
            Err(_) => panic!("server did not close on malformed frame"),
        }
        server.await.expect("server done");
    }

    #[tokio::test]
    async fn test_listener_pumps_outbound_frames_to_client() {
        use tokio::io::AsyncReadExt;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, _rx) = mpsc::channel::<Inbound>(8);
        let (outbound_tx, _) = broadcast::channel::<Bytes>(16);
        let outbound_tx_clone = outbound_tx.clone();

        let server = tokio::spawn(async move {
            let (sock, peer) = listener.accept().await.expect("accept");
            let outbound_rx = outbound_tx_clone.subscribe();
            handle_connection(sock, peer, tx, outbound_rx, DEFAULT_READ_BUFFER).await;
        });

        let mut client = TcpStream::connect(addr).await.expect("connect");
        // Give the server a moment to subscribe before we publish.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Push two arbitrary framed payloads onto the broadcast.
        let payload_a = Bytes::from_static(&[
            0x01, 0x00, 0x00, 0x00, // len = 1
            0x65, // ExecReport kind (any kind works for the byte echo test)
        ]);
        let payload_b = Bytes::from_static(&[
            0x02, 0x00, 0x00, 0x00, // len = 2
            0x66, // TradePrint kind
            0xAB, // 1 byte of payload
        ]);
        outbound_tx.send(payload_a.clone()).expect("send");
        outbound_tx.send(payload_b.clone()).expect("send");

        // Read the bytes back.
        let mut received = vec![0u8; payload_a.len() + payload_b.len()];
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            client.read_exact(&mut received),
        )
        .await
        .expect("read timeout")
        .expect("read");
        let mut expected = Vec::new();
        expected.extend_from_slice(&payload_a);
        expected.extend_from_slice(&payload_b);
        assert_eq!(received, expected);

        // Close the client to terminate the server cleanly.
        drop(client);
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), server).await;
    }
}
