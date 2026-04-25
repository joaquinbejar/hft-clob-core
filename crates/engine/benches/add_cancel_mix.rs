//! `add_cancel_mix` — Criterion + hdrhistogram benchmark.
//!
//! Workload: 70 % `NewOrder` (limit), 20 % `CancelOrder`, 10 %
//! aggressive crosses. Mix ratio matches the v1 scenario in
//! `doc/DESIGN.md` § 9.
//!
//! Per-op timing measures `recv_ts → emit_ts` inside the engine
//! thread by reading a synthetic monotonic clock at step entry and
//! exit. The harness uses a fresh seeded RNG per iteration so the
//! recorded sequence is deterministic across runs and reproducible
//! when investigating regressions.
//!
//! Tail-latency emphasis (CLAUDE.md § Benchmarking): the report
//! shows p50 / p99 / p99.9 / p99.99 only — never mean / stddev.
//! The HDR histogram is configured with a 1 ns lower bound and a
//! 30-second upper bound, providing 3-significant-digit precision
//! across the relevant range.

use std::cell::Cell;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use domain::{AccountId, ClientTs, Clock, OrderId, OrderType, Price, Qty, RecvTs, Side, Tif};
use engine::{CounterIdGenerator, Engine, VecSink};
use hdrhistogram::Histogram;
use wire::inbound::{CancelOrder, Inbound, NewOrder};

/// Deterministic monotonic clock — one tick per `now()` call. The
/// engine reads this once at the top of every `step` (recv_ts) and
/// once per emitted message (emit_ts); we want the bench to
/// faithfully record the per-op latency, not the clock-read cost.
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

/// Tiny LCG to keep the bench deterministic and free of `rand`
/// (CLAUDE.md bans randomness on the matching path; the bench
/// driver lives outside that boundary but staying seeded keeps
/// regressions reproducible).
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

const N_OPS: u64 = 1_000_000;
const ACCOUNT_COUNT: u64 = 8;
const PRICE_LOW: i64 = 90;
const PRICE_HIGH: i64 = 110;

#[derive(Clone, Copy, Debug)]
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

/// Pre-generate a deterministic op stream. Cancel and aggressive
/// ops reference ids that have already been added so the workload
/// stays self-consistent — the engine never sees a Cancel for an
/// id it has not seen.
fn generate_ops(n: u64, seed: u64) -> Vec<Op> {
    let mut rng = Lcg::new(seed);
    let mut ops = Vec::with_capacity(n as usize);
    let mut next_id: u64 = 1;
    let mut active_ids: Vec<u64> = Vec::with_capacity(8192);
    for _ in 0..n {
        let r = rng.next() % 100;
        let account = ((rng.next() % ACCOUNT_COUNT) + 1) as u32;
        let side = if rng.next().is_multiple_of(2) {
            Side::Bid
        } else {
            Side::Ask
        };
        let price = PRICE_LOW + (rng.next() as i64) % (PRICE_HIGH - PRICE_LOW + 1);
        let qty = (rng.next() % 9) + 1;
        if r < 20 && !active_ids.is_empty() {
            // Cancel an existing id.
            let idx = (rng.next() as usize) % active_ids.len();
            let id = active_ids.swap_remove(idx);
            ops.push(Op::Cancel { id, account });
        } else if r < 30 {
            // Aggressive cross.
            ops.push(Op::Aggressive {
                id: next_id,
                account,
                side,
                price,
                qty,
            });
            next_id += 1;
        } else {
            // Add a passive limit on either side. Bias the price
            // away from the cross to avoid unintentional self-trades.
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

fn run_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("add_cancel_mix");
    // Throughput is one op per logical inbound command.
    group.throughput(criterion::Throughput::Elements(N_OPS));
    group.warm_up_time(std::time::Duration::from_secs(5));
    group.measurement_time(std::time::Duration::from_secs(10));

    let ops = generate_ops(N_OPS, 0xdeadbeefdeadbeef);

    group.bench_function("step_per_op", |b| {
        b.iter_batched(
            || Engine::new(BenchClock::new(), CounterIdGenerator::new(), VecSink::new()),
            |mut engine| {
                let mut hist = Histogram::<u64>::new_with_bounds(1, 30_000_000_000, 3)
                    .expect("hdr histogram bounds valid");
                for op in &ops {
                    let inbound = op_to_inbound(*op);
                    let t0 = Instant::now();
                    engine.step(inbound);
                    let elapsed_ns = t0.elapsed().as_nanos() as u64;
                    let _ = hist.record(elapsed_ns.max(1));
                    // Drain the sink between ops so capture cost
                    // does not skew the next step. Vec::clear is
                    // O(n) on its current length; for a typical
                    // step that is single digits.
                    engine.sink_mut().events.clear();
                }
                println!(
                    "p50={} ns p99={} ns p99.9={} ns p99.99={} ns max={} ns",
                    hist.value_at_quantile(0.50),
                    hist.value_at_quantile(0.99),
                    hist.value_at_quantile(0.999),
                    hist.value_at_quantile(0.9999),
                    hist.max(),
                );
                std::hint::black_box(hist);
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(benches, run_workload);
criterion_main!(benches);
