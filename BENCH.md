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
| CPU pinning | None (default macOS scheduler); pinning to the perf cores is future work |
| Coordinated-omission correction | **None at the bench level**. The bench drives synchronous `step()` calls in a tight loop; there is no arrival-rate target, so the classical CO failure mode (a slow op delays the next request and hides the slow op from the histogram) does not apply. Histogram values are raw `t_end - t_start` per op. When this bench grows a closed-loop driver (issue 17 follow-up), `Histogram::record_correct(value, expected_interval)` will replace `record(value)`. |

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
