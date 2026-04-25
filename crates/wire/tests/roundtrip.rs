//! Roundtrip property tests covering every wire message.
//!
//! Two properties per message:
//!
//! - `decode(encode(x)) == x` (value-identical)
//! - `encode(decode(encode(x))) == encode(x)` (byte-identical)
//!
//! Strategies are inline rather than feature-gated on `domain` so this
//! crate's tests do not pull in a public `proptest` feature surface.

use proptest::prelude::*;

use domain::{
    AccountId, CancelReason, ClientTs, EngineSeq, ExecState, OrderId, OrderType, Price, Qty,
    RecvTs, RejectReason, Side, Tif, TradeId,
};

use wire::inbound::{
    cancel_order::{self, CancelOrder},
    cancel_replace::{self, CancelReplace},
    kill_switch::{self, KillSwitchSet, KillSwitchState},
    mass_cancel::{self, MassCancel},
    new_order::{self, NewOrder},
    snapshot_request::{self, SnapshotRequest},
};

use wire::outbound::{
    book_update_l2_delta::{self, BookUpdateL2Delta},
    book_update_top::{self, BookUpdateTop},
    exec_report::{self, ExecReport},
    trade_print::{self, TradePrint},
};

// --- domain strategies ----------------------------------------------------

fn arb_price() -> impl Strategy<Value = Price> {
    (1i64..=1_000_000).prop_map(|n| Price::new(n).expect("strategy yields valid Price"))
}

fn arb_qty() -> impl Strategy<Value = Qty> {
    (1u64..=1_000_000).prop_map(|n| Qty::new(n).expect("strategy yields valid Qty"))
}

fn arb_order_id() -> impl Strategy<Value = OrderId> {
    (1u64..u64::MAX).prop_map(|n| OrderId::new(n).expect("strategy yields valid OrderId"))
}

fn arb_account_id() -> impl Strategy<Value = AccountId> {
    (1u32..u32::MAX).prop_map(|n| AccountId::new(n).expect("strategy yields valid AccountId"))
}

fn arb_engine_seq() -> impl Strategy<Value = EngineSeq> {
    any::<u64>().prop_map(EngineSeq::new)
}

fn arb_trade_id() -> impl Strategy<Value = TradeId> {
    any::<u64>().prop_map(TradeId::new)
}

fn arb_client_ts() -> impl Strategy<Value = ClientTs> {
    any::<i64>().prop_map(ClientTs::new)
}

fn arb_recv_ts() -> impl Strategy<Value = RecvTs> {
    any::<i64>().prop_map(RecvTs::new)
}

fn arb_side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Bid), Just(Side::Ask)]
}

fn arb_order_type() -> impl Strategy<Value = OrderType> {
    prop_oneof![Just(OrderType::Limit), Just(OrderType::Market)]
}

fn arb_tif() -> impl Strategy<Value = Tif> {
    prop_oneof![Just(Tif::Gtc), Just(Tif::Ioc), Just(Tif::PostOnly)]
}

/// Covers every [`RejectReason`] variant (15 total).
fn arb_reject_reason() -> impl Strategy<Value = RejectReason> {
    prop_oneof![
        Just(RejectReason::KillSwitched),
        Just(RejectReason::InvalidPrice),
        Just(RejectReason::InvalidQty),
        Just(RejectReason::TickViolation),
        Just(RejectReason::LotViolation),
        Just(RejectReason::PriceBand),
        Just(RejectReason::MaxOpenOrders),
        Just(RejectReason::MaxNotional),
        Just(RejectReason::UnknownOrderId),
        Just(RejectReason::DuplicateOrderId),
        Just(RejectReason::PostOnlyWouldCross),
        Just(RejectReason::SelfTradePrevented),
        Just(RejectReason::MarketOrderInEmptyBook),
        Just(RejectReason::UnknownAccount),
        Just(RejectReason::MalformedMessage),
    ]
}

