//! Smoke benchmark binary. Runs the documented `add_cancel_mix`
//! workload for a small fixed number of ops, records per-op latency
//! into an `hdrhistogram::Histogram::<u64>`, and prints the
//! percentile table to stdout. Intended for the docker compose
//! `bench` service so reviewers can run an end-to-end performance
//! check from a clean clone with `docker compose run bench`.
//!
//! The longer-form Criterion bench remains the source of truth for
//! authoritative numbers; this smoke binary is the cheap one.

use std::cell::Cell;
use std::time::Instant;

use domain::{AccountId, ClientTs, Clock, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
use engine::{CounterIdGenerator, Engine, VecSink};
use hdrhistogram::Histogram;
use wire::inbound::{CancelOrder, Inbound, NewOrder};

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

const N_OPS: u64 = 10_000;
const ACCOUNT_COUNT: u64 = 8;
const PRICE_LOW: i64 = 90;
const PRICE_HIGH: i64 = 110;

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
    let ops = generate_ops(N_OPS, 0xdeadbeefdeadbeef);
    let mut engine = Engine::new(BenchClock::new(), CounterIdGenerator::new(), VecSink::new());
    let mut hist =
        Histogram::<u64>::new_with_bounds(1, 30_000_000_000, 3).expect("histogram bounds valid");

    let bench_start = Instant::now();
    for op in &ops {
        let inbound = op_to_inbound(*op);
        let t0 = Instant::now();
        engine.step(inbound);
        let elapsed_ns = t0.elapsed().as_nanos() as u64;
        let _ = hist.record(elapsed_ns.max(1));
        engine.sink_mut().events.clear();
    }
    let total = bench_start.elapsed();

    println!("hft-clob-core smoke_bench: add_cancel_mix");
    println!("  ops              {N_OPS}");
    println!("  total wall-clock {} ms", total.as_millis());
    println!(
        "  throughput       {:.0} ops/sec",
        (N_OPS as f64) / total.as_secs_f64()
    );
    println!();
    println!("  p50    {} ns", hist.value_at_quantile(0.50));
    println!("  p99    {} ns", hist.value_at_quantile(0.99));
    println!("  p99.9  {} ns", hist.value_at_quantile(0.999));
    println!("  p99.99 {} ns", hist.value_at_quantile(0.9999));
    println!("  max    {} ns", hist.max());
}
