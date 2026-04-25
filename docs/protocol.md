# Wire Protocol

Single source of truth for the on-wire format of `hft-clob-core`. Any
encoder or decoder change must update this document in the same commit.

## Design constants

| Constant         | Value                                              |
|------------------|----------------------------------------------------|
| Endianness       | little-endian (target is x86_64; avoids per-field byte swaps on the hot path). Enforced at compile time — `crates/wire/src/lib.rs` carries a `compile_error!` on `target_endian = "big"` |
| Framing          | `len: u32 \| kind: u8 \| payload: [u8; len - 1]`   |
| Alignment        | packed (`#[repr(C, packed)]`); decoder rejects non-zero padding |
| Strings          | not used; all identifiers are fixed-width integers |
| Timestamps       | wire-level `u64` nanoseconds; carried opaque, the matching engine never reads them for control flow |
| Price / Qty      | `i64` ticks / `u64` lots; see `domain::consts::{TICK_SIZE, LOT_SIZE}` |
| Wire version     | `WIRE_VERSION = 1`. Bump on any layout change      |

The `len` field counts bytes from the `kind` byte through the end of the
payload (i.e. `len = 1 + payload.len()`). A frame is therefore `4 + len`
bytes total. Decoders that encounter an unknown `kind` advance `len`
bytes and continue — see `Frame::parse_or_skip` in `crates/wire/src/framing.rs`.

### Timestamp representation

The wire field is `u64`. The corresponding domain newtypes
(`domain::ClientTs`, `domain::RecvTs`) are `i64`. Encode and decode
perform a bit-preserving `as` cast between the two:

- Encode: `i64 as u64` reinterprets the two's-complement bits.
- Decode: `u64 as i64` reinterprets the same bits back.

A negative `i64` timestamp therefore encodes as a `u64` value above
`i64::MAX`. The cast is lossless and the roundtrip is byte-identical.
The matching core never reads the value for control flow, so the
signed / unsigned interpretation is irrelevant beyond storage; clients
that read the wire as `u64` and the engine that reads it as `i64`
agree on every bit.

## Message-kind table

| Kind | Hex   | Direction | Name                | Size (kind + payload) |
|------|-------|-----------|---------------------|-----------------------|
| 1    | 0x01  | inbound   | `NewOrder`          | 41 bytes              |
| 2    | 0x02  | inbound   | `CancelOrder`       | 25 bytes              |
| 3    | 0x03  | inbound   | `CancelReplace`     | 41 bytes              |
| 4    | 0x04  | inbound   | `MassCancel`        | 17 bytes              |
| 5    | 0x05  | inbound   | `KillSwitchSet`     | 25 bytes              |
| 6    | 0x06  | inbound   | `SnapshotRequest`   | 17 bytes              |
| 101  | 0x65  | outbound  | `ExecReport`        | 65 bytes              |
| 102  | 0x66  | outbound  | `TradePrint`        | 49 bytes              |
| 103  | 0x67  | outbound  | `BookUpdateTop`     | 49 bytes              |
| 104  | 0x68  | outbound  | `BookUpdateL2Delta` | 41 bytes              |

`SnapshotResponse` (kind 0x69 / 105) is intentionally absent — the
variable-length book depth array conflicts with the bespoke fixed-size
pattern this crate uses, so it lives behind a separate framing scheme
to be added under a later issue.

## Inbound layouts

### `NewOrder` (kind = 0x01)

