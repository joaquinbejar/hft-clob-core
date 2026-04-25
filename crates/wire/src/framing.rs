//! Length-prefixed framing.
//!
//! Layout: `[len: u32 LE][kind: u8][payload: u8; len - 1]`.
//! `len` counts bytes from the `kind` byte through the end of the
//! payload, so total frame size = `4 + len`. Decoders that encounter an
//! unknown `kind` advance `len` bytes and continue.
//!
//! See `docs/protocol.md` for the per-message layouts.

use crate::WireError;

/// Number of bytes used to encode the frame length prefix.
pub const FRAME_LEN_BYTES: usize = 4;
/// Number of bytes used to encode the message-kind discriminant.
pub const FRAME_KIND_BYTES: usize = 1;
/// Total header size in bytes: `len` prefix + `kind` byte.
pub const FRAME_HEADER_BYTES: usize = FRAME_LEN_BYTES + FRAME_KIND_BYTES;

/// Wire-level message kind. Numeric discriminants are wire-stable and
/// must match the table in `docs/protocol.md`. Inbound kinds occupy
/// `0x01..=0x06`; outbound kinds occupy `0x65..=0x68` (decimal 101..104).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MessageKind {
    /// `NewOrder` — limit or market order entry.
    NewOrder = 0x01,
    /// `CancelOrder` — explicit cancel of a resting order.
    CancelOrder = 0x02,
    /// `CancelReplace` — modify a resting order in place.
    CancelReplace = 0x03,
    /// `MassCancel` — cancel every resting order on an account.
    MassCancel = 0x04,
    /// `KillSwitchSet` — admin halt / resume of the engine.
    KillSwitchSet = 0x05,
    /// `SnapshotRequest` — operator dump of book + engine state.
    SnapshotRequest = 0x06,
    /// `ExecReport` — order lifecycle transition (accepted, rejected,
    /// (partially) filled, cancelled, replaced).
    ExecReport = 0x65,
    /// `TradePrint` — public trade emission.
    TradePrint = 0x66,
    /// `BookUpdateTop` — top-of-book on every book change.
    BookUpdateTop = 0x67,
    /// `BookUpdateL2Delta` — per-level depth delta.
    BookUpdateL2Delta = 0x68,
}

impl MessageKind {
    /// Numeric discriminant for the wire encoder.
    #[inline(always)]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for MessageKind {
    type Error = WireError;
    #[inline]
    fn try_from(v: u8) -> Result<Self, WireError> {
        match v {
            0x01 => Ok(Self::NewOrder),
            0x02 => Ok(Self::CancelOrder),
            0x03 => Ok(Self::CancelReplace),
            0x04 => Ok(Self::MassCancel),
            0x05 => Ok(Self::KillSwitchSet),
            0x06 => Ok(Self::SnapshotRequest),
            0x65 => Ok(Self::ExecReport),
            0x66 => Ok(Self::TradePrint),
            0x67 => Ok(Self::BookUpdateTop),
            0x68 => Ok(Self::BookUpdateL2Delta),
            other => Err(WireError::UnknownKind(other)),
        }
    }
}

/// One parsed frame: a message kind plus a borrowed payload slice.
///
/// `Frame` does NOT own the buffer; callers feed `&[u8]` from the
/// inbound socket. Lifetimes follow the input.
#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Discriminant from the frame header.
    pub kind: MessageKind,
    /// Raw payload bytes (everything after the `kind` byte).
    pub payload: &'a [u8],
}

/// Outcome of [`Frame::parse_or_skip`]. Lets callers advance past a
/// frame whose `kind` byte they do not recognise without losing
/// stream synchronization.
#[derive(Debug, Clone, Copy)]
pub enum ParseOutcome<'a> {
    /// Header carried a known [`MessageKind`]. Consume `total` bytes
    /// and act on `frame`.
    Frame(Frame<'a>),
    /// Header carried an unrecognised kind byte. Consume `total` bytes
    /// and continue; the caller's transport is now resynced.
    UnknownKind(u8),
}

impl<'a> Frame<'a> {
    /// Parse one frame from the start of `buf`, strictly. Returns the
    /// frame and the number of bytes consumed (`4 + len`).
    ///
    /// # Errors
    /// - [`WireError::Truncated`] if `buf` is shorter than the declared frame.
    /// - [`WireError::UnknownKind`] if the `kind` byte is not assigned.
    ///
    /// Use [`Frame::parse_or_skip`] when the caller wants to resync
    /// past unknown kinds rather than abort the stream.
    #[inline]
    pub fn parse(buf: &'a [u8]) -> Result<(Self, usize), WireError> {
        match Self::parse_or_skip(buf)? {
            (ParseOutcome::Frame(frame), total) => Ok((frame, total)),
            (ParseOutcome::UnknownKind(byte), _) => Err(WireError::UnknownKind(byte)),
        }
    }

