//! Outbound message → wire-byte encoder.
//!
//! Translates [`wire::outbound::Outbound`] events emitted by the
//! engine into framed bytes ready for network transmission. The
//! encoder is sync — it runs on the engine thread (or a dedicated
//! draining thread) without tokio. Per-session fan-out lives in
//! `crates/gateway/src/sink.rs`.
//!
//! Each encoded frame carries `engine_seq` from the typed payload.
//! Consumers verify monotonicity by reading the prefix of each
//! decoded frame; gap detection is built into the contract.

use wire::framing::{Frame, MessageKind};
use wire::outbound::{Outbound, book_update_l2_delta, book_update_top, exec_report, trade_print};
use wire::{WireError, framing};

/// Encode one [`Outbound`] event into a framed byte sequence appended
/// to `out`. Allocates `framing::FRAME_HEADER_BYTES + payload_size`
/// at the back of the supplied buffer.
///
/// # Errors
/// Propagates [`WireError`] from the framing writer when the payload
/// exceeds [`u32::MAX`] bytes (impossible for any of the fixed-size
/// outbound messages in v1; reported anyway).
pub fn encode(msg: &Outbound, out: &mut Vec<u8>) -> Result<(), WireError> {
    let mut payload = Vec::with_capacity(64);
    let kind = match msg {
        Outbound::ExecReport(r) => {
            exec_report::encode(r, &mut payload);
            MessageKind::ExecReport
        }
        Outbound::TradePrint(t) => {
            trade_print::encode(t, &mut payload);
            MessageKind::TradePrint
        }
        Outbound::BookUpdateTop(b) => {
            book_update_top::encode(b, &mut payload);
            MessageKind::BookUpdateTop
        }
        Outbound::BookUpdateL2Delta(d) => {
            book_update_l2_delta::encode(d, &mut payload);
            MessageKind::BookUpdateL2Delta
        }
    };
    Frame::write(kind, &payload, out)?;
    Ok(())
}

/// Decode one outbound frame at the front of `buf`. Mirrors
/// [`encode`] for tests and replay diff. Returns the decoded
/// [`Outbound`] and the number of bytes consumed.
///
/// # Errors
/// Propagates [`WireError`] from the framing reader / per-message
/// decoder.
pub fn decode(buf: &[u8]) -> Result<(Outbound, usize), WireError> {
    let (frame, total) = Frame::parse(buf)?;
    let msg = wire::outbound::parse_frame(frame)?;
    Ok((msg, total))
}

/// Number of bytes used for the frame header (length prefix + kind).
/// Re-exported so consumers can size their gap-detection buffers.
pub const FRAME_HEADER_BYTES: usize = framing::FRAME_HEADER_BYTES;

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{AccountId, EngineSeq, ExecState, OrderId, Price, Qty, RecvTs, Side, TradeId};
    use wire::outbound::{BookUpdateTop, ExecReport, TradePrint};

    #[test]
    fn test_encode_then_decode_roundtrip_exec_report() {
        let msg = Outbound::ExecReport(ExecReport {
            engine_seq: EngineSeq::new(1),
            order_id: OrderId::new(42).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            state: ExecState::Accepted,
            fill_price: None,
            fill_qty: None,
            leaves_qty: Some(Qty::new(5).expect("ok")),
            reject_reason: None,
            cancel_reason: None,
            recv_ts: RecvTs::new(100),
            emit_ts: RecvTs::new(101),
        });
        let mut buf = Vec::new();
        encode(&msg, &mut buf).expect("encode");
        let (decoded, total) = decode(&buf).expect("decode");
        assert_eq!(total, buf.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_trade_print() {
        let msg = Outbound::TradePrint(TradePrint {
            engine_seq: EngineSeq::new(2),
            trade_id: TradeId::new(1),
            price: Price::new(100).expect("ok"),
            qty: Qty::new(5).expect("ok"),
            aggressor_side: Side::Bid,
            emit_ts: RecvTs::new(200),
        });
        let mut buf = Vec::new();
        encode(&msg, &mut buf).expect("encode");
        let (decoded, total) = decode(&buf).expect("decode");
        assert_eq!(total, buf.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_then_decode_roundtrip_book_update_top() {
        let msg = Outbound::BookUpdateTop(BookUpdateTop {
            engine_seq: EngineSeq::new(3),
            bid: Some((Price::new(99).expect("ok"), Qty::new(10).expect("ok"))),
            ask: Some((Price::new(101).expect("ok"), Qty::new(7).expect("ok"))),
            emit_ts: RecvTs::new(300),
        });
        let mut buf = Vec::new();
        encode(&msg, &mut buf).expect("encode");
        let (decoded, total) = decode(&buf).expect("decode");
        assert_eq!(total, buf.len());
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_appends_to_existing_buffer() {
        let mut buf = vec![0xCA, 0xFE];
        let msg = Outbound::TradePrint(TradePrint {
            engine_seq: EngineSeq::new(1),
            trade_id: TradeId::new(1),
            price: Price::new(100).expect("ok"),
            qty: Qty::new(1).expect("ok"),
            aggressor_side: Side::Bid,
            emit_ts: RecvTs::new(0),
        });
        encode(&msg, &mut buf).expect("encode");
        assert_eq!(&buf[..2], &[0xCA, 0xFE]);
        let (decoded, _) = decode(&buf[2..]).expect("decode");
        assert_eq!(decoded, msg);
    }
}