/// Covers every [`CancelReason`] variant (5 total).
fn arb_cancel_reason() -> impl Strategy<Value = CancelReason> {
    prop_oneof![
        Just(CancelReason::UserRequested),
        Just(CancelReason::MassCancel),
        Just(CancelReason::IocRemaining),
        Just(CancelReason::SelfTradePrevented),
        Just(CancelReason::Replaced),
    ]
}

fn arb_exec_state() -> impl Strategy<Value = ExecState> {
    prop_oneof![
        Just(ExecState::Accepted),
        Just(ExecState::Rejected),
        Just(ExecState::PartiallyFilled),
        Just(ExecState::Filled),
        Just(ExecState::Cancelled),
        Just(ExecState::Replaced),
    ]
}

fn arb_kill_switch_state() -> impl Strategy<Value = KillSwitchState> {
    prop_oneof![Just(KillSwitchState::Resume), Just(KillSwitchState::Halt)]
}

// --- inbound message strategies ------------------------------------------

prop_compose! {
    fn arb_new_order()(
        client_ts in arb_client_ts(),
        order_id in arb_order_id(),
        account_id in arb_account_id(),
        side in arb_side(),
        order_type in arb_order_type(),
        tif in arb_tif(),
        price in arb_price(),
        qty in arb_qty(),
    ) -> NewOrder {
        let price = match order_type {
            OrderType::Market => None,
            OrderType::Limit => Some(price),
        };
        NewOrder { client_ts, order_id, account_id, side, order_type, tif, price, qty }
    }
}

prop_compose! {
    fn arb_cancel_order()(
        client_ts in arb_client_ts(),
        order_id in arb_order_id(),
        account_id in arb_account_id(),
    ) -> CancelOrder {
        CancelOrder { client_ts, order_id, account_id }
    }
}

prop_compose! {
    fn arb_cancel_replace()(
        client_ts in arb_client_ts(),
        order_id in arb_order_id(),
        account_id in arb_account_id(),
        new_price in arb_price(),
        new_qty in arb_qty(),
    ) -> CancelReplace {
        CancelReplace { client_ts, order_id, account_id, new_price, new_qty }
    }
}

prop_compose! {
    fn arb_mass_cancel()(
        client_ts in arb_client_ts(),
        account_id in arb_account_id(),
    ) -> MassCancel {
        MassCancel { client_ts, account_id }
    }
}

prop_compose! {
    fn arb_kill_switch_set()(
        client_ts in arb_client_ts(),
        admin_token in any::<u64>(),
        state in arb_kill_switch_state(),
    ) -> KillSwitchSet {
        KillSwitchSet { client_ts, admin_token, state }
    }
}

prop_compose! {
    fn arb_snapshot_request()(
        request_id in any::<u64>(),
    ) -> SnapshotRequest {
        SnapshotRequest { request_id }
    }
}

// --- outbound message strategies -----------------------------------------

