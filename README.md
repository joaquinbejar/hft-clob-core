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
| 105  | 0x69  | `SnapshotResponse`  | variable     | Variable-length book + engine state dump in response to `SnapshotRequest`. Layout: `[engine_seq u64][request_id u64][recv_ts u64][emit_ts u64][n_bid u32][n_ask u32][bid levels][ask levels]`, each level `[price i64][qty u64]`. Bids best-first (descending), asks best-first (ascending). |

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

The single-binary deployment is the `engine` crate's `engine`
binary, which spawns the tokio gateway listener and the dedicated
matching thread.

```bash
# Run the test suite end-to-end (matching, risk, engine, wire,
# marketdata, gateway, replay):
cargo nextest run

# Verify the workspace builds in release with LTO.
cargo build --release

# Run the engine listener on the default 0.0.0.0:9000.
cargo run --release --bin engine
```

### Docker — zero-host-setup path

```bash
# Bring up the engine listener (foreground; ^C to stop).
docker compose -f docker/docker-compose.yml up engine

# Run the small `add_cancel_mix` smoke bench (10 k ops; prints
# p50 / p99 / p99.9 / p99.99 / max plus throughput).
docker compose -f docker/docker-compose.yml run --rm bench

# Run the replay binary against the committed fixture.
docker compose -f docker/docker-compose.yml run --rm replay
```

The runtime image is `debian:bookworm-slim` plus three release
binaries (`engine`, `smoke_bench`, `replay`) and the committed
`fixtures/`, well under 300 MB. No host-side install beyond
Docker.

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

## Microstructure

A continuous central-limit order book is the right shape for a
single-symbol on-chain-settled spot venue **only if** the trade
tape is the source of truth and the venue can absorb cancel /
amend bursts without leaking the resting queue. Below is the
short version of the design pressure that landed us here.

### Why price-time priority + cancel-both STP

Price-time is the only allocation rule that survives both
adversarial inspection and naive scrutiny without invoking
participant-specific logic (size pro-rata privileges large
desks; volume-weighted pro-rata privileges momentum). FIFO
within a price level is information-cheap: every participant
knows the rank of their order from the ack alone, no model
of the book microstate required.

Self-trade prevention is **cancel-both** (drop the resting
maker, halt the walk, cancel the taker irrespective of TIF).
The alternative — "cancel the maker, fill the taker against
the next queue entry" — is operationally simpler but it lets
an account use its own resting maker as a free trigger to
skip the queue against an unrelated counterparty (the
maker's residual evaporates the moment the same-account
taker arrives, so the taker's effective priority jumps). That
is a priority-jump primitive that none of the legitimate
order types we ship can build, and cancel-both denies it
entirely. The cost is a slightly worse fill on shallow books;
the benefit is no class of trade that exists only because the
participant owned both sides.

### CLOB vs frequent batch auction vs RFQ for a ZK-settled crypto venue

A frequent batch auction (FBA) — Budish-style, 100 ms cadence
or similar — does genuinely eliminate the *intra-batch*
latency race by construction: every inbound that lands in the
batch clears at one uniform price, so there is no time
priority to game once the batch closes. The case against it
for *this* deployment is that intra-batch fairness is not the
threat model we are defending:

1. **The threat model is verifiable post-hoc reconstruction.**
   On a ZK-settled venue the trust anchor downstream is the
   prover, not the matching policy — every participant
   eventually verifies what cleared. The microstructure layer's
   job is to make the inbound → outbound mapping legible and
   reproducible, which `fixtures/outbound.golden` demonstrates
   end-to-end. An FBA shifts the within-batch race away but
   does not remove sequencer trust: the sequencer still picks
   which inbound goes into which batch, and the
   inclusion-time-vs-clearing-time gap is the surface that an
   adversarial sequencer (or a co-located arbitrageur with a
   private feed of inclusion decisions) exploits. Legibility of
   the continuous tape closes that surface; uniform-price
   clearing inside the batch does not.
2. **ZK proving cadence vs auction cadence.** The settlement
   layer's proof-generation cadence (single-digit seconds at
   best for current SNARK stacks on a non-trivial program) is
   already an order of magnitude slower than an FBA's natural
   100 ms tick. Layering an FBA *on top* of an already-batched
   ZK rollup leaves you with two synchronisation regimes whose
   offsets you have to reason about; the latency advantage of
   the inner FBA is then bounded by the outer prover's slack.
   For a venue whose committed delivery is "trade now, settle
   in a ZK proof later," continuous matching gives the
   participant a price they can quote against immediately and
   the prover catches up asynchronously.

RFQ is the other plausible shape — appropriate for a venue
where the order flow is overwhelmingly large block trades and
the cost of revealing intent on the tape exceeds the value of
the public price discovery. Spot crypto on a single retail-tier
symbol is the opposite regime: order count is high, average
size is small, and the public tape is a feature for everyone
who is not a block-flow specialist. CLOB wins on the spec's
target market.

### Honest limitation: per-level snapshot allocation on the fill loop

The matching core is single-writer and `Engine::step` runs
~40 ns p50 (see [`BENCH.md`](BENCH.md)). The current p99.9
tail (~210 µs) lives almost entirely in one place: every level
the fill loop enters calls `pricelevel::PriceLevel::snapshot_orders()`,
which iterates the level's `DashMap` and `sort_by_key` into a
fresh `Vec<Arc<OrderType<()>>>`. We pay that cost because
pricelevel's `iter_orders()` walks the DashMap in hash order
and is explicitly documented as not FIFO-stable — we observed
it in CI as flake on a same-price multi-maker STP test. Strict
price-time priority is non-negotiable, so the snapshot is the
v1 trade-off.

The evolution is a Book-side `BTreeMap<Price, VecDeque<OrderId>>`
mirror that we own and append to from `add_resting`. The fill
loop then peeks the FIFO front in O(1) and pops on full fill,
no Arc clones, no allocation. The matching crate already
holds the per-order metadata sidecar (`HashMap<OrderId,
OrderMeta>`); adding the per-level FIFO is structural, not a
new abstraction. After that lands the next contention point
moves to the outbound side: the engine's broadcast send
currently walks every subscriber serially on the engine
thread, so as session count grows the engine's tail becomes a
function of N. The clean shape is an SPMC ring per *kind* of
stream (trades + book updates fanning out from one producer;
per-session exec reports staying paired with their session) so
the engine's latency is a function of `num_kinds` rather than
`num_sessions`. Both of these land cleanly because matching
binds to `OutboundSink`, not to the broadcast channel — the
topology change is a sink swap in one place.
