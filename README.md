# hft-clob-core

Single-symbol central-limit-order-book (CLOB) matching engine in Rust.
Hot path is integer-only, single-writer, no `tokio` and no wall-clock
reads inside matching. Replay is byte-identical against a committed
golden file.

## Architecture

The workspace splits along strict module boundaries; each crate is a
single concern with no upward dependencies. The matching core is the
**single writer** — every state mutation runs on one thread.

```
                            ┌──────────────────────────────────────┐
                            │             gateway                  │
                            │  (tokio TCP listener + recorder +    │
                            │   ChannelSink fan-out)               │
                            └─────────────┬────────────────────────┘
                                          │ tokio::mpsc<Inbound>
                                          ▼
   ┌──────────┐   ┌──────────┐   ┌────────────────────────────────┐
   │  domain  │ ◀ │   wire   │ ◀ │            engine              │
   │ (types,  │   │ (framing │   │ ┌──────┐  ┌──────┐  ┌────────┐ │
   │  Clock,  │   │  encode  │   │ │ risk │→ │ book │→ │ sink:  │ │
   │  IdGen,  │   │  decode) │   │ └──────┘  └──────┘  │ Channel│ │
   │  consts) │   └──────────┘   │           ▲         └────────┘ │
   └──────────┘         ▲        │           │                    │
        ▲               │        │  ┌────────┴────────────────┐   │
        │               │        │  │ matching                │   │
        │               │        │  │ (Book, match_aggressive,│   │
        │               │        │  │  STP, replace,          │   │
        │               │        │  │  mass_cancel, FIFO via  │   │
        │               │        │  │  pricelevel snapshot)   │   │
        │               │        │  └─────────────────────────┘   │
        │               │        └────────────────────────────────┘
        │               │                  │
        │               │                  │ broadcast::Bytes
        │               │                  ▼
        │               │        ┌──────────────────────────────┐
        │               └──────▶ │ marketdata                   │
        │                        │ (Outbound encoder + decoder, │
        │                        │  OutboundSink + VecSink)     │
        │                        └──────────────────────────────┘
        │
        │            (no upward edges; everything depends on `domain`)
        │
        ▼
  pricelevel (path dep)   ironsbe-* (path deps; gateway-only)
```

### Crate map

| Crate | Concerns | Bans |
|-------|----------|------|
| `domain` | Newtypes, consts, `Clock` / `IdGenerator` traits | I/O, async, time, randomness |
| `matching` | `Book`, `match_aggressive`, STP, `replace`, `mass_cancel`. Single writer. | Wall-clock, randomness, hash-based iteration into outputs, `tokio`, `tracing` macros |
| `risk` | Pre-trade gate (kill / band / per-account caps). `RiskState`. | tokio, tracing, wall-clock, randomness |
| `wire` | Bespoke binary framing + per-message encode / decode (`#[repr(C, packed)]` + zerocopy) | serde / bincode (penalised by spec) |
| `marketdata` | `Outbound` byte encoder; `OutboundSink` trait; `VecSink` for tests / replay | tokio |
| `gateway` | TCP listener, per-connection inbound decode, ingress recorder, `ChannelSink` broadcast. **Only crate using tokio** | reaching into matching / risk / domain |
| `engine` | Wires risk + matching + sink. `Engine::step(Inbound)`. `WallClock` / `StubClock` / `CounterIdGenerator` live here. | `tokio`, async, `.await`, global mutable state |

### Runtime topology

A single-symbol deployment is one `Engine` thread driven by one
`tokio` runtime that hosts the gateway. The engine reads `Inbound`
from a `tokio::sync::mpsc::Receiver` (sync `recv()` from the engine
thread), runs the pipeline, and emits via `OutboundSink` →
`gateway::ChannelSink` → `tokio::sync::broadcast::Sender<Bytes>` →
per-session async TCP writers. Backpressure on the inbound mpsc is
"close the client connection if the ring is full" (fail-fast); on
the outbound broadcast a slow subscriber drops with `Lagged` and is
expected to disconnect / re-subscribe.

## Wire protocol

The on-wire format is bespoke, fixed-size, and little-endian. Every
message is length-prefixed:

```
| len: u32 LE | kind: u8 | payload: [u8; len - 1] |
```

`len` counts bytes from `kind` through the end of the payload, so the
total frame size is `4 + len`.

Per-message field offsets and widths live in
[`docs/protocol.md`](docs/protocol.md) — single source of truth for
encoders, decoders, and roundtrip tests.

### Inbound message kinds

