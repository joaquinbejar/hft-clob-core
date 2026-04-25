# hft-clob-core

Single-symbol central-limit-order-book (CLOB) matching engine in Rust.

> Status: in active development. The README is grown one section per
> implementation issue; sections marked **stub** will be expanded as
> their owning issue lands. Full architecture / how-to-run /
> microstructure write-up arrive with milestone **M8 Docs + Ship**
> (issues #18 / #19 / #20).

## Wire protocol *(stub — expanded in #5)*

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
