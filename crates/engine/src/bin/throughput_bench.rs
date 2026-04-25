//! `throughput_bench` — sustained burst throughput on the hot path.
//!
//! Runs the documented `add_cancel_mix` workload for 1,000,000 ops
//! after a 100,000-op warmup, then reports the sustained ops/sec
//! computed from `total_ns / N_MEASURE`. No per-op timing
//! instrumentation — that lives in `smoke_bench` and the Criterion
//! `add_cancel_mix` bench. The output is the headline burst number
//! the spec asks for separate from the percentile signal.
//!
//! ## Run
//!
//! ```bash
//! cargo run --release --bin throughput_bench
//! ```
//!
//! Output:
//!
//! ```text
//! hft-clob-core throughput_bench: add_cancel_mix
//!   warmup ops      100000
//!   measurement ops 1000000
//!   warmup wall     X ms
//!   measurement     Y ms
//!   throughput      Z ops/sec
//! ```

use std::cell::Cell;
use std::time::Instant;

use domain::{AccountId, ClientTs, Clock, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
use engine::{CounterIdGenerator, Engine, VecSink};
use wire::inbound::{CancelOrder, Inbound, NewOrder};

const N_WARMUP: u64 = 100_000;
const N_MEASURE: u64 = 1_000_000;
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
    let warmup_ops = generate_ops(N_WARMUP, 0xdeadbeef);
    let measure_ops = generate_ops(N_MEASURE, 0xc0ffee);
    let mut engine = Engine::new(BenchClock::new(), CounterIdGenerator::new(), VecSink::new());

    let warmup_start = Instant::now();
    for op in &warmup_ops {
        engine.step(op_to_inbound(*op));
        engine.sink_mut().events.clear();
    }
    let warmup_elapsed = warmup_start.elapsed();

    let measure_start = Instant::now();
    for op in &measure_ops {
        engine.step(op_to_inbound(*op));
        engine.sink_mut().events.clear();
    }
    let measure_elapsed = measure_start.elapsed();

    let throughput_ops_per_sec = (N_MEASURE as f64) / measure_elapsed.as_secs_f64();

    println!("hft-clob-core throughput_bench: add_cancel_mix");
    println!("  warmup ops      {N_WARMUP}");
    println!("  measurement ops {N_MEASURE}");
    println!("  warmup wall     {} ms", warmup_elapsed.as_millis());
    println!("  measurement     {} ms", measure_elapsed.as_millis());
    println!("  throughput      {:.0} ops/sec", throughput_ops_per_sec);
}
