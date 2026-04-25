//! `TradePrint` (kind = 0x66 / 102).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{EngineSeq, Price, Qty, RecvTs, Side, TradeId};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 48 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct TradePrintWire {
    /// Strictly monotonic engine sequence number.
    pub engine_seq: u64,
    /// Engine-internal trade id.
    pub trade_id: u64,
    /// Trade price in ticks; equals the maker price.
    pub price: i64,
    /// Trade quantity in lots.
    pub qty: u64,
    /// `Side` discriminant of the aggressor.
    pub aggressor_side: u8,
    /// Reserved padding; every byte must be zero.
    pub _pad0: [u8; 7],
    /// Engine emit timestamp.
    pub emit_ts: u64,
}

const _: () = assert!(core::mem::size_of::<TradePrintWire>() == 48);

const SIZE: usize = core::mem::size_of::<TradePrintWire>();
const PAD0_OFFSET_BASE: usize = 33;

/// Domain-typed `TradePrint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradePrint {
    /// Engine-assigned sequence number.
    pub engine_seq: EngineSeq,
    /// Engine-internal trade id.
    pub trade_id: TradeId,
    /// Trade price.
    pub price: Price,
    /// Trade quantity.
    pub qty: Qty,
    /// Side of the aggressor.
    pub aggressor_side: Side,
    /// Engine emit timestamp.
    pub emit_ts: RecvTs,
}

/// Decode `TradePrint` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or domain validation.
pub fn parse(payload: &[u8]) -> Result<TradePrint, WireError> {
    let w = TradePrintWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    for (i, byte) in pad.iter().enumerate() {
        if *byte != 0 {
            return Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + i));
        }
    }
    Ok(TradePrint {
        engine_seq: EngineSeq::new(w.engine_seq),
        trade_id: TradeId::new(w.trade_id),
        price: Price::new(w.price).map_err(to_wire)?,
        qty: Qty::new(w.qty).map_err(to_wire)?,
        aggressor_side: Side::try_from(w.aggressor_side).map_err(to_wire)?,
        emit_ts: RecvTs::new(w.emit_ts as i64),
    })
}

/// Encode `TradePrint` into a payload buffer.
pub fn encode(msg: &TradePrint, out: &mut Vec<u8>) {
    let w = TradePrintWire {
        engine_seq: msg.engine_seq.as_raw(),
        trade_id: msg.trade_id.as_raw(),
        price: msg.price.as_ticks(),
        qty: msg.qty.as_lots(),
        aggressor_side: msg.aggressor_side.as_u8(),
        _pad0: [0; 7],
        emit_ts: msg.emit_ts.as_nanos() as u64,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TradePrint {
        TradePrint {
            engine_seq: EngineSeq::new(50),
            trade_id: TradeId::new(99),
            price: Price::new(1234).expect("ok"),
            qty: Qty::new(5).expect("ok"),
            aggressor_side: Side::Bid,
            emit_ts: RecvTs::new(1_000_000),
        }
    }

    #[test]
    fn test_trade_print_wire_size_is_48() {
        assert_eq!(SIZE, 48);
    }

    #[test]
    fn test_trade_print_roundtrip() {
        let msg = sample();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_trade_print_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_trade_print_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(), &mut buf);
        buf[PAD0_OFFSET_BASE + 4] = 0xFF;
        assert_eq!(
            parse(&buf),
            Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + 4))
        );
    }
}
