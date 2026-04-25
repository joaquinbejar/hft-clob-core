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

/// Domain-typed `BookUpdateTop`.
///
/// `bid` and `ask` are paired `Option<(Price, Qty)>` so a half-empty
/// side cannot be represented in the type system. The wire format
/// expresses "side empty" as `(price = 0, qty = 0)` on that side; any
/// `(0, n)` or `(n, 0)` mix at decode time is rejected with
/// [`WireError::InconsistentPayload`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookUpdateTop {
    /// Engine-assigned sequence number.
    pub engine_seq: EngineSeq,
    /// Best bid: `Some((price, qty))` when the bid side is non-empty,
    /// `None` otherwise.
    pub bid: Option<(Price, Qty)>,
    /// Best ask: `Some((price, qty))` when the ask side is non-empty,
    /// `None` otherwise.
    pub ask: Option<(Price, Qty)>,
    /// Engine emit timestamp.
    pub emit_ts: RecvTs,
}

#[inline]
fn decode_side(
    price: i64,
    qty: u64,
    side: &'static str,
) -> Result<Option<(Price, Qty)>, WireError> {
    match (price, qty) {
        (0, 0) => Ok(None),
        (p, q) if p != 0 && q != 0 => {
            let price = Price::new(p).map_err(to_wire)?;
            let qty = Qty::new(q).map_err(to_wire)?;
            Ok(Some((price, qty)))
        }
        _ => Err(WireError::InconsistentPayload(side)),
    }
}

#[inline]
fn encode_side(side: Option<(Price, Qty)>) -> (i64, u64) {
    match side {
        Some((p, q)) => (p.as_ticks(), q.as_lots()),
        None => (0, 0),
    }
}

/// Decode `BookUpdateTop` from a payload.
///
/// # Errors
/// [`WireError::PayloadSize`] on size mismatch.
/// [`WireError::Domain`] when a non-zero price / qty fails domain validation.
/// [`WireError::InconsistentPayload`] when only one of `(price, qty)` on
/// a side is zero — the wire format requires both or neither.
pub fn parse(payload: &[u8]) -> Result<BookUpdateTop, WireError> {
    let w = BookUpdateTopWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let bid = decode_side(w.bid_price, w.bid_qty, "BookUpdateTop: half-empty bid side")?;
    let ask = decode_side(w.ask_price, w.ask_qty, "BookUpdateTop: half-empty ask side")?;
    Ok(BookUpdateTop {
        engine_seq: EngineSeq::new(w.engine_seq),
        bid,
        ask,
        emit_ts: RecvTs::new(w.emit_ts as i64),
    })
}

/// Encode `BookUpdateTop` into a payload buffer. The paired
/// [`BookUpdateTop::bid`] / [`BookUpdateTop::ask`] guarantees an empty
/// side is encoded as `(0, 0)`; half-empty wire payloads are
/// unrepresentable.
pub fn encode(msg: &BookUpdateTop, out: &mut Vec<u8>) {
    let (bid_price, bid_qty) = encode_side(msg.bid);
    let (ask_price, ask_qty) = encode_side(msg.ask);
    let w = BookUpdateTopWire {
        engine_seq: msg.engine_seq.as_raw(),
        bid_price,
        bid_qty,
        ask_price,
        ask_qty,
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
            bid: Some((Price::new(99).expect("ok"), Qty::new(5).expect("ok"))),
            ask: Some((Price::new(101).expect("ok"), Qty::new(7).expect("ok"))),
            emit_ts: RecvTs::new(1_000_000),
        }
    }

    fn sample_one_sided() -> BookUpdateTop {
        BookUpdateTop {
            engine_seq: EngineSeq::new(101),
            bid: Some((Price::new(99).expect("ok"), Qty::new(5).expect("ok"))),
            ask: None,
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
        assert!(decoded.ask.is_none());
        assert_eq!(decoded.bid, msg.bid);
    }

    #[test]
    fn test_book_update_top_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    /// Wire payload with `bid_price = 100`, `bid_qty = 0` is malformed.
    /// The decoder must reject rather than silently materialising one
    /// half of a side.
    #[test]
    fn test_book_update_top_half_empty_bid_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_two_sided(), &mut buf);
        // bid_qty offset = 16 (after engine_seq + bid_price); set to 0
        for byte in &mut buf[16..24] {
            *byte = 0;
        }
        assert!(matches!(
            parse(&buf),
            Err(WireError::InconsistentPayload(_))
        ));
    }

    /// Symmetrical: `ask_price = 0`, `ask_qty = N` is malformed too.
    #[test]
    fn test_book_update_top_half_empty_ask_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_two_sided(), &mut buf);
        // ask_price offset = 24; set to 0 while ask_qty stays non-zero
        for byte in &mut buf[24..32] {
            *byte = 0;
        }
        assert!(matches!(
            parse(&buf),
            Err(WireError::InconsistentPayload(_))
        ));
    }
}
