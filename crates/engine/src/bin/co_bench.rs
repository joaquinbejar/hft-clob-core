//! `co_bench` — closed-loop bench with coordinated-omission correction.
//!
//! Drives `add_cancel_mix` at a fixed target arrival rate. Each op
//! is scheduled at `start + i * interval`; the actual latency
//! recorded is `t_end - scheduled_start`. When the engine takes
//! longer than `interval`, subsequent ops are delayed: classical
//! coordinated omission. `Histogram::record_correct(value,
//! expected_interval)` backfills the histogram for the missed
//! intervals so the resulting percentiles reflect what a real
//! producer offering load at that rate would have observed.
//!
//! ## Run
//!
//! ```bash
//! # Default 200k ops/sec target rate, 1M ops measurement.
//! cargo run --release --bin co_bench
//!
//! # Custom rate via env var.
//! TARGET_OPS_PER_SEC=500000 cargo run --release --bin co_bench
//! ```
//!
//! Compare against the open-loop number printed by `smoke_bench`
//! at the same hardware.

use std::cell::Cell;
use std::time::{Duration, Instant};

use domain::{AccountId, ClientTs, Clock, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
use engine::{CounterIdGenerator, Engine, VecSink};
use hdrhistogram::Histogram;
use wire::inbound::{CancelOrder, Inbound, NewOrder};

const N_WARMUP: u64 = 50_000;
const N_MEASURE: u64 = 1_000_000;
const DEFAULT_TARGET_OPS_PER_SEC: u64 = 200_000;
const ACCOUNT_COUNT: u64 = 8;
const PRICE_LOW: i64 = 90;
const PRICE_HIGH: i64 = 110;

struct BenchClock {
    next: Cell<i64>,
}
impl BenchClock {
    fn new() -> Self {
        Self { next: Cell::new(1) }
    }
}
impl Clock for BenchClock {
    fn now(&self) -> RecvTs {
        let v = self.next.get();
        self.next.set(v + 1);
        RecvTs::new(v)
    }
}

struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0
    }
}

#[derive(Clone, Copy)]
enum Op {
    Add {
        id: u64,
        account: u32,
        side: Side,
        price: i64,
        qty: u64,
    },
    Cancel {
        id: u64,
        account: u32,
    },
    Aggressive {
        id: u64,
        account: u32,
        side: Side,
        price: i64,
        qty: u64,
    },
}

fn generate_ops(n: u64, seed: u64) -> Vec<Op> {
    let mut rng = Lcg::new(seed);
    let mut ops = Vec::with_capacity(n as usize);
    let mut next_id: u64 = 1;
    let mut active_ids: Vec<u64> = Vec::new();
    for _ in 0..n {
        let r = rng.next() % 100;
        let account = ((rng.next() % ACCOUNT_COUNT) + 1) as u32;
        let side = if rng.next().is_multiple_of(2) {
            Side::Bid
        } else {
            Side::Ask
        };
        let qty = (rng.next() % 9) + 1;
        if r < 20 && !active_ids.is_empty() {
            let idx = (rng.next() as usize) % active_ids.len();
            let id = active_ids.swap_remove(idx);
            ops.push(Op::Cancel { id, account });
        } else if r < 30 {
            let price = PRICE_LOW + (rng.next() as i64) % (PRICE_HIGH - PRICE_LOW + 1);
            ops.push(Op::Aggressive {
                id: next_id,
                account,
                side,
                price,
                qty,
            });
            next_id += 1;
        } else {
            let safe_price = match side {
                Side::Bid => PRICE_LOW,
                Side::Ask => PRICE_HIGH,
            };
            ops.push(Op::Add {
                id: next_id,
                account,
                side,
                price: safe_price,
                qty,
            });
            active_ids.push(next_id);
            next_id += 1;
        }
    }
    ops
}

fn op_to_inbound(op: Op) -> Inbound {
    match op {
        Op::Add {
            id,
            account,
            side,
            price,
            qty,
        }
        | Op::Aggressive {
            id,
            account,
            side,
            price,
            qty,
        } => Inbound::NewOrder(NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side,
            order_type: OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(Price::new(price).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
        }),
        Op::Cancel { id, account } => Inbound::CancelOrder(CancelOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
        }),
    }
}

fn main() {
    let target_rate: u64 = std::env::var("TARGET_OPS_PER_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TARGET_OPS_PER_SEC);
    let interval_ns: u64 = 1_000_000_000 / target_rate;
    let interval = Duration::from_nanos(interval_ns);

    let warmup_ops = generate_ops(N_WARMUP, 0xdeadbeef);
    let measure_ops = generate_ops(N_MEASURE, 0xc0ffee);

    let mut engine = Engine::new(BenchClock::new(), CounterIdGenerator::new(), VecSink::new());

    // Open-loop warmup just to warm caches; closed-loop measurement
    // applies after.
    for op in &warmup_ops {
        engine.step(op_to_inbound(*op));
        engine.sink_mut().events.clear();
    }

    let mut hist =
        Histogram::<u64>::new_with_bounds(1, 30_000_000_000, 3).expect("histogram bounds valid");

    let bench_start = Instant::now();
    for (i, op) in measure_ops.iter().enumerate() {
        let scheduled = bench_start + interval * (i as u32);
        // Busy-wait spin until the scheduled time. `thread::sleep`
        // on macOS / Linux has ~100 µs granularity which dominates
        // the latency budget at high target rates. Spin keeps the
        // CPU hot and gives ns-precision scheduling. Acceptable
        // for a bench harness; production schedulers would use a
        // hybrid sleep + spin pattern.
        while Instant::now() < scheduled {
            std::hint::spin_loop();
        }
        let inbound = op_to_inbound(*op);
        let t0 = Instant::now();
        engine.step(inbound);
        let step_ns = t0.elapsed().as_nanos() as u64;
        // Latency seen from the producer's perspective: time
        // between when the message would have arrived and when it
        // finished being processed. Captures coordinated omission
        // — if `step_ns > interval_ns`, the next op was already
        // late at delivery.
        let scheduled_latency_ns = scheduled.elapsed().as_nanos() as u64;
        let _ = hist.record_correct(scheduled_latency_ns.max(1), interval_ns);
        engine.sink_mut().events.clear();
        // Suppress unused: kept for diagnostic purposes if a
        // future consumer wants step-only vs scheduled-latency
        // breakdown.
        let _ = step_ns;
    }
    let total = bench_start.elapsed();

    println!("hft-clob-core co_bench: add_cancel_mix (closed-loop, CO-corrected)");
    println!("  target rate     {target_rate} ops/sec");
    println!("  interval        {interval_ns} ns");
    println!("  warmup ops      {N_WARMUP}");
    println!("  measurement ops {N_MEASURE}");
    println!("  total wall      {} ms", total.as_millis());
    println!();
    println!("  p50    {} ns", hist.value_at_quantile(0.50));
    println!("  p99    {} ns", hist.value_at_quantile(0.99));
    println!("  p99.9  {} ns", hist.value_at_quantile(0.999));
    println!("  p99.99 {} ns", hist.value_at_quantile(0.9999));
    println!("  max    {} ns", hist.max());
}
