//! `SnapshotResponse` (kind = 0x69 / 105) — variable-length book +
//! engine state dump emitted in response to an inbound
//! `SnapshotRequest`.
//!
//! This is the only outbound message in v1 with a non-fixed payload
//! length. Layout (all little-endian):
//!
//! ```text
//! offset  size  field
//! ------  ----  -----
//!   0      8    engine_seq
//!   8      8    request_id     (echoed from the request)
//!  16      8    recv_ts        (engine-stamped from the request)
//!  24      8    emit_ts        (engine-stamped at emission)
//!  32      4    n_bid_levels   (u32 LE)
//!  36      4    n_ask_levels   (u32 LE)
//!  40      ?    bid levels  — best first (descending price)
//!   ?      ?    ask levels  — best first (ascending price)
//! ```
//!
//! Each level is `[i64 LE price][u64 LE qty]`, 16 bytes. Total payload
//! size = `40 + 16 * (n_bid_levels + n_ask_levels)`.
//!
//! The framing prefix outside the payload is the standard
//! `[u32 LE total][u8 kind = 0x69]`.

use domain::{EngineSeq, Price, Qty, RecvTs};

use crate::WireError;
use crate::error::to_wire;

/// Per-level entry in the snapshot — `(price, total_qty)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// Price of the level.
    pub price: Price,
    /// Aggregate qty resting at this level.
    pub qty: Qty,
}

/// Domain-typed snapshot response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotResponse {
    /// Engine sequence number. Strictly monotonic across all outbound.
    pub engine_seq: EngineSeq,
    /// Echo of the request's correlation id.
    pub request_id: u64,
    /// Engine-stamped recv timestamp from the request.
    pub recv_ts: RecvTs,
    /// Engine-stamped emit timestamp at snapshot construction.
    pub emit_ts: RecvTs,
    /// Bid side, best first (descending price).
    pub bids: Vec<Level>,
    /// Ask side, best first (ascending price).
    pub asks: Vec<Level>,
}

const HEADER_BYTES: usize = 40;
const LEVEL_BYTES: usize = 16;

/// Compute the encoded payload length without allocating.
#[must_use]
pub fn encoded_len(msg: &SnapshotResponse) -> usize {
    HEADER_BYTES + LEVEL_BYTES * (msg.bids.len() + msg.asks.len())
}

/// Encode a snapshot into `out`. Appends `encoded_len(msg)` bytes.
pub fn encode(msg: &SnapshotResponse, out: &mut Vec<u8>) {
    out.extend_from_slice(&msg.engine_seq.as_raw().to_le_bytes());
    out.extend_from_slice(&msg.request_id.to_le_bytes());
    out.extend_from_slice(&(msg.recv_ts.as_nanos() as u64).to_le_bytes());
    out.extend_from_slice(&(msg.emit_ts.as_nanos() as u64).to_le_bytes());
    let n_bid = u32::try_from(msg.bids.len()).expect("level count fits in u32");
    let n_ask = u32::try_from(msg.asks.len()).expect("level count fits in u32");
    out.extend_from_slice(&n_bid.to_le_bytes());
    out.extend_from_slice(&n_ask.to_le_bytes());
    for level in &msg.bids {
        out.extend_from_slice(&level.price.as_ticks().to_le_bytes());
        out.extend_from_slice(&level.qty.as_lots().to_le_bytes());
    }
    for level in &msg.asks {
        out.extend_from_slice(&level.price.as_ticks().to_le_bytes());
        out.extend_from_slice(&level.qty.as_lots().to_le_bytes());
    }
}

