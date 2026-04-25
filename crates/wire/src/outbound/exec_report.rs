//! `ExecReport` (kind = 0x65 / 101).

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use domain::{
    AccountId, CancelReason, EngineSeq, ExecState, OrderId, Price, Qty, RecvTs, RejectReason,
};

use crate::error::{WireError, to_wire};

/// Wire layout — `#[repr(C, packed)]`, 64 bytes, little-endian.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Unaligned, KnownLayout, Immutable)]
#[repr(C, packed)]
pub struct ExecReportWire {
    /// Strictly monotonic engine sequence number.
    pub engine_seq: u64,
    /// Order this report applies to.
    pub order_id: u64,
    /// Account binding.
    pub account_id: u32,
    /// `ExecState` discriminant.
    pub state: u8,
    /// `RejectReason` discriminant; `0` when the report is not a reject.
    pub reject_reason: u8,
    /// `CancelReason` discriminant; `0` when the report is not a cancel
    /// (or replace) terminal.
    pub cancel_reason: u8,
    /// Reserved padding; decoder rejects non-zero.
    pub _pad0: u8,
    /// Fill price in ticks; `0` when the report carries no fill.
    pub fill_price: i64,
    /// Fill quantity in lots; `0` when the report carries no fill.
    pub fill_qty: u64,
    /// Resting quantity remaining after this transition.
    pub leaves_qty: u64,
    /// Echo of the inbound `recv_ts`.
    pub recv_ts: u64,
    /// Engine emit timestamp; ns since the documented epoch.
    pub emit_ts: u64,
}

const _: () = assert!(core::mem::size_of::<ExecReportWire>() == 64);

const SIZE: usize = core::mem::size_of::<ExecReportWire>();
const PAD0_OFFSET: usize = 23;

/// Domain-typed `ExecReport`.
///
/// Fields that do not apply to a given `state` are encoded as wire
/// zeroes and decoded as `None` / sentinel values:
///
/// - `reject_reason` is `Some(_)` only when `state == Rejected`.
/// - `cancel_reason` is `Some(_)` only when `state ∈ {Cancelled, Replaced}`.
/// - `fill_price` / `fill_qty` are `Some(_)` only when the report
///   carries a fill (`PartiallyFilled`, `Filled`).
/// - `leaves_qty` may be `0` post-fill (`Filled` / `Cancelled` /
///   `Rejected`); modelled as `Option<Qty>` so the type-level invariant
///   `Qty > 0` is not violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecReport {
    /// Engine-assigned sequence number.
    pub engine_seq: EngineSeq,
    /// Order this report applies to.
    pub order_id: OrderId,
    /// Account binding.
    pub account_id: AccountId,
    /// Lifecycle state.
    pub state: ExecState,
    /// Fill price (only when the transition carries a fill).
    pub fill_price: Option<Price>,
    /// Fill quantity (only when the transition carries a fill).
    pub fill_qty: Option<Qty>,
    /// Quantity that remains resting after this transition.
    pub leaves_qty: Option<Qty>,
    /// Reject reason (only when `state == Rejected`).
    pub reject_reason: Option<RejectReason>,
    /// Cancel reason (only when `state ∈ {Cancelled, Replaced}`).
    pub cancel_reason: Option<CancelReason>,
    /// Echo of the inbound `recv_ts`.
    pub recv_ts: RecvTs,
    /// Engine emit timestamp.
    pub emit_ts: RecvTs,
}

