//! `BookUpdateL2Delta` (kind = 0x68 / 104).
//!
//! Per-level depth delta. `new_qty == 0` is the wire sentinel for
//! "level removed"; non-zero is the new aggregate qty at that level.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{EngineSeq, Price, Qty, RecvTs, Side};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 40 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct BookUpdateL2DeltaWire {
    /// Strictly monotonic engine sequence number.
    pub engine_seq: u64,
    /// Level price in ticks.
    pub price: i64,
    /// New aggregate qty at this level in lots; `0` means the level
    /// has been removed.
    pub new_qty: u64,
    /// `Side` discriminant.
    pub side: u8,
    /// Reserved padding; every byte must be zero.
    pub _pad0: [u8; 7],
    /// Engine emit timestamp.
    pub emit_ts: u64,
}

const _: () = assert!(core::mem::size_of::<BookUpdateL2DeltaWire>() == 40);

const SIZE: usize = core::mem::size_of::<BookUpdateL2DeltaWire>();
const PAD0_OFFSET_BASE: usize = 25;

/// Domain-typed `BookUpdateL2Delta`. `new_qty == None` corresponds to
/// "level removed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookUpdateL2Delta {
    /// Engine-assigned sequence number.
    pub engine_seq: EngineSeq,
    /// Level price.
    pub price: Price,
    /// New aggregate qty at this level; `None` means the level has
    /// been removed.
    pub new_qty: Option<Qty>,
    /// Side that the level lives on.
    pub side: Side,
    /// Engine emit timestamp.
    pub emit_ts: RecvTs,
}

/// Decode `BookUpdateL2Delta` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or domain validation.
pub fn parse(payload: &[u8]) -> Result<BookUpdateL2Delta, WireError> {
    let w = BookUpdateL2DeltaWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    for (i, byte) in pad.iter().enumerate() {
        if *byte != 0 {
            return Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + i));
        }
    }
    let new_qty = if w.new_qty != 0 {
        Some(Qty::new(w.new_qty).map_err(to_wire)?)
    } else {
        None
    };
    Ok(BookUpdateL2Delta {
        engine_seq: EngineSeq::new(w.engine_seq),
        price: Price::new(w.price).map_err(to_wire)?,
        new_qty,
        side: Side::try_from(w.side).map_err(to_wire)?,
        emit_ts: RecvTs::new(w.emit_ts as i64),
    })
}

/// Encode `BookUpdateL2Delta` into a payload buffer.
pub fn encode(msg: &BookUpdateL2Delta, out: &mut Vec<u8>) {
    let w = BookUpdateL2DeltaWire {
        engine_seq: msg.engine_seq.as_raw(),
        price: msg.price.as_ticks(),
        new_qty: msg.new_qty.map(Qty::as_lots).unwrap_or(0),
        side: msg.side.as_u8(),
        _pad0: [0; 7],
        emit_ts: msg.emit_ts.as_nanos() as u64,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_present() -> BookUpdateL2Delta {
        BookUpdateL2Delta {
            engine_seq: EngineSeq::new(200),
            price: Price::new(50).expect("ok"),
            new_qty: Some(Qty::new(13).expect("ok")),
            side: Side::Bid,
            emit_ts: RecvTs::new(1_500_000),
        }
    }

    fn sample_removed() -> BookUpdateL2Delta {
        BookUpdateL2Delta {
            engine_seq: EngineSeq::new(201),
            price: Price::new(60).expect("ok"),
            new_qty: None,
            side: Side::Ask,
            emit_ts: RecvTs::new(1_500_001),
        }
    }

    #[test]
    fn test_book_update_l2_delta_wire_size_is_40() {
        assert_eq!(SIZE, 40);
    }

    #[test]
    fn test_book_update_l2_delta_present_roundtrip() {
        let msg = sample_present();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_book_update_l2_delta_removed_decodes_with_none_qty() {
        let msg = sample_removed();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        let decoded = parse(&buf).expect("decode");
        assert!(decoded.new_qty.is_none());
    }

    #[test]
    fn test_book_update_l2_delta_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_book_update_l2_delta_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_present(), &mut buf);
        buf[PAD0_OFFSET_BASE + 2] = 0xAA;
        assert_eq!(
            parse(&buf),
            Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + 2))
        );
    }
}
