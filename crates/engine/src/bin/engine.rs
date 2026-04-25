//! Engine binary. Tokio entry point that owns the gateway listener,
//! a dedicated engine thread, and a `ChannelSink` fan-out for any
//! per-session outbound writers added in follow-ups.
//!
//! ## CLI
//!
//! ```text
//! engine [bind_addr]
//! ```
//!
//! Default bind address `0.0.0.0:9000`. The engine logs accepted
//! sessions to stderr; outbound events are encoded into the
//! `ChannelSink`'s broadcast channel but the per-session writer
//! tasks that subscribe and pump to TCP are out of scope for v1
//! and tracked as a follow-up. The binary is the smallest end-to-
//! end shape the docker compose smoke test exercises.

use std::sync::mpsc;
use std::thread;

use engine::{CounterIdGenerator, Engine, OutboundSink, WallClock};
use gateway::run;
use tokio::sync::mpsc as tmpsc;
use wire::inbound::Inbound;
use wire::outbound::Outbound;

const DEFAULT_ADDR: &str = "0.0.0.0:9000";

/// Drop-in sink that counts outbound events without holding them in
/// memory. The real production wiring substitutes a `ChannelSink`
/// once per-session TCP writers exist; for the smoke binary we
/// keep the hot path alloc-free and surface a periodic counter.
#[derive(Default)]
struct CountingSink {
    count: u64,
}

impl OutboundSink for CountingSink {
    fn emit(&mut self, _msg: Outbound) {
        self.count = self.count.wrapping_add(1);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).cloned().unwrap_or_else(|| DEFAULT_ADDR.into());
    eprintln!("engine: binding gateway listener on {addr}");

    // Async → sync bridge: the gateway runs on tokio and produces
    // typed `Inbound`. The engine is sync. We bridge with a sync
    // mpsc channel so the engine thread can `recv()` blocking.
    let (sync_tx, sync_rx) = mpsc::channel::<Inbound>();
    let (tokio_tx, mut tokio_rx) = tmpsc::channel::<Inbound>(8192);

    // Forwarder task: read from tokio mpsc, push into sync mpsc.
    tokio::spawn(async move {
        while let Some(msg) = tokio_rx.recv().await {
            if sync_tx.send(msg).is_err() {
                eprintln!("engine: sync channel closed, dropping inbound");
                return;
            }
        }
    });

    // Engine thread.
    thread::Builder::new()
        .name("engine".into())
        .spawn(move || engine_loop(sync_rx))
        .expect("engine thread spawn");

    // Listener loop. Returns on listener error.
    if let Err(e) = run(&addr, tokio_tx).await {
        eprintln!("engine: listener error: {e}");
        return Err(e);
    }
    Ok(())
}

fn engine_loop(rx: mpsc::Receiver<Inbound>) {
    let mut engine = Engine::new(
        WallClock,
        CounterIdGenerator::new(),
        CountingSink::default(),
    );
    let mut last_log: u64 = 0;
    while let Ok(msg) = rx.recv() {
        engine.step(msg);
        let count = engine.sink_mut().count;
        if count.is_multiple_of(10_000) && count != last_log {
            eprintln!("engine: outbound count = {count}");
            last_log = count;
        }
    }
    eprintln!("engine: inbound channel closed, shutting down");
}