/// Validate that an [`ExecReport`]'s state-dependent fields are
/// internally consistent. Used by both [`parse`] (after raw decode) and
/// to document the required pairings:
///
/// | `state`            | required `Some` fields           | required `None` fields                                |
/// |--------------------|----------------------------------|-------------------------------------------------------|
/// | `Accepted`         | `leaves_qty`                     | `fill_*`, `reject_reason`, `cancel_reason`            |
/// | `Rejected`         | `reject_reason`                  | `fill_*`, `leaves_qty`, `cancel_reason`               |
/// | `PartiallyFilled`  | `fill_price`, `fill_qty`, `leaves_qty` | `reject_reason`, `cancel_reason`                |
/// | `Filled`           | `fill_price`, `fill_qty`         | `leaves_qty`, `reject_reason`, `cancel_reason`        |
/// | `Cancelled`        | `cancel_reason`                  | `fill_*`, `leaves_qty`, `reject_reason`               |
/// | `Replaced`         | `cancel_reason`                  | `fill_*`, `leaves_qty`, `reject_reason`               |
fn validate_coherent(msg: &ExecReport) -> Result<(), WireError> {
    let has_fill = msg.fill_price.is_some() || msg.fill_qty.is_some();
    let has_both_fill_fields = msg.fill_price.is_some() && msg.fill_qty.is_some();

    match msg.state {
        ExecState::Accepted => {
            if has_fill || msg.reject_reason.is_some() || msg.cancel_reason.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Accepted requires no fill / reject / cancel fields",
                ));
            }
            if msg.leaves_qty.is_none() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Accepted requires leaves_qty",
                ));
            }
        }
        ExecState::Rejected => {
            if has_fill || msg.leaves_qty.is_some() || msg.cancel_reason.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Rejected requires reject_reason only",
                ));
            }
            if msg.reject_reason.is_none() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Rejected requires reject_reason",
                ));
            }
        }
        ExecState::PartiallyFilled => {
            if !has_both_fill_fields {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: PartiallyFilled requires both fill_price and fill_qty",
                ));
            }
            if msg.leaves_qty.is_none() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: PartiallyFilled requires leaves_qty",
                ));
            }
            if msg.reject_reason.is_some() || msg.cancel_reason.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: PartiallyFilled cannot carry reject / cancel reason",
                ));
            }
        }
        ExecState::Filled => {
            if !has_both_fill_fields {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Filled requires both fill_price and fill_qty",
                ));
            }
            if msg.leaves_qty.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Filled is terminal — leaves_qty must be None",
                ));
            }
            if msg.reject_reason.is_some() || msg.cancel_reason.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Filled cannot carry reject / cancel reason",
                ));
            }
        }
        ExecState::Cancelled | ExecState::Replaced => {
            if has_fill || msg.leaves_qty.is_some() || msg.reject_reason.is_some() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Cancelled / Replaced require cancel_reason only",
                ));
            }
            if msg.cancel_reason.is_none() {
                return Err(WireError::InconsistentPayload(
                    "ExecReport: Cancelled / Replaced require cancel_reason",
                ));
            }
        }
    }
    Ok(())
}

/// Decode `ExecReport` from a payload.
///
/// # Errors
/// [`WireError::PayloadSize`] / [`WireError::NonZeroPad`] / [`WireError::Domain`]
/// on the usual decode failures, plus [`WireError::InconsistentPayload`]
/// when the decoded message violates the state→fields contract
/// documented in [`validate_coherent`].
pub fn parse(payload: &[u8]) -> Result<ExecReport, WireError> {
    let w = ExecReportWire::ref_from_bytes(payload).map_err(|_| WireError::PayloadSize {
        expected: SIZE,
        got: payload.len(),
    })?;
    let pad = { w._pad0 };
    if pad != 0 {
        return Err(WireError::NonZeroPad(PAD0_OFFSET));
    }
    let state = ExecState::try_from(w.state).map_err(to_wire)?;
    let order_id = OrderId::new(w.order_id).map_err(to_wire)?;
    let account_id = AccountId::new(w.account_id).map_err(to_wire)?;
    let engine_seq = EngineSeq::new(w.engine_seq);

    let reject_reason = if w.reject_reason != 0 {
        Some(RejectReason::try_from(w.reject_reason).map_err(to_wire)?)
    } else {
        None
    };
    let cancel_reason = if w.cancel_reason != 0 {
        Some(CancelReason::try_from(w.cancel_reason).map_err(to_wire)?)
    } else {
        None
    };
    let fill_price = if w.fill_price != 0 {
        Some(Price::new(w.fill_price).map_err(to_wire)?)
    } else {
        None
    };
    let fill_qty = if w.fill_qty != 0 {
        Some(Qty::new(w.fill_qty).map_err(to_wire)?)
    } else {
        None
    };
    let leaves_qty = if w.leaves_qty != 0 {
        Some(Qty::new(w.leaves_qty).map_err(to_wire)?)
    } else {
        None
    };

    let msg = ExecReport {
        engine_seq,
        order_id,
        account_id,
        state,
        fill_price,
        fill_qty,
        leaves_qty,
        reject_reason,
        cancel_reason,
        recv_ts: RecvTs::new(w.recv_ts as i64),
        emit_ts: RecvTs::new(w.emit_ts as i64),
    };
    validate_coherent(&msg)?;
    Ok(msg)
}