| Kind | Hex   | Name              | Payload size | Purpose                             |
|------|-------|-------------------|--------------|-------------------------------------|
| 1    | 0x01  | `NewOrder`        | 40 bytes     | Limit or market order entry         |
| 2    | 0x02  | `CancelOrder`     | 24 bytes     | Cancel a resting order              |
| 3    | 0x03  | `CancelReplace`   | 40 bytes     | Modify a resting order              |
| 4    | 0x04  | `MassCancel`      | 16 bytes     | Cancel all resting orders for an account |
| 5    | 0x05  | `KillSwitchSet`   | 24 bytes     | Admin halt / resume                 |
| 6    | 0x06  | `SnapshotRequest` | 16 bytes     | Operator dump of book + engine state |

### Outbound message kinds

| Kind | Hex   | Name                | Payload size | Purpose                                            |
|------|-------|---------------------|--------------|----------------------------------------------------|
| 101  | 0x65  | `ExecReport`        | 64 bytes     | Order lifecycle transition (accepted, rejected, (partially) filled, cancelled, replaced) |
| 102  | 0x66  | `TradePrint`        | 48 bytes     | Public trade emission                              |
| 103  | 0x67  | `BookUpdateTop`     | 48 bytes     | Top-of-book on every book change                   |
| 104  | 0x68  | `BookUpdateL2Delta` | 40 bytes     | Per-level depth delta (level removed when `new_qty = 0`) |

`SnapshotResponse` (105) is intentionally absent from the bespoke
fixed-size pattern; the variable-length book depth array lives behind a
separate framing scheme to be added under a later issue.

### Decode invariants

- Frame length must match the declared `len`; truncation returns
  `WireError::Truncated`.
- `kind` must be assigned; otherwise `WireError::UnknownKind(byte)`.
- Every reserved padding byte must be zero; otherwise
  `WireError::NonZeroPad(offset)`.
- Each integer field is converted to its `domain::` newtype
  (`Price`, `Qty`, `OrderId`, `AccountId`, …) at the boundary; failures
  surface as `WireError::Domain(_)`.

`WIRE_VERSION = 1`. Bumped on any layout change to inbound or outbound
message bodies.

### Numeric representation

| Field | Wire type | Domain newtype | Notes |
|-------|-----------|----------------|-------|
| `price` | `i64` LE | `Price` | Multiple of `domain::consts::TICK_SIZE`. **Never** `f32`/`f64`. |
| `qty` | `u64` LE | `Qty` | Multiple of `domain::consts::LOT_SIZE`. |
| `order_id` | `u64` LE | `OrderId` | Client-assigned, `> 0`. |
| `account_id` | `u32` LE | `AccountId` | `> 0`. |
| `recv_ts` / `emit_ts` | `u64` LE | `RecvTs` | Nanoseconds since session start. |
| `engine_seq` | `u64` LE | `EngineSeq` | Strictly monotonic across the entire outbound stream. |

## How to run

### Engine + gateway (live TCP)

A single-binary deployment is the `engine` crate's main path; for
v1 the binary's command-line wiring lives behind a follow-up issue,
but the components compose:

```bash
# Run the test suite end-to-end (matching, risk, engine, wire,
# marketdata, gateway, replay):
cargo nextest run

# Verify the workspace builds in release with LTO.
cargo build --release
```

### Replay against the golden file

```bash
cargo run --release --bin replay -- \
    fixtures/inbound.bin /tmp/out.bin --no-timestamps
diff /tmp/out.bin fixtures/outbound.golden  # must be silent
```

`--no-timestamps` zeros every `emit_ts` / `recv_ts` field on the
emitted outbound messages before encoding — the spec-documented
escape hatch for the `pricelevel::Trade::new` wall-clock leak.
With the flag, two replays of the same inbound bytes produce a
byte-identical outbound stream regardless of when the replay ran.

### Benchmark

```bash
# Default budget — 5 s warmup + 10 s measurement, 100 samples per
# iteration. About 18 minutes wall-clock on Apple M4 Max.
cargo bench --bench add_cancel_mix

# Faster sanity run (1 s warmup + 3 s measurement, 10 samples).
cargo bench --bench add_cancel_mix -- \
    --warm-up-time 1 --measurement-time 3 --sample-size 10
```

Each iteration prints `p50 / p99 / p99.9 / p99.99 / max` to stdout
plus a Criterion HTML report at
`target/criterion/add_cancel_mix/step_per_op/report/index.html`.
Methodology, hardware, and current observed numbers live in
[`BENCH.md`](BENCH.md).

### Regenerate the inbound fixture

Developer-only — `gen_fixture` rewrites the recorded inbound
stream feeding the replay golden:

```bash
cargo run --bin gen_fixture -- fixtures/inbound.bin
cargo run --bin replay -- \
    fixtures/inbound.bin fixtures/outbound.golden --no-timestamps
```

