//! `BookUpdateTop` (kind = 0x67 / 103).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{EngineSeq, Price, Qty, RecvTs};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 48 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct BookUpdateTopWire {
    /// Strictly monotonic engine sequence number.
    pub engine_seq: u64,
    /// Best bid price in ticks; `0` when the bid side is empty.
    pub bid_price: i64,
    /// Aggregate qty at the best bid in lots; `0` when the bid side
    /// is empty.
    pub bid_qty: u64,
    /// Best ask price in ticks; `0` when the ask side is empty.
    pub ask_price: i64,
    /// Aggregate qty at the best ask in lots; `0` when the ask side
    /// is empty.
    pub ask_qty: u64,
    /// Engine emit timestamp.
    pub emit_ts: u64,
}

const _: () = assert!(core::mem::size_of::<BookUpdateTopWire>() == 48);

const SIZE: usize = core::mem::size_of::<BookUpdateTopWire>();

/// Domain-typed `BookUpdateTop`. A side without resting orders carries
/// `None` for both `price` and `qty` on that side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookUpdateTop {
    /// Engine-assigned sequence number.
    pub engine_seq: EngineSeq,
    /// Best bid price; `None` when the bid side is empty.
    pub bid_price: Option<Price>,
    /// Aggregate qty at the best bid; `None` when the bid side is empty.
    pub bid_qty: Option<Qty>,
    /// Best ask price; `None` when the ask side is empty.
    pub ask_price: Option<Price>,
    /// Aggregate qty at the best ask; `None` when the ask side is empty.
    pub ask_qty: Option<Qty>,
    /// Engine emit timestamp.
    pub emit_ts: RecvTs,
}

/// Decode `BookUpdateTop` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch or domain validation.
pub fn parse(payload: &[u8]) -> Result<BookUpdateTop, WireError> {
    let w = BookUpdateTopWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let bid_price = if w.bid_price != 0 {
        Some(Price::new(w.bid_price).map_err(to_wire)?)
    } else {
        None
    };
    let bid_qty = if w.bid_qty != 0 {
        Some(Qty::new(w.bid_qty).map_err(to_wire)?)
    } else {
        None
    };
    let ask_price = if w.ask_price != 0 {
        Some(Price::new(w.ask_price).map_err(to_wire)?)
    } else {
        None
    };
    let ask_qty = if w.ask_qty != 0 {
        Some(Qty::new(w.ask_qty).map_err(to_wire)?)
    } else {
        None
    };
    Ok(BookUpdateTop {
        engine_seq: EngineSeq::new(w.engine_seq),
        bid_price,
        bid_qty,
        ask_price,
        ask_qty,
        emit_ts: RecvTs::new(w.emit_ts as i64),
    })
}

/// Encode `BookUpdateTop` into a payload buffer.
pub fn encode(msg: &BookUpdateTop, out: &mut Vec<u8>) {
    let w = BookUpdateTopWire {
        engine_seq: msg.engine_seq.as_raw(),
        bid_price: msg.bid_price.map(Price::as_ticks).unwrap_or(0),
        bid_qty: msg.bid_qty.map(Qty::as_lots).unwrap_or(0),
        ask_price: msg.ask_price.map(Price::as_ticks).unwrap_or(0),
        ask_qty: msg.ask_qty.map(Qty::as_lots).unwrap_or(0),
        emit_ts: msg.emit_ts.as_nanos() as u64,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_two_sided() -> BookUpdateTop {
        BookUpdateTop {
            engine_seq: EngineSeq::new(100),
            bid_price: Some(Price::new(99).expect("ok")),
            bid_qty: Some(Qty::new(5).expect("ok")),
            ask_price: Some(Price::new(101).expect("ok")),
            ask_qty: Some(Qty::new(7).expect("ok")),
            emit_ts: RecvTs::new(1_000_000),
        }
    }

    fn sample_one_sided() -> BookUpdateTop {
        BookUpdateTop {
            engine_seq: EngineSeq::new(101),
            bid_price: Some(Price::new(99).expect("ok")),
            bid_qty: Some(Qty::new(5).expect("ok")),
            ask_price: None,
            ask_qty: None,
            emit_ts: RecvTs::new(1_000_001),
        }
    }

    #[test]
    fn test_book_update_top_wire_size_is_48() {
        assert_eq!(SIZE, 48);
    }

    #[test]
    fn test_book_update_top_two_sided_roundtrip() {
        let msg = sample_two_sided();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_book_update_top_one_sided_decodes_with_none() {
        let msg = sample_one_sided();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        let decoded = parse(&buf).expect("decode");
        assert!(decoded.ask_price.is_none());
        assert!(decoded.ask_qty.is_none());
        assert_eq!(decoded.bid_price, msg.bid_price);
    }

    #[test]
    fn test_book_update_top_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }
}
