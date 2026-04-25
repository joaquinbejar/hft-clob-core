//! `CancelReplace` (kind = 0x03).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{AccountId, ClientTs, OrderId, Price, Qty};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 40 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct CancelReplaceWire {
    /// Client-stamped timestamp.
    pub client_ts: u64,
    /// Resting order to be replaced.
    pub order_id: u64,
    /// Account that owns `order_id`.
    pub account_id: u32,
    /// Reserved padding; decoder rejects non-zero.
    pub _pad0: u32,
    /// New limit price in ticks.
    pub new_price: i64,
    /// New quantity in lots; `> 0`.
    pub new_qty: u64,
}

const _: () = assert!(core::mem::size_of::<CancelReplaceWire>() == 40);

const SIZE: usize = core::mem::size_of::<CancelReplaceWire>();
const PAD0_OFFSET: usize = 20;

/// Domain-typed `CancelReplace`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelReplace {
    /// Client-stamped timestamp.
    pub client_ts: ClientTs,
    /// Resting order to replace.
    pub order_id: OrderId,
    /// Account binding.
    pub account_id: AccountId,
    /// New price.
    pub new_price: Price,
    /// New quantity.
    pub new_qty: Qty,
}

/// Decode `CancelReplace` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or domain validation.
pub fn parse(payload: &[u8]) -> Result<CancelReplace, WireError> {
    let w = CancelReplaceWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    if pad != 0 {
        return Err(WireError::NonZeroPad(PAD0_OFFSET));
    }
    Ok(CancelReplace {
        client_ts: ClientTs::new({ w.client_ts } as i64),
        order_id: OrderId::new(w.order_id).map_err(to_wire)?,
        account_id: AccountId::new(w.account_id).map_err(to_wire)?,
        new_price: Price::new(w.new_price).map_err(to_wire)?,
        new_qty: Qty::new(w.new_qty).map_err(to_wire)?,
    })
}

/// Encode `CancelReplace` into a payload buffer.
pub fn encode(msg: &CancelReplace, out: &mut Vec<u8>) {
    let w = CancelReplaceWire {
        client_ts: msg.client_ts.as_nanos() as u64,
        order_id: msg.order_id.as_raw(),
        account_id: msg.account_id.as_raw(),
        _pad0: 0,
        new_price: msg.new_price.as_ticks(),
        new_qty: msg.new_qty.as_lots(),
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CancelReplace {
        CancelReplace {
            client_ts: ClientTs::new(100),
            order_id: OrderId::new(42).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            new_price: Price::new(200).expect("ok"),
            new_qty: Qty::new(11).expect("ok"),
        }
    }

    #[test]
    fn test_cancel_replace_wire_size_is_40() {
        assert_eq!(SIZE, 40);
    }

    #[test]
    fn test_cancel_replace_roundtrip() {
        let msg = sample();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_cancel_replace_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_cancel_replace_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(), &mut buf);
        buf[PAD0_OFFSET] = 0x01;
        assert_eq!(parse(&buf), Err(WireError::NonZeroPad(PAD0_OFFSET)));
    }
}