CI never runs `gen_fixture`. The committed `fixtures/outbound.golden`
is the contract — both binaries and the proptest in
`crates/engine/tests/replay_equality.rs` defend against drift.

## Matching policies

### Price-time priority

Levels are stored in a `BTreeMap<Price, Arc<PriceLevel>>` per side;
iteration is sorted by price. Within a level the FIFO queue is
walked head-first via `PriceLevel::snapshot_orders()` — the
snapshot is sorted by the per-add timestamp we write from the
`Book`'s monotonic `seq` counter, which guarantees deterministic
FIFO ordering across runs and across processes.

**Why**: pricelevel's `iter_orders()` walks the underlying `DashMap`
and is hash-ordered ("iteration order is not guaranteed to be
stable"); we observed flake under load. The snapshot-once-per-level
approach trades one `Vec<Arc<_>>` allocation per level entry for
strict FIFO determinism. The Book-side `VecDeque<OrderId>` mirror
that eliminates the alloc is tracked as a follow-up.

### Self-trade prevention (cancel-both)

When the next maker on the queue belongs to the taker's account,
both sides are dropped: the maker is cancelled (an `StpCancellation`
record carries the residual qty and price), the walk halts, and
`MatchResult::taker_stp_cancelled` is set so the engine emits a
`Cancelled{SelfTradePrevented}` for the taker — regardless of TIF,
including `Gtc` and `Ioc`.

**Why cancel-both, not "cancel maker, fill taker against the next
queue entry"**: the cancel-both policy is the cleanest way to
prevent wash-trade-like patterns from appearing on the public
trade tape. It is also operationally simple — a single signal
covers both sides — and it is the policy used by every venue
the spec references in `doc/DESIGN.md` § 5.2.

### Cancel-replace

Strict qty-down with no price change preserves FIFO position via
`pricelevel::UpdateQuantity` in place — `ReplaceOutcome { kept_priority: true }`.
Any price change or qty-up loses priority: the existing order is
cancelled and the new `(price, qty)` is added at the tail of the
new level — `kept_priority: false`.

**Why qty-up loses priority**: bumping a resting order's qty up at
the head of the queue would let a passive participant claim a
priority advantage they did not earn. Cancel-and-readd is the
unambiguous way to enforce that. Same rationale for any price
change: the new price's queue is unrelated to the old one.

### Market order in a thin book

A market order against an empty opposite side is rejected with
`MarketOrderInEmptyBook` before any matching state is touched.
This is a risk-layer reject (`crates/risk/`), not a matching one —
the engine reads `best_opposite` once and dispatches.

**Why**: arriving in an empty book and "fishing" for a fill at the
opposite of any future quote is a degenerate case; the spec
prefers an explicit reject over silent rest-as-limit semantics.

### Locked / crossed book

The matching path never produces a locked or crossed book on its
own — every aggressive order walks the opposite side until either
it is filled, the opposite side stops crossing, or STP halts the
walk. A passive `Limit` order at a level that already crosses is a
spec-level error condition (the gateway / wire decoder enforces no
such order can arrive); inside `match_aggressive`, the loop
verifies `taker_price >= level_price` (Bid) / `<=` (Ask) at every
level entry, so by construction the post-step book has
`best_bid < best_ask`.

### Post-only

`Tif::PostOnly` orders are subject to a "would-cross" gate at the
top of `match_aggressive`. If the order would cross at arrival the
gate emits `Rejected{PostOnlyWouldCross}` with **zero state
mutation** — no fills, no STP cancels, no level changes. Otherwise
the order rests in the regular `Gtc` path.

**Why pre-match gate, not post-walk reject**: the post-walk
implementation lets the order partial-fill before noticing it
crossed, leaking maker rebate on a market that the venue
explicitly told the participant they did not want to take. The
pre-match gate is the conservative, correctness-preserving choice.

## Risk controls

`RiskState` (in `crates/risk/`) is single-writer, owned by the
engine. Every inbound client command goes through one of the
`check_*` methods before the matching layer touches state.

Check order matches `doc/DESIGN.md` § 7:

1. **Kill switch**.
2. **Message validation** (handled at the wire-decode boundary;
   any failure surfaces as `MalformedMessage`).
3. **Price band** — `|price - last_trade_price| * 10_000 <= last *
   PRICE_BAND_BPS`. Skipped while `last_trade_price` is `None`
   (warm-up). Integer-only `i128` / `u128` arithmetic — no floats.
4. **Per-account limits** — `MAX_OPEN_ORDERS_PER_ACCT` and
   `MAX_NOTIONAL_PER_ACCT`. Market orders use the engine-supplied
   `best_opposite` price as the "far-touch worst-case" reference;
   a market order arriving against a one-sided book is rejected
   with `MarketOrderInEmptyBook` *before* the limit check.

### Configuration

Constants live in `crates/domain/src/consts.rs`:

| Constant | Value | Notes |
|----------|-------|-------|
| `TICK_SIZE` | `1` | Smallest legal price increment, in raw ticks. |
| `LOT_SIZE` | `1` | Smallest legal qty increment, in raw lots. |
| `PRICE_BAND_BPS` | `500` | ±5 % band relative to last trade. `1 bp = 0.01 %`. |
| `MAX_OPEN_ORDERS_PER_ACCT` | `1_000` | Hard ceiling on resting orders per account. |
| `MAX_NOTIONAL_PER_ACCT` | `10_000_000_000` | Hard ceiling on aggregate resting notional, in `price_ticks * qty_lots` units. `u128` to avoid overflow on market-order pre-trade checks. |
| `SYMBOL` | `"BTC-USDC"` | Single-symbol venue identifier; v1 is single-symbol by design. |

### Rejection taxonomy

Numeric discriminants are wire-stable (encoded into
`ExecReport.reject_reason`). New reasons are appended; never
renumbered.

| Code | Name | Layer | Meaning |
|-----:|------|-------|---------|
| 1 | `KillSwitched` | risk | Kill switch engaged. |
| 2 | `InvalidPrice` | wire | Price field outside legal range (`<= 0`). |
| 3 | `InvalidQty` | wire | Qty field outside legal range (`= 0`). |
| 4 | `TickViolation` | wire | Price not a multiple of `TICK_SIZE`. |
| 5 | `LotViolation` | wire | Qty not a multiple of `LOT_SIZE`. |
| 6 | `PriceBand` | risk | Price differs from last trade by more than `PRICE_BAND_BPS`. |
| 7 | `MaxOpenOrders` | risk | Account at `MAX_OPEN_ORDERS_PER_ACCT`. |
| 8 | `MaxNotional` | risk | Adding this order would exceed `MAX_NOTIONAL_PER_ACCT`. |
| 9 | `UnknownOrderId` | matching | Cancel / cancel-replace targeted an unknown id. |
| 10 | `DuplicateOrderId` | matching | New order's id already exists for this account. |
| 11 | `PostOnlyWouldCross` | matching | Post-only order would cross on arrival. |
| 12 | `SelfTradePrevented` | matching | STP fired (cancel-both). |
| 13 | `MarketOrderInEmptyBook` | risk | Market order against an empty opposite side. |
| 14 | `UnknownAccount` | gateway | Inbound referenced an account not bound to the session. |
| 15 | `MalformedMessage` | wire | Inbound payload failed wire-protocol validation. |

The `255` discriminant is reserved as a sentinel for forward-
compatible decode of future schema versions.

## Assumptions & tradeoffs

- **Single symbol.** The engine tracks one book; multi-symbol is
  not supported in v1. The single-symbol shape is intentional —
  it lets the matching core be the single writer with no
  per-symbol partitioning concerns.
- **In-memory state.** No persistence, no networking heroics
  beyond TCP. Recovery is via replay of the recorded inbound
  log against `fixtures/outbound.golden`. Spec-aligned per
  CLAUDE.md.
- **`--no-timestamps` replay flag.** `pricelevel::Trade::new`
  reads `SystemTime::now()`. The matching crate never reads back
  pricelevel's `Trade.timestamp`, so the leak does not affect
  matching control flow — but to keep the outbound diff
  byte-identical without forking the upstream library, replay
  zeros the timestamp fields before encoding. Byte-identical
  replay with timestamps included is a follow-up (wrap or fork
  upstream).
- **Vendored crates.** `pricelevel` (atomics + `DashMap` per
  level) and `IronSBE` (zero-copy buffers, async transport) are
  path deps. Their relaxations of CLAUDE.md core invariants are
  documented in CLAUDE.md § Third-party crates and confined to
  the matching / gateway boundaries respectively.
- **Tokio confined to gateway.** Matching, risk, domain, wire,
  and marketdata are pure blocking. Gateway is the single
  consumer of `tokio` — the boundary is structural, enforced by
  Cargo dep edges.
- **No floats anywhere on the matching path.** Prices and
  quantities are integer-only. Notional is `u128`. Risk band
  arithmetic is integer-only.
- **Ingress recorder is a no-op when disabled.** When the
  `--record` flag is absent, the gateway never instantiates a
  `Recorder`, so the listener's hot loop has no
  `Option<&mut Recorder>` branch to guard.

The microstructure write-up — CLOB vs batch auction vs RFQ, plus
honest limitations — lives in the section landing under issue #19.
