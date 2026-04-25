//! `SnapshotRequest` (kind = 0x06).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::WireError;

/// Wire layout — `#[repr(C, packed)]`, 16 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct SnapshotRequestWire {
    /// Operator-assigned correlation id; echoed in `SnapshotResponse`.
    pub request_id: u64,
    /// Reserved padding; every byte must be zero.
    pub _pad0: [u8; 8],
}

const _: () = assert!(core::mem::size_of::<SnapshotRequestWire>() == 16);

const SIZE: usize = core::mem::size_of::<SnapshotRequestWire>();
const PAD0_OFFSET_BASE: usize = 8;

/// Domain-typed `SnapshotRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRequest {
    /// Operator-assigned correlation id.
    pub request_id: u64,
}

/// Decode `SnapshotRequest` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch or non-zero padding.
pub fn parse(payload: &[u8]) -> Result<SnapshotRequest, WireError> {
    let w = SnapshotRequestWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    for (i, byte) in pad.iter().enumerate() {
        if *byte != 0 {
            return Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + i));
        }
    }
    Ok(SnapshotRequest {
        request_id: { w.request_id },
    })
}

/// Encode `SnapshotRequest` into a payload buffer.
pub fn encode(msg: &SnapshotRequest, out: &mut Vec<u8>) {
    let w = SnapshotRequestWire {
        request_id: msg.request_id,
        _pad0: [0; 8],
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_request_wire_size_is_16() {
        assert_eq!(SIZE, 16);
    }

    #[test]
    fn test_snapshot_request_roundtrip() {
        let msg = SnapshotRequest {
            request_id: 0xCAFEBABE,
        };
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_snapshot_request_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_snapshot_request_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&SnapshotRequest { request_id: 1 }, &mut buf);
        buf[PAD0_OFFSET_BASE + 5] = 0xAA;
        assert_eq!(
            parse(&buf),
            Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + 5))
        );
    }
}