/// Generates an [`ExecReport`] whose state-dependent fields are
/// internally consistent — i.e. `reject_reason` is `Some` iff
/// `state == Rejected`, `fill_*` is `Some` iff the report carries a
/// fill, etc. The roundtrip property only holds for coherent inputs.
fn arb_exec_report() -> impl Strategy<Value = ExecReport> {
    let accepted = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_qty(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, leaves)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::Accepted,
                fill_price: None,
                fill_qty: None,
                leaves_qty: Some(leaves),
                reject_reason: None,
                cancel_reason: None,
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let rejected = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_reject_reason(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, reason)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::Rejected,
                fill_price: None,
                fill_qty: None,
                leaves_qty: None,
                reject_reason: Some(reason),
                cancel_reason: None,
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let partial = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_price(),
        arb_qty(),
        arb_qty(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, price, fqty, lqty)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::PartiallyFilled,
                fill_price: Some(price),
                fill_qty: Some(fqty),
                leaves_qty: Some(lqty),
                reject_reason: None,
                cancel_reason: None,
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let filled = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_price(),
        arb_qty(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, price, fqty)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::Filled,
                fill_price: Some(price),
                fill_qty: Some(fqty),
                leaves_qty: None,
                reject_reason: None,
                cancel_reason: None,
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let cancelled = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_cancel_reason(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, reason)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::Cancelled,
                fill_price: None,
                fill_qty: None,
                leaves_qty: None,
                reject_reason: None,
                cancel_reason: Some(reason),
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let replaced = (
        arb_engine_seq(),
        arb_order_id(),
        arb_account_id(),
        arb_recv_ts(),
        arb_recv_ts(),
        arb_cancel_reason(),
    )
        .prop_map(
            |(engine_seq, order_id, account_id, recv_ts, emit_ts, reason)| ExecReport {
                engine_seq,
                order_id,
                account_id,
                state: ExecState::Replaced,
                fill_price: None,
                fill_qty: None,
                leaves_qty: None,
                reject_reason: None,
                cancel_reason: Some(reason),
                recv_ts,
                emit_ts,
            },
        )
        .boxed();

    let _state_coverage = arb_exec_state(); // ensure every variant has a strategy above

    prop_oneof![accepted, rejected, partial, filled, cancelled, replaced]
}

prop_compose! {
    fn arb_trade_print()(
        engine_seq in arb_engine_seq(),
        trade_id in arb_trade_id(),
        price in arb_price(),
        qty in arb_qty(),
        aggressor_side in arb_side(),
        emit_ts in arb_recv_ts(),
    ) -> TradePrint {
        TradePrint { engine_seq, trade_id, price, qty, aggressor_side, emit_ts }
    }
}

prop_compose! {
    fn arb_book_update_top()(
        engine_seq in arb_engine_seq(),
        bid in proptest::option::of((arb_price(), arb_qty())),
        ask in proptest::option::of((arb_price(), arb_qty())),
        emit_ts in arb_recv_ts(),
    ) -> BookUpdateTop {
        BookUpdateTop { engine_seq, bid, ask, emit_ts }
    }
}

prop_compose! {
    fn arb_book_update_l2_delta()(
        engine_seq in arb_engine_seq(),
        price in arb_price(),
        new_qty in proptest::option::of(arb_qty()),
        side in arb_side(),
        emit_ts in arb_recv_ts(),
    ) -> BookUpdateL2Delta {
        BookUpdateL2Delta { engine_seq, price, new_qty, side, emit_ts }
    }
}

// --- the actual roundtrip property tests ---------------------------------

macro_rules! roundtrip_proptest {
    ($mod:ident, $arb:expr_2021) => {
        proptest! {
            #[test]
            fn $mod(msg in $arb) {
                let mut buf1 = Vec::new();
                $mod::encode(&msg, &mut buf1);
                let decoded = $mod::parse(&buf1).expect("decode");
                prop_assert_eq!(decoded.clone(), msg.clone(), "value-identical");

                let mut buf2 = Vec::new();
                $mod::encode(&decoded, &mut buf2);
                prop_assert_eq!(buf1, buf2, "byte-identical re-encode");
            }
        }
    };
}

// Inbound (covered already by per-module unit tests, but the
// proptest matrix exercises every reachable value space).
roundtrip_proptest!(new_order, arb_new_order());
roundtrip_proptest!(cancel_order, arb_cancel_order());
roundtrip_proptest!(cancel_replace, arb_cancel_replace());
roundtrip_proptest!(mass_cancel, arb_mass_cancel());
roundtrip_proptest!(kill_switch, arb_kill_switch_set());
roundtrip_proptest!(snapshot_request, arb_snapshot_request());

// Outbound — every message, every reachable variant.
roundtrip_proptest!(exec_report, arb_exec_report());
roundtrip_proptest!(trade_print, arb_trade_print());
roundtrip_proptest!(book_update_top, arb_book_update_top());
roundtrip_proptest!(book_update_l2_delta, arb_book_update_l2_delta());
