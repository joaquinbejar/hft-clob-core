//! `CancelOrder` (kind = 0x02).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{AccountId, ClientTs, OrderId};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 24 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct CancelOrderWire {
    /// Client-stamped timestamp.
    pub client_ts: u64,
    /// Order to cancel.
    pub order_id: u64,
    /// Account binding.
    pub account_id: u32,
    /// Reserved padding; decoder rejects non-zero.
    pub _pad0: u32,
}

const _: () = assert!(core::mem::size_of::<CancelOrderWire>() == 24);

const SIZE: usize = core::mem::size_of::<CancelOrderWire>();
const PAD0_OFFSET: usize = 20;

/// Domain-typed `CancelOrder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelOrder {
    /// Client-stamped timestamp.
    pub client_ts: ClientTs,
    /// Order to cancel.
    pub order_id: OrderId,
    /// Account binding.
    pub account_id: AccountId,
}

/// Decode `CancelOrder` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or domain validation.
pub fn parse(payload: &[u8]) -> Result<CancelOrder, WireError> {
    let w = CancelOrderWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    if pad != 0 {
        return Err(WireError::NonZeroPad(PAD0_OFFSET));
    }
    Ok(CancelOrder {
        client_ts: ClientTs::new({ w.client_ts } as i64),
        order_id: OrderId::new(w.order_id).map_err(to_wire)?,
        account_id: AccountId::new(w.account_id).map_err(to_wire)?,
    })
}

/// Encode `CancelOrder` into a payload buffer.
pub fn encode(msg: &CancelOrder, out: &mut Vec<u8>) {
    let w = CancelOrderWire {
        client_ts: msg.client_ts.as_nanos() as u64,
        order_id: msg.order_id.as_raw(),
        account_id: msg.account_id.as_raw(),
        _pad0: 0,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CancelOrder {
        CancelOrder {
            client_ts: ClientTs::new(100),
            order_id: OrderId::new(42).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
        }
    }

    #[test]
    fn test_cancel_order_wire_size_is_24() {
        assert_eq!(SIZE, 24);
    }

    #[test]
    fn test_cancel_order_roundtrip() {
        let msg = sample();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_cancel_order_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_cancel_order_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(), &mut buf);
        buf[PAD0_OFFSET] = 0xAB;
        assert_eq!(parse(&buf), Err(WireError::NonZeroPad(PAD0_OFFSET)));
    }
}
