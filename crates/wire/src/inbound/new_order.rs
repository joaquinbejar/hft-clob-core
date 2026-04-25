//! `NewOrder` (kind = 0x01).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 40 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct NewOrderWire {
    /// Client-stamped timestamp; ns since the documented epoch.
    pub client_ts: u64,
    /// Client-assigned non-zero `order_id`.
    pub order_id: u64,
    /// Account binding, non-zero.
    pub account_id: u32,
    /// `Side` discriminant.
    pub side: u8,
    /// `OrderType` discriminant.
    pub order_type: u8,
    /// `Tif` discriminant.
    pub tif: u8,
    /// Reserved padding; decoder rejects non-zero.
    pub _pad0: u8,
    /// Limit price in ticks; ignored when `order_type = Market`.
    pub price: i64,
    /// Quantity in lots; `> 0`.
    pub qty: u64,
}

const _: () = assert!(core::mem::size_of::<NewOrderWire>() == 40);

const NEW_ORDER_SIZE: usize = core::mem::size_of::<NewOrderWire>();
const PAD0_OFFSET: usize = 23;

/// Domain-typed `NewOrder` after decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOrder {
    /// Client-stamped timestamp.
    pub client_ts: ClientTs,
    /// Client-assigned identifier.
    pub order_id: OrderId,
    /// Account binding.
    pub account_id: AccountId,
    /// Buy / sell.
    pub side: Side,
    /// Limit / Market.
    pub order_type: OrderType,
    /// Time-in-force policy.
    pub tif: Tif,
    /// Limit price; `None` for market orders.
    pub price: Option<Price>,
    /// Order quantity.
    pub qty: Qty,
}

/// Decode `NewOrder` from a raw payload (no frame header).
///
/// # Errors
/// Returns [`WireError`] on size mismatch, non-zero pad, or domain
/// validation failure.
pub fn parse(payload: &[u8]) -> Result<NewOrder, WireError> {
    let w = NewOrderWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: NEW_ORDER_SIZE,
        got: payload.len(),
    })?;

    // Reads from packed fields must copy into a local first.
    let pad = { w._pad0 };
    if pad != 0 {
        return Err(WireError::NonZeroPad(PAD0_OFFSET));
    }

    let client_ts = ClientTs::new({ w.client_ts } as i64);
    let order_id = OrderId::new(w.order_id).map_err(to_wire)?;
    let account_id = AccountId::new(w.account_id).map_err(to_wire)?;
    let side = Side::try_from(w.side).map_err(to_wire)?;
    let order_type = OrderType::try_from(w.order_type).map_err(to_wire)?;
    let tif = Tif::try_from(w.tif).map_err(to_wire)?;
    let qty = Qty::new(w.qty).map_err(to_wire)?;

    let price = match order_type {
        OrderType::Market => None,
        OrderType::Limit => Some(Price::new(w.price).map_err(to_wire)?),
    };

    Ok(NewOrder {
        client_ts,
        order_id,
        account_id,
        side,
        order_type,
        tif,
        price,
        qty,
    })
}

/// Encode `NewOrder` into a payload buffer.
pub fn encode(msg: &NewOrder, out: &mut Vec<u8>) {
    let w = NewOrderWire {
        client_ts: msg.client_ts.as_nanos() as u64,
        order_id: msg.order_id.as_raw(),
        account_id: msg.account_id.as_raw(),
        side: msg.side.as_u8(),
        order_type: msg.order_type.as_u8(),
        tif: msg.tif.as_u8(),
        _pad0: 0,
        price: msg.price.map(Price::as_ticks).unwrap_or(0),
        qty: msg.qty.as_lots(),
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_limit() -> NewOrder {
        NewOrder {
            client_ts: ClientTs::new(1_700_000_000_000_000_000),
            order_id: OrderId::new(42).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            side: Side::Bid,
            order_type: OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(Price::new(100).expect("ok")),
            qty: Qty::new(5).expect("ok"),
        }
    }

    fn sample_market() -> NewOrder {
        NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(1).expect("ok"),
            side: Side::Ask,
            order_type: OrderType::Market,
            tif: Tif::Ioc,
            price: None,
            qty: Qty::new(10).expect("ok"),
        }
    }

    #[test]
    fn test_new_order_wire_size_is_40() {
        assert_eq!(core::mem::size_of::<NewOrderWire>(), 40);
    }

    #[test]
    fn test_new_order_limit_roundtrip() {
        let msg = sample_limit();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        let decoded = parse(&buf).expect("decode");
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_new_order_market_decodes_with_none_price() {
        let msg = sample_market();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        let decoded = parse(&buf).expect("decode");
        assert!(decoded.price.is_none());
        assert_eq!(decoded.order_type, OrderType::Market);
    }

    #[test]
    fn test_new_order_truncated_returns_payload_size_error() {
        let buf = [0u8; NEW_ORDER_SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_new_order_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_limit(), &mut buf);
        buf[PAD0_OFFSET] = 0xFF;
        assert_eq!(parse(&buf), Err(WireError::NonZeroPad(PAD0_OFFSET)));
    }

    #[test]
    fn test_new_order_zero_order_id_returns_domain_err() {
        let mut buf = Vec::new();
        encode(&sample_limit(), &mut buf);
        // overwrite order_id (offset 8) with zero
        for byte in &mut buf[8..16] {
            *byte = 0;
        }
        assert!(matches!(parse(&buf), Err(WireError::Domain(_))));
    }
}
