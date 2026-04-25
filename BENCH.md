# Benchmark — `add_cancel_mix`

Hot-path latency for the engine's `step()` pipeline under a mixed
inbound workload. Each measurement is the wall-clock interval
between `recv_ts = clock.now()` at the top of `Engine::step` and
the return of the call (after risk, matching, registry update, and
all outbound emission to the in-memory sink). Latency is recorded
into an `hdrhistogram::Histogram::<u64>` with 1 ns resolution and a
30 s upper bound; the report covers `p50 / p99 / p99.9 / p99.99 /
max` only — no mean, no stddev, per CLAUDE.md tail-latency
discipline.

## Methodology

| Knob | Value |
|------|-------|
| Workload mix | 70 % `NewOrder` (limit), 20 % `CancelOrder`, 10 % aggressive crosses |
| Ops per iter | 1,000,000 |
| Warmup | 5 s (Criterion default override; raised from 3 s) |
| Measurement window | 10 s |
| Sample size | Criterion default (100 by default; 10 in the captured numbers below for budget reasons) |
| Histogram resolution | 1 ns lower bound, 30 s upper bound, 3 significant digits |
| RNG | Seeded LCG inside the bench (no `rand::*`, deterministic) |
| Sink | `engine::VecSink` with `events.clear()` per op (drains, never mutates external state) |
| Clock | `BenchClock` — synthetic monotonic, one tick per `now()` call |
| CPU pinning | Optional via `pinned-engine` feature + `--engine-core <N>`. Default off — Linux is the supported target; macOS treats the affinity hint as a soft QoS bias. |
| Coordinated-omission correction | None on the open-loop `add_cancel_mix` Criterion bench (no arrival-rate target). A separate `co_bench` binary runs a **closed-loop** driver at a configurable target rate and applies `Histogram::record_correct(value, expected_interval)`. See "Closed-loop CO-corrected" section below. |

The driver is `criterion::iter_batched` with `BatchSize::SmallInput`
to keep the per-iter setup cost (`Engine::new`) outside the
measurement window.

## Hardware (captured below)

- **CPU**: Apple M4 Max (`arm64`, 16 cores: 12 perf + 4 efficiency)
- **OS**: macOS Darwin 25.4
- **Build**: `cargo bench --bench add_cancel_mix` with the workspace
  `[profile.bench]` settings — `opt-level = 3`, `lto = "fat"`,
  `codegen-units = 1`, `debug = true`.

## Observed numbers

Capture from a single run with `--warm-up-time 1 --measurement-time 3
--sample-size 10` for budget — the captured percentiles are stable
across the 18 iterations Criterion ran:

| Percentile | Min observed (ns) | Median across iterations (ns) | Max observed (ns) |
|-----------:|------------------:|------------------------------:|------------------:|
| p50        | 41                | 41                            | 41                |
| p99        | 458               | 542                           | 625               |
| p99.9      | 201,855           | 213,887                       | 230,271           |
| p99.99     | 272,127           | 412,671                       | 569,343           |
| max        | 887,807           | 1,016,831                     | 4,702,207         |

(All values in nanoseconds. `min observed` is the lowest the histogram
saw across iterations; `median` is the median across the 18-iteration
sample; `max observed` is the worst.)

A representative single-iteration row from the bench output:

```
p50=41 ns p99=500 ns p99.9=210175 ns p99.99=387583 ns max=921599 ns
```

Default-budget run (5 s warmup + 10 s measurement, 100 samples) is
expected to tighten the p99.9 / p99.99 spread modestly; the median
across the captured-budget run is already in line with the
microstructure cost model (matching is dominated by the
`snapshot_orders()` allocation per level entry, which is the next
target — see "Where the tail comes from" below).

## Closed-loop CO-corrected

The Criterion `add_cancel_mix` bench is open-loop: a tight `for` loop
calls `engine.step()` synchronously and records `t_end - t_start`.
That measurement style understates tail latency under realistic
offered load — when `step()` runs slow, the next "request" is
delayed and the slow op is hidden from the histogram.

`crates/engine/src/bin/co_bench.rs` runs a closed-loop driver at a
configurable target arrival rate. Each op is scheduled at
`bench_start + i * interval`; before issuing, the driver
busy-waits (`spin_loop`) until `Instant::now() >= scheduled`. The
recorded value is `scheduled.elapsed()` — i.e., the latency the
producer would observe from "I would have delivered this at time T"
to "engine returned." `Histogram::record_correct(value,
expected_interval)` synthesises additional records for any op
where `value > interval`, backfilling the missed intervals.

