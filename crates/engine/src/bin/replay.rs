//! Replay binary. Reads a recorded inbound stream (`gateway::Recorder`
//! format), drives a fresh `engine::Engine` backed by `StubClock` and
//! `CounterIdGenerator`, and writes the resulting outbound byte stream
//! to disk in the same framing the live wire uses.
//!
//! ## CLI
//!
//! ```text
//! replay <inbound.bin> <outbound.bin> [--no-timestamps]
//! ```
//!
//! `--no-timestamps` zeros every `emit_ts` / `recv_ts` field on the
//! emitted outbound messages before they are encoded — the
//! spec-documented escape hatch for the `pricelevel::Trade::new`
//! wall-clock leak (see CLAUDE.md § Third-party crates). With the
//! flag, two replays of the same inbound bytes produce a
//! byte-identical outbound stream against the committed golden.
//!
//! ## Exit codes
//!
//! - `0` on success.
//! - `2` on argv / I/O failure.
//! - `3` on inbound decode failure (trailing partial record always
//!   tolerated; only structural corruption surfaces here).

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use engine::{CounterIdGenerator, Engine, StubClock, VecSink};
use gateway::recorder;
use marketdata::encoder;
use wire::framing::Frame;
use wire::outbound::Outbound;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let result = run(&args);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(ReplayError::Usage) => {
            eprintln!("usage: replay <inbound.bin> <outbound.bin> [--no-timestamps]");
            ExitCode::from(2)
        }
        Err(ReplayError::Io(e)) => {
            eprintln!("replay: I/O error: {e}");
            ExitCode::from(2)
        }
        Err(ReplayError::Decode(msg)) => {
            eprintln!("replay: decode error: {msg}");
            ExitCode::from(3)
        }
    }
}

#[derive(Debug)]
enum ReplayError {
    Usage,
    Io(std::io::Error),
    Decode(String),
}

impl From<std::io::Error> for ReplayError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

fn run(args: &[String]) -> Result<(), ReplayError> {
    if args.len() < 3 {
        return Err(ReplayError::Usage);
    }
    let inbound_path = PathBuf::from(&args[1]);
    let outbound_path = PathBuf::from(&args[2]);
    let no_timestamps = args.iter().any(|a| a == "--no-timestamps");

    let bytes = fs::read(&inbound_path)?;
    let outbound_bytes =
        replay_bytes(&bytes, no_timestamps).map_err(|e| ReplayError::Decode(e.to_string()))?;
    fs::write(&outbound_path, outbound_bytes)?;
    Ok(())
}

/// Drive a fresh engine over the inbound `bytes` (recorder format),
/// returning the encoded outbound byte stream. Pure function — no
/// I/O, no state retained between calls. Used by both the CLI and
/// the determinism proptest.
///
/// # Errors
/// Surfaces decode failures from the inbound recorder reader or
/// the wire frame parser. A trailing partial inbound record is
/// tolerated and silently dropped.
pub fn replay_bytes(
    bytes: &[u8],
    no_timestamps: bool,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let (records, _trailing) = recorder::read_records(bytes)?;
    let mut engine = Engine::new(StubClock::new(0), CounterIdGenerator::new(), VecSink::new());
    for rec in &records {
        engine.clock().set(rec.recv_ts.as_nanos());
        let (frame, _consumed) = Frame::parse(rec.frame)?;
        let inbound = wire::inbound::parse_frame(frame)?;
        engine.step(inbound);
    }
    let events = std::mem::take(&mut engine.sink_mut().events);
    let mut out = Vec::with_capacity(events.len() * 64);
    for mut ev in events {
        if no_timestamps {
            zero_timestamps(&mut ev);
        }
        encoder::encode(&ev, &mut out)?;
    }
    Ok(out)
}

/// Zero every wall-clock-shaped field on the outbound event so two
/// independent replays produce byte-identical bytes.
fn zero_timestamps(ev: &mut Outbound) {
    use domain::RecvTs;
    match ev {
        Outbound::ExecReport(r) => {
            r.recv_ts = RecvTs::new(0);
            r.emit_ts = RecvTs::new(0);
        }
        Outbound::TradePrint(t) => {
            t.emit_ts = RecvTs::new(0);
        }
        Outbound::BookUpdateTop(b) => {
            b.emit_ts = RecvTs::new(0);
        }
        Outbound::BookUpdateL2Delta(d) => {
            d.emit_ts = RecvTs::new(0);
        }
        Outbound::SnapshotResponse(s) => {
            s.recv_ts = RecvTs::new(0);
            s.emit_ts = RecvTs::new(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
    use wire::framing::MessageKind;
    use wire::inbound::{NewOrder, new_order};

    fn record_new_order(out: &mut Vec<u8>, id: u64, account: u32, price: i64, qty: u64, ts: i64) {
        let msg = NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side: Side::Bid,
            order_type: OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(Price::new(price).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
        };
        let mut payload = Vec::new();
        new_order::encode(&msg, &mut payload);
        let mut frame = Vec::new();
        Frame::write(MessageKind::NewOrder, &payload, &mut frame).expect("fits");
        out.extend_from_slice(&frame);
        let ts_u: u64 = ts as u64;
        out.extend_from_slice(&ts_u.to_le_bytes());
    }

    #[test]
    fn test_replay_bytes_two_independent_runs_identical_with_no_timestamps() {
        let mut inbound = Vec::new();
        for i in 0..20u64 {
            record_new_order(
                &mut inbound,
                i + 1,
                7,
                100 + (i as i64),
                5,
                1_000 + i as i64,
            );
        }
        let a = replay_bytes(&inbound, true).expect("a");
        let b = replay_bytes(&inbound, true).expect("b");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn test_replay_bytes_two_independent_runs_identical_with_timestamps_when_clock_deterministic() {
        let mut inbound = Vec::new();
        for i in 0..10u64 {
            record_new_order(
                &mut inbound,
                i + 1,
                7,
                100 + (i as i64),
                5,
                1_000 + i as i64,
            );
        }
        let a = replay_bytes(&inbound, false).expect("a");
        let b = replay_bytes(&inbound, false).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn test_replay_bytes_partial_trailing_record_is_tolerated() {
        let mut inbound = Vec::new();
        record_new_order(&mut inbound, 1, 7, 100, 5, 1_000);
        // Append a few junk bytes that look like the start of a
        // truncated record.
        inbound.extend_from_slice(&[0u8; 3]);
        let out = replay_bytes(&inbound, true).expect("replay");
        assert!(!out.is_empty());
    }

    // Silence the unused import warning when the test crate is built
    // alone — `RecvTs` is used in `zero_timestamps`.
    #[allow(dead_code)]
    fn _touch_recv_ts() -> RecvTs {
        RecvTs::new(0)
    }
}