    /// Parse one frame, treating an unknown `kind` byte as a recoverable
    /// outcome rather than an error. Returns the parsed envelope and the
    /// number of bytes consumed (`4 + len`).
    ///
    /// This is the canonical resync entry point: a caller advancing
    /// `total` bytes after every call always lands at the next frame
    /// boundary, regardless of whether the kind was recognised.
    ///
    /// # Errors
    /// [`WireError::Truncated`] if `buf` is shorter than the declared
    /// frame. Truncation is unrecoverable here because the length prefix
    /// cannot be trusted.
    pub fn parse_or_skip(buf: &'a [u8]) -> Result<(ParseOutcome<'a>, usize), WireError> {
        let len_arr: [u8; FRAME_LEN_BYTES] = buf
            .get(..FRAME_LEN_BYTES)
            .ok_or(WireError::Truncated)?
            .try_into()
            .map_err(|_| WireError::Truncated)?;
        let len = u32::from_le_bytes(len_arr) as usize;
        if len < FRAME_KIND_BYTES {
            return Err(WireError::Truncated);
        }
        let total = FRAME_LEN_BYTES + len;
        let frame_bytes = buf.get(..total).ok_or(WireError::Truncated)?;
        let kind_byte = *frame_bytes
            .get(FRAME_LEN_BYTES)
            .ok_or(WireError::Truncated)?;
        let outcome = match MessageKind::try_from(kind_byte) {
            Ok(kind) => {
                let payload = frame_bytes
                    .get(FRAME_HEADER_BYTES..)
                    .ok_or(WireError::Truncated)?;
                ParseOutcome::Frame(Frame { kind, payload })
            }
            Err(_) => ParseOutcome::UnknownKind(kind_byte),
        };
        Ok((outcome, total))
    }

    /// Append a framed message to `out`. Layout matches [`Frame::parse`].
    ///
    /// # Errors
    /// [`WireError::PayloadTooLarge`] if `1 + payload.len()` exceeds
    /// [`u32::MAX`]. Inbound payloads in this crate are fixed-size and
    /// far below that ceiling, but the encoder refuses to silently
    /// truncate the length prefix.
    pub fn write(kind: MessageKind, payload: &[u8], out: &mut Vec<u8>) -> Result<(), WireError> {
        let framed = FRAME_KIND_BYTES
            .checked_add(payload.len())
            .ok_or(WireError::PayloadTooLarge(payload.len()))?;
        let len = u32::try_from(framed).map_err(|_| WireError::PayloadTooLarge(framed))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.push(kind.as_u8());
        out.extend_from_slice(payload);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_write_then_parse_roundtrip() {
        let payload = [1u8, 2, 3, 4, 5];
        let mut buf = Vec::new();
        Frame::write(MessageKind::NewOrder, &payload, &mut buf).expect("fits");
        let (frame, consumed) = Frame::parse(&buf).expect("parse");
        assert_eq!(frame.kind, MessageKind::NewOrder);
        assert_eq!(frame.payload, &payload);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn test_frame_parse_truncated_len_returns_err() {
        let buf = [0x01u8, 0x00, 0x00]; // only 3 bytes — needs 4 for len
        assert!(matches!(Frame::parse(&buf), Err(WireError::Truncated)));
    }

    #[test]
    fn test_frame_parse_truncated_payload_returns_err() {
        // declares len = 10 but provides only kind + 2 payload bytes
        let mut buf = vec![10u8, 0, 0, 0]; // len = 10
        buf.push(0x01); // kind
        buf.extend_from_slice(&[0u8; 2]);
        assert!(matches!(Frame::parse(&buf), Err(WireError::Truncated)));
    }

    #[test]
    fn test_frame_parse_unknown_kind_returns_err() {
        let mut buf = vec![1u8, 0, 0, 0]; // len = 1 (kind only)
        buf.push(0xFF);
        assert!(matches!(
            Frame::parse(&buf),
            Err(WireError::UnknownKind(0xFF))
        ));
    }

    #[test]
    fn test_frame_parse_or_skip_unknown_kind_returns_outcome() {
        // Unknown kind 0xAB followed by 2 trailing payload bytes —
        // declared len = 3 (kind + 2). parse_or_skip must report the
        // kind byte and the full consumed size so the caller can
        // resync.
        let mut buf = vec![3u8, 0, 0, 0]; // len = 3
        buf.push(0xAB);
        buf.extend_from_slice(&[0u8; 2]);
        let (outcome, consumed) = Frame::parse_or_skip(&buf).expect("framing valid");
        assert_eq!(consumed, buf.len());
        match outcome {
            ParseOutcome::UnknownKind(byte) => assert_eq!(byte, 0xAB),
            ParseOutcome::Frame(_) => panic!("expected UnknownKind"),
        }
    }

    #[test]
    fn test_frame_parse_or_skip_known_kind_returns_frame() {
        let payload = [9u8, 8, 7];
        let mut buf = Vec::new();
        Frame::write(MessageKind::CancelOrder, &payload, &mut buf).expect("fits");
        let (outcome, consumed) = Frame::parse_or_skip(&buf).expect("framing valid");
        assert_eq!(consumed, buf.len());
        match outcome {
            ParseOutcome::Frame(frame) => {
                assert_eq!(frame.kind, MessageKind::CancelOrder);
                assert_eq!(frame.payload, &payload);
            }
            ParseOutcome::UnknownKind(_) => panic!("expected Frame"),
        }
    }

    #[test]
    fn test_message_kind_try_from_unknown_returns_err() {
        assert_eq!(MessageKind::try_from(0), Err(WireError::UnknownKind(0)));
        assert_eq!(
            MessageKind::try_from(0x7F),
            Err(WireError::UnknownKind(0x7F))
        );
    }
}
