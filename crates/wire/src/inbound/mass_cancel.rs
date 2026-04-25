//! `MassCancel` (kind = 0x04).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{AccountId, ClientTs};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 16 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct MassCancelWire {
    /// Client-stamped timestamp.
    pub client_ts: u64,
    /// Account whose resting orders are cancelled.
    pub account_id: u32,
    /// Reserved padding; decoder rejects non-zero.
    pub _pad0: u32,
}

const _: () = assert!(core::mem::size_of::<MassCancelWire>() == 16);

const SIZE: usize = core::mem::size_of::<MassCancelWire>();
const PAD0_OFFSET: usize = 12;

/// Domain-typed `MassCancel`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MassCancel {
    /// Client-stamped timestamp.
    pub client_ts: ClientTs,
    /// Account binding.
    pub account_id: AccountId,
}

/// Decode `MassCancel` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or domain validation.
pub fn parse(payload: &[u8]) -> Result<MassCancel, WireError> {
    let w = MassCancelWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    if pad != 0 {
        return Err(WireError::NonZeroPad(PAD0_OFFSET));
    }
    Ok(MassCancel {
        client_ts: ClientTs::new({ w.client_ts } as i64),
        account_id: AccountId::new(w.account_id).map_err(to_wire)?,
    })
}

/// Encode `MassCancel` into a payload buffer.
pub fn encode(msg: &MassCancel, out: &mut Vec<u8>) {
    let w = MassCancelWire {
        client_ts: msg.client_ts.as_nanos() as u64,
        account_id: msg.account_id.as_raw(),
        _pad0: 0,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MassCancel {
        MassCancel {
            client_ts: ClientTs::new(0),
            account_id: AccountId::new(9).expect("ok"),
        }
    }

    #[test]
    fn test_mass_cancel_wire_size_is_16() {
        assert_eq!(SIZE, 16);
    }

    #[test]
    fn test_mass_cancel_roundtrip() {
        let msg = sample();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_mass_cancel_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_mass_cancel_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(), &mut buf);
        buf[PAD0_OFFSET] = 0x77;
        assert_eq!(parse(&buf), Err(WireError::NonZeroPad(PAD0_OFFSET)));
    }
}