| Offset | Size | Field        | Type | Notes                                  |
|-------:|-----:|--------------|------|----------------------------------------|
| 0      | 8    | `client_ts`  | u64  | nanoseconds; opaque to the engine      |
| 8      | 8    | `order_id`   | u64  | client-assigned, non-zero, unique/acct |
| 16     | 4    | `account_id` | u32  | non-zero                               |
| 20     | 1    | `side`       | u8   | `1 = Bid`, `2 = Ask`                   |
| 21     | 1    | `order_type` | u8   | `1 = Limit`, `2 = Market`              |
| 22     | 1    | `tif`        | u8   | `1 = GTC`, `2 = IOC`, `3 = POST_ONLY`  |
| 23     | 1    | `_pad0`      | u8   | zero; reserved                         |
| 24     | 8    | `price`      | i64  | ticks; ignored when `order_type = Market` |
| 32     | 8    | `qty`        | u64  | lots; > 0                              |
| —      | 40   | **total**    |      |                                        |

### `CancelOrder` (kind = 0x02)

| Offset | Size | Field        | Type | Notes                  |
|-------:|-----:|--------------|------|------------------------|
| 0      | 8    | `client_ts`  | u64  |                        |
| 8      | 8    | `order_id`   | u64  | non-zero               |
| 16     | 4    | `account_id` | u32  | non-zero               |
| 20     | 4    | `_pad0`      | u32  | zero                   |
| —      | 24   | **total**    |      |                        |

### `CancelReplace` (kind = 0x03)

| Offset | Size | Field        | Type | Notes                                |
|-------:|-----:|--------------|------|--------------------------------------|
| 0      | 8    | `client_ts`  | u64  |                                      |
| 8      | 8    | `order_id`   | u64  | resting order to be replaced         |
| 16     | 4    | `account_id` | u32  | must own `order_id`                  |
| 20     | 4    | `_pad0`      | u32  | zero                                 |
| 24     | 8    | `new_price`  | i64  | ticks                                |
| 32     | 8    | `new_qty`    | u64  | lots; `> 0`                          |
| —      | 40   | **total**    |      |                                      |

Priority semantics (per `doc/DESIGN.md` § 5.3): price change → new
priority; qty up → new priority; qty down → in-place, priority preserved.

### `MassCancel` (kind = 0x04)

| Offset | Size | Field        | Type | Notes                  |
|-------:|-----:|--------------|------|------------------------|
| 0      | 8    | `client_ts`  | u64  |                        |
| 8      | 4    | `account_id` | u32  | cancels every resting order on this account |
| 12     | 4    | `_pad0`      | u32  | zero                   |
| —      | 16   | **total**    |      |                        |

### `KillSwitchSet` (kind = 0x05)

| Offset | Size | Field          | Type    | Notes                  |
|-------:|-----:|----------------|---------|------------------------|
| 0      | 8    | `client_ts`    | u64     |                        |
| 8      | 8    | `admin_token`  | u64     | shared-secret check    |
| 16     | 1    | `state`        | u8      | `0 = resume`, `1 = halt` |
| 17     | 7    | `_pad0`        | [u8; 7] | all zero               |
| —      | 24   | **total**      |         |                        |

### `SnapshotRequest` (kind = 0x06)

| Offset | Size | Field         | Type    | Notes                  |
|-------:|-----:|---------------|---------|------------------------|
| 0      | 8    | `request_id`  | u64     | echoed in the response |
| 8      | 8    | `_pad0`       | [u8; 8] | all zero               |
| —      | 16   | **total**     |         |                        |

## Decode invariants

- Frame length must be exactly `4 + len` bytes; truncation returns
  `WireError::Truncated`.
- `kind` byte must match an assigned discriminant; otherwise
  `WireError::UnknownKind(byte)`.
- Every `_pad*` byte must be zero; otherwise
  `WireError::NonZeroPad(offset)`.
- Each integer field is parsed into its corresponding `domain::` newtype
  at the boundary; validation failures surface as `WireError::Domain(_)`.

## Outbound layouts

### `ExecReport` (kind = 0x65 / 101)

