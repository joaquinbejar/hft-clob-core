//! Engine binary. Tokio entry point that owns the gateway listener,
//! a dedicated engine thread, and a `ChannelSink` broadcast that
//! every accepted TCP session subscribes to.
//!
//! ## CLI
//!
//! ```text
//! engine [bind_addr]
//! ```
//!
//! Default bind address `0.0.0.0:9000`. Connected clients can
//! submit framed inbound messages and read framed outbound bytes
//! (`ExecReport` / `TradePrint` / `BookUpdateTop` / `SnapshotResponse`)
//! on the same TCP connection.
//!
//! ## Backpressure
//!
//! - Inbound `tokio::sync::mpsc` is bounded at 8192. `try_send` Full
//!   from the gateway closes the offending connection (fail-fast).
//! - Outbound `tokio::sync::broadcast` is bounded at 1024 frames per
//!   subscriber. A subscriber that lags by more than the buffer
//!   receives `Lagged(_)` and the per-session writer task closes
//!   the TCP connection. The engine never blocks on a slow
//!   subscriber.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use engine::{CounterIdGenerator, Engine, WallClock};
use gateway::{ChannelSink, run};
use tokio::sync::mpsc as tmpsc;
use wire::inbound::Inbound;

const DEFAULT_ADDR: &str = "0.0.0.0:9000";
const OUTBOUND_BROADCAST_CAPACITY: usize = 1024;
const INBOUND_CHANNEL_CAPACITY: usize = 8192;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).cloned().unwrap_or_else(|| DEFAULT_ADDR.into());
    eprintln!("engine: binding gateway listener on {addr}");

    // Async → sync bridge: the gateway runs on tokio and produces
    // typed `Inbound`. The engine is sync. We bridge with a sync
    // mpsc channel so the engine thread can `recv()` blocking.
    let (sync_tx, sync_rx) = mpsc::channel::<Inbound>();
    let (tokio_tx, mut tokio_rx) = tmpsc::channel::<Inbound>(INBOUND_CHANNEL_CAPACITY);

    // Forwarder task: read from tokio mpsc, push into sync mpsc.
    tokio::spawn(async move {
        while let Some(msg) = tokio_rx.recv().await {
            if sync_tx.send(msg).is_err() {
                eprintln!("engine: sync channel closed, dropping inbound");
                return;
            }
        }
    });

    // Outbound sink shared between engine (producer) and listener
    // (subscriber factory). Both clones share the same broadcast.
    let sink = ChannelSink::new(OUTBOUND_BROADCAST_CAPACITY);
    let listener_sink = Arc::new(sink.clone());

    // Engine thread.
    thread::Builder::new()
        .name("engine".into())
        .spawn(move || engine_loop(sync_rx, sink))
        .expect("engine thread spawn");

    // Listener loop. Returns on listener error.
    if let Err(e) = run(&addr, tokio_tx, listener_sink).await {
        eprintln!("engine: listener error: {e}");
        return Err(e);
    }
    Ok(())
}

fn engine_loop(rx: mpsc::Receiver<Inbound>, sink: ChannelSink) {
    let mut engine = Engine::new(WallClock, CounterIdGenerator::new(), sink);
    let mut count: u64 = 0;
    let mut last_log: u64 = 0;
    while let Ok(msg) = rx.recv() {
        engine.step(msg);
        count = count.wrapping_add(1);
        if count.is_multiple_of(10_000) && count != last_log {
            eprintln!("engine: processed {count} inbound commands");
            last_log = count;
        }
    }
    eprintln!("engine: inbound channel closed, shutting down");
}