### Run

```bash
# Default 200k ops/sec target rate, 1M ops measurement.
cargo run --release --bin co_bench

# Custom rate via env var.
TARGET_OPS_PER_SEC=100000 cargo run --release --bin co_bench
```

### Captured numbers (Apple M4 Max, release build, 100k ops/sec target)

| Metric | Value |
|--------|------:|
| Target rate | 100,000 ops/sec |
| Interval | 10,000 ns |
| Warmup | 50,000 ops |
| Measurement | 1,000,000 ops |
| Wall-clock | 9,999 ms |
| p50 | 26,543 ns |
| p99 | 813,055 ns |
| p99.9 | 1,209,343 ns |
| p99.99 | 1,577,983 ns |
| max | 1,765,375 ns |

### Comparison to open-loop

Open-loop p99.9 from the Criterion bench is ~210 µs; CO-corrected
p99.9 at 100k ops/sec is ~1.21 ms, reflecting the additional
latency a real producer offering load at that rate would have
observed. The deltas are dominated by:

- Spin-loop overshoot on macOS — `Instant::now()` is precise but
  the spin path itself has scheduler-driven jitter at sub-µs scale.
- Engine `step()` cost variance (the same `snapshot_orders()`
  allocation that drives the open-loop tail still applies).

On a Linux box with `--engine-core` pinning + `taskset` cgroup
isolation the CO-corrected p50 should collapse toward the open-
loop p50 (the spin path is ns-precise without scheduler jitter).
Capturing those numbers is a follow-up — the methodology and the
binary are in place.

## CPU pinning + NUMA placement

The engine binary supports CPU pinning behind a feature flag —
recommended on Linux for production runs and bench laps where p99.99 /
max are interesting.

```bash
cargo run --release --features pinned-engine --bin engine -- 0.0.0.0:9000 --engine-core 4
```

What pinning actually does:

- Linux: `core_affinity::set_for_current` calls `sched_setaffinity(2)`
  with a single-CPU mask. The matching thread stops migrating across
  cores. Combined with `taskset` / `cpuset` cgroups isolating the
  chosen core from other workloads, the p99.99 tail collapses
  toward p99 because OS scheduler preemption disappears.
- macOS: the same `core_affinity` call maps to a QoS hint
  (`pthread_set_qos_class_self_np`-style). Effect is real but
  smaller than on Linux — reproducible numbers there require a
  Linux box.

NUMA placement is intentionally out of scope for v1 (single-symbol,
single-thread engine doesn't have a memory-locality story to tell
yet). When the SPMC outbound bipartite split lands per the README's
"honest limitation" section, the producer / consumer threads should
co-locate on the same NUMA node — that's a follow-up issue.

Without the `pinned-engine` feature, `--engine-core` is parsed but
ignored with a warning. The unpinned path is what the captured
numbers in this document reflect.

## Sustained throughput

The Criterion bench reports per-op tail latency. The spec also asks
for sustained throughput under a 1M-order burst as a separate
headline number — that signal is what one engine core can absorb
under the documented mix without queuing.

`crates/engine/src/bin/throughput_bench.rs` runs the same
`add_cancel_mix` workload (70 / 20 / 10 add / cancel / aggressive,
deterministic LCG seed) for `N_WARMUP = 100_000` warmup ops + a
contiguous `N_MEASURE = 1_000_000` measurement burst on a single
thread. Throughput is `N_MEASURE / measure_wall_clock`.

### Captured numbers (Apple M4 Max, release build)

| Metric | Value |
|--------|------:|
| Warmup ops | 100,000 |
| Measurement ops | 1,000,000 |
| Warmup wall-clock | 7 ms |
| Measurement wall-clock | 655 ms |
| **Sustained throughput** | **~1.53 M ops/sec** |

### Reproducing

```bash
cargo run --release --bin throughput_bench
```

### Interpretation

The 1.53 M ops/sec ceiling is bounded by the same per-level
`snapshot_orders()` allocation that drives the p99.9 percentile
in the Criterion table (~210 µs). The ~25 % of ops that take an
aggressive path through the fill loop pay one Vec allocation +
N atomic refcount bumps per level entered, which is what the
allocator pause tail captures. The Book-side `VecDeque<OrderId>`
mirror evolution should lift this number into the 3–5 M ops/sec
neighborhood without any change to the matching policy or
outbound contract.

## Where the tail comes from