| Offset | Size | Field           | Type | Notes                                                                    |
|-------:|-----:|-----------------|------|--------------------------------------------------------------------------|
| 0      | 8    | `engine_seq`    | u64  | strictly monotonic across all outbound streams                           |
| 8      | 8    | `order_id`      | u64  |                                                                          |
| 16     | 4    | `account_id`    | u32  |                                                                          |
| 20     | 1    | `state`         | u8   | `ExecState`: `1=Accepted`, `2=Rejected`, `3=PartiallyFilled`, `4=Filled`, `5=Cancelled`, `6=Replaced` |
| 21     | 1    | `reject_reason` | u8   | `RejectReason` discriminant; `0` when `state != Rejected`                |
| 22     | 1    | `cancel_reason` | u8   | `CancelReason` discriminant; `0` when `state ∉ {Cancelled, Replaced}`    |
| 23     | 1    | `_pad0`         | u8   | zero                                                                     |
| 24     | 8    | `fill_price`    | i64  | ticks; `0` when no fill                                                  |
| 32     | 8    | `fill_qty`      | u64  | lots; `0` when no fill                                                   |
| 40     | 8    | `leaves_qty`    | u64  | lots; `0` after a terminal fill / cancel / reject                        |
| 48     | 8    | `recv_ts`       | u64  | echo of the inbound `recv_ts`                                            |
| 56     | 8    | `emit_ts`       | u64  | engine emit timestamp                                                    |
| —      | 64   | **total**       |      |                                                                          |

### `TradePrint` (kind = 0x66 / 102)

| Offset | Size | Field             | Type    | Notes                              |
|-------:|-----:|-------------------|---------|------------------------------------|
| 0      | 8    | `engine_seq`      | u64     |                                    |
| 8      | 8    | `trade_id`        | u64     | engine-internal monotonic          |
| 16     | 8    | `price`           | i64     | ticks; equals the maker price      |
| 24     | 8    | `qty`             | u64     | lots                               |
| 32     | 1    | `aggressor_side`  | u8      | `Side` discriminant                |
| 33     | 7    | `_pad0`           | [u8; 7] | all zero                           |
| 40     | 8    | `emit_ts`         | u64     |                                    |
| —      | 48   | **total**         |         |                                    |

### `BookUpdateTop` (kind = 0x67 / 103)

| Offset | Size | Field        | Type | Notes                                          |
|-------:|-----:|--------------|------|------------------------------------------------|
| 0      | 8    | `engine_seq` | u64  |                                                |
| 8      | 8    | `bid_price`  | i64  | ticks; `0` when the bid side is empty          |
| 16     | 8    | `bid_qty`    | u64  | lots; `0` when the bid side is empty           |
| 24     | 8    | `ask_price`  | i64  | ticks; `0` when the ask side is empty          |
| 32     | 8    | `ask_qty`    | u64  | lots; `0` when the ask side is empty           |
| 40     | 8    | `emit_ts`    | u64  |                                                |
| —      | 48   | **total**    |      |                                                |

### `BookUpdateL2Delta` (kind = 0x68 / 104)

| Offset | Size | Field        | Type    | Notes                                                  |
|-------:|-----:|--------------|---------|--------------------------------------------------------|
| 0      | 8    | `engine_seq` | u64     |                                                        |
| 8      | 8    | `price`      | i64     | level price in ticks                                   |
| 16     | 8    | `new_qty`    | u64     | new aggregate qty at this level; `0` removes the level |
| 24     | 1    | `side`       | u8      | `Side` discriminant                                    |
| 25     | 7    | `_pad0`      | [u8; 7] | all zero                                               |
| 32     | 8    | `emit_ts`    | u64     |                                                        |
| —      | 40   | **total**    |         |                                                        |

## Roundtrip property

`crates/wire/tests/roundtrip.rs` proptest matrix asserts, per message:

- `decode(encode(x)) == x` (value-identical), and
- `encode(decode(encode(x))) == encode(x)` (byte-identical).

`RejectReason` (15 variants), `CancelReason` (5), `ExecState` (6),
`Side`, `OrderType`, `Tif`, `KillSwitchState` are exhaustively covered
via `prop_oneof![all variants]` strategies.