/// Encode `ExecReport` into a payload buffer.
///
/// Canonicalises state-irrelevant fields to the wire-zero sentinel
/// based on `msg.state`, so a sloppy caller cannot produce a wire
/// payload that contradicts the spec table in `docs/protocol.md`.
/// Specifically:
///
/// - `reject_reason` is only encoded when `state == Rejected`.
/// - `cancel_reason` is only encoded when `state ∈ {Cancelled, Replaced}`.
/// - `fill_price` / `fill_qty` are only encoded when `state ∈
///   {PartiallyFilled, Filled}`, and only as a pair (if either is
///   `None` the pair is zeroed).
/// - `leaves_qty` is only encoded when `state ∈ {Accepted,
///   PartiallyFilled}`; on terminal states it is forced to zero.
pub fn encode(msg: &ExecReport, out: &mut Vec<u8>) {
    let state = msg.state;

    let reject_reason = if state == ExecState::Rejected {
        msg.reject_reason.map(RejectReason::as_u8).unwrap_or(0)
    } else {
        0
    };

    let cancel_reason = if matches!(state, ExecState::Cancelled | ExecState::Replaced) {
        msg.cancel_reason.map(CancelReason::as_u8).unwrap_or(0)
    } else {
        0
    };

    let (fill_price, fill_qty) = match (state, msg.fill_price, msg.fill_qty) {
        (ExecState::PartiallyFilled | ExecState::Filled, Some(p), Some(q)) => {
            (p.as_ticks(), q.as_lots())
        }
        _ => (0, 0),
    };

    let leaves_qty = match state {
        ExecState::Accepted | ExecState::PartiallyFilled => {
            msg.leaves_qty.map(Qty::as_lots).unwrap_or(0)
        }
        ExecState::Filled | ExecState::Rejected | ExecState::Cancelled | ExecState::Replaced => 0,
    };

    let w = ExecReportWire {
        engine_seq: msg.engine_seq.as_raw(),
        order_id: msg.order_id.as_raw(),
        account_id: msg.account_id.as_raw(),
        state: state.as_u8(),
        reject_reason,
        cancel_reason,
        _pad0: 0,
        fill_price,
        fill_qty,
        leaves_qty,
        recv_ts: msg.recv_ts.as_nanos() as u64,
        emit_ts: msg.emit_ts.as_nanos() as u64,
    };
    out.extend_from_slice(w.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_accepted() -> ExecReport {
        ExecReport {
            engine_seq: EngineSeq::new(42),
            order_id: OrderId::new(7).expect("ok"),
            account_id: AccountId::new(3).expect("ok"),
            state: ExecState::Accepted,
            fill_price: None,
            fill_qty: None,
            leaves_qty: Some(Qty::new(10).expect("ok")),
            reject_reason: None,
            cancel_reason: None,
            recv_ts: RecvTs::new(100),
            emit_ts: RecvTs::new(101),
        }
    }

    fn sample_rejected() -> ExecReport {
        ExecReport {
            engine_seq: EngineSeq::new(43),
            order_id: OrderId::new(7).expect("ok"),
            account_id: AccountId::new(3).expect("ok"),
            state: ExecState::Rejected,
            fill_price: None,
            fill_qty: None,
            leaves_qty: None,
            reject_reason: Some(RejectReason::PriceBand),
            cancel_reason: None,
            recv_ts: RecvTs::new(200),
            emit_ts: RecvTs::new(201),
        }
    }

    fn sample_partial_fill() -> ExecReport {
        ExecReport {
            engine_seq: EngineSeq::new(44),
            order_id: OrderId::new(7).expect("ok"),
            account_id: AccountId::new(3).expect("ok"),
            state: ExecState::PartiallyFilled,
            fill_price: Some(Price::new(100).expect("ok")),
            fill_qty: Some(Qty::new(3).expect("ok")),
            leaves_qty: Some(Qty::new(7).expect("ok")),
            reject_reason: None,
            cancel_reason: None,
            recv_ts: RecvTs::new(300),
            emit_ts: RecvTs::new(301),
        }
    }

    #[test]
    fn test_exec_report_wire_size_is_64() {
        assert_eq!(SIZE, 64);
    }

    #[test]
    fn test_exec_report_accepted_roundtrip() {
        let msg = sample_accepted();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_exec_report_rejected_roundtrip() {
        let msg = sample_rejected();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_exec_report_partial_fill_roundtrip() {
        let msg = sample_partial_fill();
        let mut buf = Vec::new();
        encode(&msg, &mut buf);
        assert_eq!(parse(&buf).expect("decode"), msg);
    }

    #[test]
    fn test_exec_report_truncated_returns_payload_size_error() {
        let buf = [0u8; SIZE - 1];
        assert!(matches!(parse(&buf), Err(WireError::PayloadSize { .. })));
    }

    #[test]
    fn test_exec_report_non_zero_pad_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_accepted(), &mut buf);
        buf[PAD0_OFFSET] = 0xFF;
        assert_eq!(parse(&buf), Err(WireError::NonZeroPad(PAD0_OFFSET)));
    }

    /// `state == Accepted` paired with a non-zero `reject_reason` byte
    /// is malformed. The decoder must reject after raw decode.
    #[test]
    fn test_exec_report_accepted_with_reject_reason_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_accepted(), &mut buf);
        // reject_reason is at offset 21 (after engine_seq + order_id +
        // account_id + state).
        buf[21] = RejectReason::PriceBand.as_u8();
        assert!(matches!(
            parse(&buf),
            Err(WireError::InconsistentPayload(_))
        ));
    }

    /// `state == Rejected` without a `reject_reason` byte is malformed.
    #[test]
    fn test_exec_report_rejected_without_reason_returns_err() {
        let mut buf = Vec::new();
        encode(&sample_rejected(), &mut buf);
        buf[21] = 0; // clear reject_reason
        assert!(matches!(
            parse(&buf),
            Err(WireError::InconsistentPayload(_))
        ));
    }

    /// `encode` canonicalises state-irrelevant fields. A sloppy caller
    /// that puts a `reject_reason` on an `Accepted` report sees that
    /// reason silently dropped on the wire — the decoded message comes
    /// back without it.
    #[test]
    fn test_exec_report_encode_canonicalises_irrelevant_fields() {
        let mut sloppy = sample_accepted();
        sloppy.reject_reason = Some(RejectReason::PriceBand); // not allowed on Accepted
        sloppy.cancel_reason = Some(CancelReason::UserRequested); // also not allowed

        let mut buf = Vec::new();
        encode(&sloppy, &mut buf);
        // Decode and verify the canonicalised version came back clean.
        let decoded = parse(&buf).expect("decode");
        assert_eq!(decoded.state, ExecState::Accepted);
        assert!(decoded.reject_reason.is_none());
        assert!(decoded.cancel_reason.is_none());
    }
}