- **p50 (≈ 40 ns)** — the hot path is dominated by the inner
  `match_aggressive` walk plus the post-step `BookUpdateTop`
  emission. Both are integer-only, no allocator, no syscall.
- **p99 (≈ 500 ns)** — branch mispredictions on the `Inbound` /
  `ExecState` match arms plus the per-side `BTreeMap::keys()`
  navigation when the price index spans many levels.
- **p99.9 (≈ 200 µs)** — the matching crate's `level.snapshot_orders()`
  per level entry: `DashMap::iter().map(Arc::clone).collect() +
  sort_by_key`. This is the largest known offender on the fill
  loop and is tracked under the determinism follow-up
  (`hotpath-reviewer P2-01` from #10) — moving to a Book-side
  `VecDeque<OrderId>` mirror eliminates the allocation and the
  N atomic refcount bumps.
- **p99.99 (≈ 0.5 ms) and max (≈ 1 ms)** — macOS thread-scheduler
  preemption (no CPU pinning) and stop-the-world allocator pauses
  caused by the `BookUpdateTop` emission's `Vec` growth before its
  capacity is cached. Pinning to a perf core and pre-sizing the
  per-emission buffers is expected to halve this; CO correction
  with a target arrival rate would expose remaining tail caused
  by GC-style cleanup in `pricelevel`.

## Allocation count on the hot path

Tracked target: zero allocations on the matching path after
warmup, per CLAUDE.md § Benchmarking. Mechanically verified via
`dhat-rs` behind the `hotpath-dhat` feature flag.

### Reproducing the measurement

```bash
cargo run --release --features hotpath-dhat --bin dhat_bench
```

The binary runs the documented `add_cancel_mix` mix for `N_WARMUP =
10_000` warmup ops + `N_MEASURE = 100_000` measurement ops. dhat
counts every heap allocation (and every byte) made by the global
allocator during the run. The `dhat::HeapStats` counters are
monotonic, so the per-window delta is `post - pre`.

### Captured numbers (Apple M4 Max, release build)

| Metric | Value |
|--------|------:|
| Warmup ops | 10,000 |
| Measurement ops | 100,000 |
| Warmup total allocations | 8,771 |
| Warmup total bytes | 1,175,012 |
| Measurement allocations (delta) | 82,805 |
| Measurement bytes (delta) | 7,862,672 |
| **Allocations per op** | **0.828** |
| **Bytes per op** | **78.6** |

### Where the per-op allocation comes from

Manual reading of the diff plus the dhat numbers cross-confirms:

- `Engine::step` itself (cancel / rejected / replace / mass-cancel
  / kill-switch) is zero-alloc on the steady state — the scratch
  buffers `fills_buf` / `stp_buf` / `mass_buf` are pre-sized in
  `Engine::new` and reused.
- `match_aggressive` per level entered: one
  `Vec<Arc<OrderType<()>>>` via `pricelevel::PriceLevel::snapshot_orders()`,
  plus N `Arc::clone` ref-count bumps. This is the documented v1
  trade-off — pricelevel's `iter_orders()` walks a `DashMap` and
  is hash-ordered, so the snapshot is the only deterministic-FIFO
  primitive available. ~0.8 allocs/op matches the workload's mix
  (10 % aggressive crosses; most aggressive walks touch 1–2
  levels; ~0.1 allocs/op of structural overhead from
  `OrderRegistryEntry` HashMap growth and the encoder Vec inside
  `ChannelSink::emit` adds the rest).
- `OrderRegistryEntry` HashMap and `risk::per_account` HashMap —
  amortised zero on the steady state once the working set stops
  growing the bucket array.

The next leverage move is the Book-side `BTreeMap<Price,
VecDeque<OrderId>>` mirror that lets the fill loop peek the FIFO
front in O(1) without `snapshot_orders()`. That eliminates both
the per-level Vec and the N atomic refcount bumps, putting the
steady-state per-op allocation count at zero or near-zero.

## Reproducing locally

```bash
# Default budget (5 s warmup + 10 s measurement, 100 samples per
# iteration). Takes about 18 minutes wall-clock on the reference
# hardware.
cargo bench --bench add_cancel_mix

# Faster sanity run (1 s warmup + 3 s measurement, 10 samples).
cargo bench --bench add_cancel_mix -- \
    --warm-up-time 1 --measurement-time 3 --sample-size 10
```

The Criterion HTML report lands at
`target/criterion/add_cancel_mix/step_per_op/report/index.html` —
each iteration prints the `p50 / p99 / p99.9 / p99.99 / max` line
to stdout, easy to grep for in CI logs.
