//! `KillSwitchSet` (kind = 0x05).

use std::fmt;

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::ClientTs;

use crate::WireError;

/// Wire layout — `#[repr(C, packed)]`, 24 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct KillSwitchSetWire {
    /// Client-stamped timestamp.
    pub client_ts: u64,
    /// Shared-secret token; the engine compares against its admin token.
    pub admin_token: u64,
    /// `0` = resume, `1` = halt. Other values are decoded as
    /// [`WireError::Domain`] downstream.
    pub state: u8,
    /// Reserved padding; every byte must be zero.
    pub _pad0: [u8; 7],
}

const _: () = assert!(core::mem::size_of::<KillSwitchSetWire>() == 24);

const SIZE: usize = core::mem::size_of::<KillSwitchSetWire>();
const PAD0_OFFSET_BASE: usize = 17;

/// Halt / resume command. Wire-stable: numeric discriminants are part
/// of the public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum KillSwitchState {
    /// Lift the kill switch.
    Resume = 0,
    /// Engage the kill switch.
    Halt = 1,
}

impl KillSwitchState {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for KillSwitchState {
    type Error = WireError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, WireError> {
        match v {
            0 => Ok(Self::Resume),
            1 => Ok(Self::Halt),
            other => Err(WireError::UnknownKind(other)),
        }
    }
}

impl fmt::Display for KillSwitchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resume => f.write_str("Resume"),
            Self::Halt => f.write_str("Halt"),
        }
    }
}

/// Domain-typed `KillSwitchSet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillSwitchSet {
    /// Client-stamped timestamp.
    pub client_ts: ClientTs,
    /// Shared-secret token presented by the operator.
    pub admin_token: u64,
    /// Halt / resume.
    pub state: KillSwitchState,
}

/// Decode `KillSwitchSet` from a payload.
///
/// # Errors
/// [`WireError`] on size mismatch, non-zero pad, or unknown `state`.
pub fn parse(payload: &[u8]) -> Result<KillSwitchSet, WireError> {
    let w = KillSwitchSetWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    for (i, byte) in pad.iter().enumerate() {
        if *byte != 0 {
            return Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + i));
        }
    }
    let state = KillSwitchState::try_from(w.state)?;
    Ok(KillSwitchSet {
        client_ts: ClientTs::new({ w.client_ts } as i64),
        admin_token: { w.admin_token },
        state,
    })
}

/// Encode `KillSwitchSet` into a payload buffer.
pub fn encode(msg: &KillSwitchSet, out: &mut Vec<u8>) {
    let w = KillSwitchSetWire {
        client_ts: msg.client_ts.as_nanos() as u64,
        admin_token: msg.admin_token,
        state: msg.state.as_u8(),
        _pad0: [0; 7],
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(state: KillSwitchState) -> KillSwitchSet {
        KillSwitchSet {
            client_ts: ClientTs::new(0),
            admin_token: 0xDEADBEEF,
            state,
        }
    }

    #[test]
    fn test_kill_switch_wire_size_is_24() {
        assert_eq!(SIZE, 24);
    }

    #[test]
    fn test_kill_switch_halt_roundtrip() {
        let msg = sample(KillSwitchState::Halt);
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_kill_switch_resume_roundtrip() {
        let msg = sample(KillSwitchState::Resume);
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_kill_switch_unknown_state_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(KillSwitchState::Halt), &mut buf);
        buf[16] = 0x05; // state field
        assert_eq!(parse(&buf), Err(WireError::UnknownKind(5)));
    }

    #[test]
    fn test_kill_switch_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample(KillSwitchState::Halt), &mut buf);
        buf[PAD0_OFFSET_BASE + 3] = 0x42;
        assert_eq!(
            parse(&buf),
            Err(WireError::NonZeroPad(PAD0_OFFSET_BASE + 3))
        );
    }
}