/// Decode a snapshot from a payload.
///
/// # Errors
/// [`WireError::PayloadSize`] when the payload is shorter than the
/// declared level count, or when the trailing bytes do not match
/// `HEADER + LEVEL * (n_bid + n_ask)` exactly.
/// [`WireError::Domain`] when a price / qty fails domain validation.
pub fn parse(payload: &[u8]) -> Result<SnapshotResponse, WireError> {
    if payload.len() < HEADER_BYTES {
        return Err(WireError::PayloadSize {
            expected: HEADER_BYTES,
            got: payload.len(),
        });
    }
    let engine_seq = EngineSeq::new(u64::from_le_bytes(
        payload[0..8].try_into().expect("8 bytes"),
    ));
    let request_id = u64::from_le_bytes(payload[8..16].try_into().expect("8 bytes"));
    let recv_ts =
        RecvTs::new(u64::from_le_bytes(payload[16..24].try_into().expect("8 bytes")) as i64);
    let emit_ts =
        RecvTs::new(u64::from_le_bytes(payload[24..32].try_into().expect("8 bytes")) as i64);
    let n_bid = u32::from_le_bytes(payload[32..36].try_into().expect("4 bytes")) as usize;
    let n_ask = u32::from_le_bytes(payload[36..40].try_into().expect("4 bytes")) as usize;
    let expected = HEADER_BYTES + LEVEL_BYTES * (n_bid + n_ask);
    if payload.len() != expected {
        return Err(WireError::PayloadSize {
            expected,
            got: payload.len(),
        });
    }
    let mut bids = Vec::with_capacity(n_bid);
    let mut asks = Vec::with_capacity(n_ask);
    let mut cursor = HEADER_BYTES;
    for _ in 0..n_bid {
        let price_raw =
            i64::from_le_bytes(payload[cursor..cursor + 8].try_into().expect("8 bytes"));
        let qty_raw = u64::from_le_bytes(
            payload[cursor + 8..cursor + 16]
                .try_into()
                .expect("8 bytes"),
        );
        bids.push(Level {
            price: Price::new(price_raw).map_err(to_wire)?,
            qty: Qty::new(qty_raw).map_err(to_wire)?,
        });
        cursor += LEVEL_BYTES;
    }
    for _ in 0..n_ask {
        let price_raw =
            i64::from_le_bytes(payload[cursor..cursor + 8].try_into().expect("8 bytes"));
        let qty_raw = u64::from_le_bytes(
            payload[cursor + 8..cursor + 16]
                .try_into()
                .expect("8 bytes"),
        );
        asks.push(Level {
            price: Price::new(price_raw).map_err(to_wire)?,
            qty: Qty::new(qty_raw).map_err(to_wire)?,
        });
        cursor += LEVEL_BYTES;
    }
    Ok(SnapshotResponse {
        engine_seq,
        request_id,
        recv_ts,
        emit_ts,
        bids,
        asks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(price: i64, qty: u64) -> Level {
        Level {
            price: Price::new(price).expect("ok"),
            qty: Qty::new(qty).expect("ok"),
        }
    }

    fn sample() -> SnapshotResponse {
        SnapshotResponse {
            engine_seq: EngineSeq::new(42),
            request_id: 7,
            recv_ts: RecvTs::new(1_000),
            emit_ts: RecvTs::new(1_001),
            bids: vec![level(100, 5), level(99, 10)],
            asks: vec![level(101, 3), level(102, 7), level(103, 2)],
        }
    }

    #[test]
    fn test_snapshot_response_roundtrip() {
        let msg = sample();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(buf.len(), encoded_len(&msg));
        let decoded = parse(&buf).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_snapshot_response_empty_book() {
        let msg = SnapshotResponse {
            engine_seq: EngineSeq::new(1),
            request_id: 0,
            recv_ts: RecvTs::new(0),
            emit_ts: RecvTs::new(0),
            bids: Vec::new(),
            asks: Vec::new(),
        };
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(buf.len(), HEADER_BYTES);
        let decoded = parse(&buf).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_snapshot_response_truncated_returns_size_err() {
        let buf = [0u8; HEADER_BYTES - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_snapshot_response_invalid_level_count_returns_size_err() {
        // Header declares 2 bid levels and 0 ask levels, but body has
        // only 16 bytes (one level worth).
        let mut buf = vec![0u8; HEADER_BYTES];
        buf[32..36].copy_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; LEVEL_BYTES]); // only 1 level body
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }
}
