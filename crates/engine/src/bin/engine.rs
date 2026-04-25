//! Engine binary. Tokio entry point that owns the gateway listener,
//! a dedicated engine thread, and a `ChannelSink` broadcast that
//! every accepted TCP session subscribes to.
//!
//! ## CLI
//!
//! ```text
//! engine [bind_addr] [--engine-core <N>]
//! ```
//!
//! Default bind address `0.0.0.0:9000`. Connected clients can
//! submit framed inbound messages and read framed outbound bytes
//! (`ExecReport` / `TradePrint` / `BookUpdateTop` /
//! `BookUpdateL2Delta` / `SnapshotResponse`) on the same TCP
//! connection.
//!
//! `--engine-core <N>` pins the matching thread to physical core
//! `N` via `core_affinity::set_for_current`. Requires the
//! `pinned-engine` feature flag at compile time. Linux is the
//! best-supported target — pinning sharply reduces p99.99 / max
//! tail latency under load by removing scheduler preemption.
//! macOS treats the affinity hint as a soft QoS bias; the
//! observable delta is smaller. Without the flag (or without the
//! feature), the engine thread runs on the OS-default scheduler.
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
    let addr = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .unwrap_or_else(|| DEFAULT_ADDR.into());
    let engine_core = parse_engine_core(&args);
    eprintln!("engine: binding gateway listener on {addr}");
    if let Some(core) = engine_core {
        eprintln!("engine: requested CPU pinning to core {core}");
    }

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
        .spawn(move || {
            pin_to_core(engine_core);
            engine_loop(sync_rx, sink);
        })
        .expect("engine thread spawn");

    // Listener loop. Returns on listener error.
    if let Err(e) = run(&addr, tokio_tx, listener_sink).await {
        eprintln!("engine: listener error: {e}");
        return Err(e);
    }
    Ok(())
}

/// Parse `--engine-core <N>` from `args`. Returns `None` when the
/// flag is absent or the value is malformed (silently — pinning is
/// a soft optimisation; an invalid argument should not fail boot).
fn parse_engine_core(args: &[String]) -> Option<usize> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--engine-core" {
            return iter.next().and_then(|v| v.parse::<usize>().ok());
        }
    }
    None
}

#[cfg(feature = "pinned-engine")]
fn pin_to_core(core: Option<usize>) {
    let Some(core_idx) = core else {
        return;
    };
    match core_affinity::get_core_ids() {
        Some(ids) => match ids.get(core_idx) {
            Some(id) => {
                if core_affinity::set_for_current(*id) {
                    eprintln!("engine: pinned thread to core {core_idx}");
                } else {
                    eprintln!("engine: pin to core {core_idx} failed (continuing unpinned)");
                }
            }
            None => {
                eprintln!(
                    "engine: requested core {core_idx} out of range (have {} cores)",
                    ids.len()
                );
            }
        },
        None => {
            eprintln!("engine: core_affinity::get_core_ids() returned None — cannot pin");
        }
    }
}

#[cfg(not(feature = "pinned-engine"))]
fn pin_to_core(core: Option<usize>) {
    if core.is_some() {
        eprintln!(
            "engine: --engine-core requested but binary not built with `--features pinned-engine`"
        );
    }
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
