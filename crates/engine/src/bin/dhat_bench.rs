//! `dhat_bench` — allocation-count proof on the engine hot path.
//!
//! Wires `dhat-rs` as the global allocator (via the `hotpath-dhat`
//! feature flag) so we can mechanically measure allocations/op
//! during the documented `add_cancel_mix` workload. Prints:
//!
//! - Total allocations during warmup (subtracted as baseline).
//! - Total allocations during measurement.
//! - Per-op allocation count (`measurement - warmup_baseline_per_op
//!   * measurement_ops`).
//! - Per-op bytes.
//!
//! ## Run
//!
//! ```bash
//! cargo run --release --features hotpath-dhat --bin dhat_bench
//! ```
//!
//! Without the feature flag the binary is excluded from the build
//! (see `[[bin]] required-features`).

#![cfg(feature = "hotpath-dhat")]

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::cell::Cell;

use domain::{AccountId, ClientTs, Clock, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
use engine::{CounterIdGenerator, Engine, VecSink};
use wire::inbound::{CancelOrder, Inbound, NewOrder};

const N_WARMUP: u64 = 10_000;
const N_MEASURE: u64 = 100_000;
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

fn drive(engine: &mut Engine<BenchClock, CounterIdGenerator, VecSink>, ops: &[Op]) {
    for op in ops {
        engine.step(op_to_inbound(*op));
        engine.sink_mut().events.clear();
    }
}

fn main() {
    let warmup_ops = generate_ops(N_WARMUP, 0xdeadbeef);
    let measure_ops = generate_ops(N_MEASURE, 0xc0ffee);
    let mut engine = Engine::new(BenchClock::new(), CounterIdGenerator::new(), VecSink::new());

    // Warmup: drives the engine to its steady state (level
    // bookkeeping, registry growth) so the measurement window
    // captures only steady-state per-op behaviour.
    let _profiler = dhat::Profiler::builder().testing().build();
    drive(&mut engine, &warmup_ops);
    let warmup_stats = dhat::HeapStats::get();

    // Measurement window. The dhat::HeapStats counters are
    // monotonic, so the per-window delta = post - pre.
    drive(&mut engine, &measure_ops);
    let measure_stats = dhat::HeapStats::get();

    let measure_allocs = measure_stats.total_blocks - warmup_stats.total_blocks;
    let measure_bytes = measure_stats.total_bytes - warmup_stats.total_bytes;
    let allocs_per_op = (measure_allocs as f64) / (N_MEASURE as f64);
    let bytes_per_op = (measure_bytes as f64) / (N_MEASURE as f64);

    println!("hft-clob-core dhat_bench: add_cancel_mix");
    println!("  warmup ops          {N_WARMUP}");
    println!("  measurement ops     {N_MEASURE}");
    println!();
    println!("  warmup total allocs {}", warmup_stats.total_blocks);
    println!("  warmup total bytes  {}", warmup_stats.total_bytes);
    println!();
    println!("  measurement allocs  {}", measure_allocs);
    println!("  measurement bytes   {}", measure_bytes);
    println!();
    println!("  allocs per op       {:.3}", allocs_per_op);
    println!("  bytes per op        {:.1}", bytes_per_op);
}
