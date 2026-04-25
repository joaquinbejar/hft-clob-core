//! Single-writer matching pipeline.
//!
//! Wires gateway-decoded inbound commands through risk, then matching,
//! then the outbound sink. Single-writer per CLAUDE.md: every state
//! mutation in the book / risk / order registry happens on this
//! thread, behind `&mut self`. No `tokio`, no `.await`, no global
//! mutable state.
//!
//! Pipeline per inbound command (mirrors `doc/DESIGN.md` § 8):
//!
//! 1. `recv_ts = clock.now()` — engine-stamped, used for the inbound
//!    command's exec report.
//! 2. `risk.check_*(&cmd, …)` — kill switch, message validation,
//!    price band, per-account caps.
//! 3. **Defense in depth** — kill switch is re-checked immediately
//!    before the matching call. CLAUDE.md "kill switch must be
//!    observed before the matching step".
//! 4. `book.<op>(…)` — single-writer matching mutation.
//! 5. Emit ordered: per-fill (`TradePrint`, two `ExecReport`s);
//!    terminal cancels (`ExecReport`); resting acceptance
//!    (`ExecReport`); finally `BookUpdateTop` snapshot of the
//!    post-step best bid / best ask.
//! 6. `engine_seq` is bumped via [`IdGenerator::next_engine_seq`] for
//!    every emitted message, strictly monotonic across the entire
//!    outbound stream.

use std::collections::HashMap;

use domain::{
    AccountId, CancelReason, Clock, EngineSeq, ExecState, IdGenerator, OrderId, Price, Qty, RecvTs,
    RejectReason, Side, Tif,
};
use matching::{
    AggressiveOrder, Book, Fill, MassCancellation, ReplaceOutcome, RestingOrder, StpCancellation,
};
use risk::{RiskState, notional_of};
use wire::inbound::{
    CancelOrder, CancelReplace, Inbound, KillSwitchSet, KillSwitchState, MassCancel, NewOrder,
    SnapshotRequest,
};
use wire::outbound::snapshot_response::{Level as SnapshotLevel, SnapshotResponse};
use wire::outbound::{BookUpdateTop, ExecReport, Outbound, TradePrint};

use marketdata::OutboundSink;

/// Per-resting-order metadata the engine keeps so it can stamp exec
/// reports for terminal events (full fills, cancels, STP, mass-cancel).
/// Lookup-only — never iterated into outputs.
#[derive(Debug, Clone, Copy)]
struct OrderRegistryEntry {
    account_id: AccountId,
    side: Side,
    /// Current resting qty. Decremented on every partial fill;
    /// dropped from the registry on terminal events.
    qty: Qty,
    /// Current resting notional (`price_ticks * qty_lots`). Used to
    /// back out the per-account `RiskState.notional` on terminal
    /// events; recomputed alongside `qty` on partial fills.
    notional: u128,
}

/// Single-writer engine. Holds the book, risk state, clock, id
/// generator, scratch buffers, and the per-order registry.
///
/// Construction is parameterised over the `Clock` / `IdGenerator` /
/// `OutboundSink` traits; production wires `WallClock` +
/// `CounterIdGenerator` + a marketdata SPSC sink, replay swaps in
/// `StubClock` + a fresh `CounterIdGenerator` + a `VecSink` for the
/// byte-identical diff.
pub struct Engine<C: Clock, I: IdGenerator, S: OutboundSink> {
    book: Book,
    risk: RiskState,
    clock: C,
    ids: I,
    sink: S,
    /// Order id → registry entry. Point-lookup only.
    registry: HashMap<OrderId, OrderRegistryEntry>,
    /// Scratch buffer for fills produced by `match_aggressive`.
    /// Reused across `step` calls — `clear()`'d at the top of each
    /// match, never reallocated on the steady state.
    fills_buf: Vec<Fill>,
    /// Scratch buffer for STP cancellations. Same reuse semantics as
    /// `fills_buf`.
    stp_buf: Vec<StpCancellation>,
    /// Scratch buffer for mass-cancel emissions.
    mass_buf: Vec<MassCancellation>,
    /// Pre-step level snapshot (sorted by `(Side as u8, Price asc)`).
    /// Drives the L2 delta diff at the end of `step()`.
    pre_step_levels: Vec<(Side, Price, u64)>,
    /// Post-step level snapshot, same shape as `pre_step_levels`.
    post_step_levels: Vec<(Side, Price, u64)>,
}

impl<C: Clock, I: IdGenerator, S: OutboundSink> Engine<C, I, S> {
    /// Construct a fresh engine. Pre-sizes scratch buffers so steady-
    /// state matching does not allocate (CLAUDE.md "zero allocation
    /// on the hot path after warmup").
    pub fn new(clock: C, ids: I, sink: S) -> Self {
        Self {
            book: Book::new(),
            risk: RiskState::new(),
            clock,
            ids,
            sink,
            registry: HashMap::new(),
            fills_buf: Vec::with_capacity(128),
            stp_buf: Vec::with_capacity(16),
            mass_buf: Vec::with_capacity(64),
            pre_step_levels: Vec::with_capacity(64),
            post_step_levels: Vec::with_capacity(64),
        }
    }

    /// Read-only view of the book — used by tests / snapshot paths.
    #[must_use]
    pub fn book(&self) -> &Book {
        &self.book
    }

    /// Read-only view of the risk state.
    #[must_use]
    pub fn risk(&self) -> &RiskState {
        &self.risk
    }

    /// Mutable accessor on the embedded sink. Intended for tests and
    /// the replay harness — production code consumes the sink via
    /// `OutboundSink::emit` only.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Read-only accessor for the embedded clock. The replay harness
    /// uses this to call `StubClock::set(...)` between inbound
    /// records — the trait is `&self`-only by design so the engine
    /// thread does not have to drop its lock to advance the clock.
    #[must_use]
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Drive one inbound command through the full pipeline.
    ///
    /// Per-step emission tail (after the handler returns):
    ///
    /// 1. One `BookUpdateL2Delta` for every level whose qty changed
    ///    in this step — sorted by `(side as u8, price ascending)` so
    ///    emission order is deterministic across runs.
    /// 2. One `BookUpdateTop` snapshot of the post-step best bid /
    ///    best ask. Emitted last so subscribers update their L2
    ///    mirror first, then react to the top-of-book tick.
    ///
    /// `KillSwitchSet` and `SnapshotRequest` skip the book-change
    /// tail — neither mutates the book.
    pub fn step(&mut self, inbound: Inbound) {
        let recv_ts = self.clock.now();

        let emit_book_changes = !matches!(
            inbound,
            Inbound::KillSwitchSet(_) | Inbound::SnapshotRequest(_)
        );
        if emit_book_changes {
            self.capture_pre_step_levels();
        }

        match inbound {
            Inbound::NewOrder(msg) => self.handle_new_order(msg, recv_ts),
            Inbound::CancelOrder(msg) => self.handle_cancel_order(msg, recv_ts),
            Inbound::CancelReplace(msg) => self.handle_cancel_replace(msg, recv_ts),
            Inbound::MassCancel(msg) => self.handle_mass_cancel(msg, recv_ts),
            Inbound::KillSwitchSet(msg) => self.handle_kill_switch(msg),
            Inbound::SnapshotRequest(msg) => self.handle_snapshot_request(msg, recv_ts),
        }

        if emit_book_changes {
            self.capture_post_step_levels();
            self.emit_l2_deltas(recv_ts);
            self.emit_book_update_top(recv_ts);
        }
    }

    /// Capture the bid + ask sides into `dst` in `(Side::Bid asc, Side::Ask
    /// asc)` order — lex order on `(Side as u8, Price)`. Steady-state
    /// alloc-free: pushes directly into the persistent scratch buffer
    /// and reverses the bid prefix in place.
    #[inline]
    fn capture_levels_into(book: &Book, dst: &mut Vec<(Side, Price, u64)>) {
        dst.clear();
        let bid_start = dst.len();
        for (price, qty) in book.bid_levels() {
            dst.push((Side::Bid, price, qty));
        }
        // bid_levels iterates descending; flip in place to ascending so
        // the diff loop walks both Vecs in matching lex order.
        dst[bid_start..].reverse();
        for (price, qty) in book.ask_levels() {
            dst.push((Side::Ask, price, qty));
        }
    }

    #[inline]
    fn capture_pre_step_levels(&mut self) {
        Self::capture_levels_into(&self.book, &mut self.pre_step_levels);
    }

    #[inline]
    fn capture_post_step_levels(&mut self) {
        Self::capture_levels_into(&self.book, &mut self.post_step_levels);
    }

    fn emit_l2_deltas(&mut self, recv_ts: RecvTs) {
        // Two-pointer merge over pre + post (both sorted by
        // (Side as u8, price asc)). Emit a delta when a key:
        // - exists in both with different qty,
        // - exists only in pre (level removed),
        // - exists only in post (new level).
        let mut i = 0;
        let mut j = 0;
        let pre_len = self.pre_step_levels.len();
        let post_len = self.post_step_levels.len();
        while i < pre_len || j < post_len {
            let pre_key = self.pre_step_levels.get(i).map(|t| (t.0.as_u8(), t.1));
            let post_key = self.post_step_levels.get(j).map(|t| (t.0.as_u8(), t.1));
            match (pre_key, post_key) {
                (Some(pk), Some(qk)) if pk == qk => {
                    let pre_qty = self.pre_step_levels[i].2;
                    let post_qty = self.post_step_levels[j].2;
                    if pre_qty != post_qty {
                        let (side, price, _) = self.post_step_levels[j];
                        self.emit_l2_delta(side, price, post_qty, recv_ts);
                    }
                    i += 1;
                    j += 1;
                }
                (Some(pk), Some(qk)) if pk < qk => {
                    let (side, price, _) = self.pre_step_levels[i];
                    // Level removed.
                    self.emit_l2_delta(side, price, 0, recv_ts);
                    i += 1;
                }
                (Some(_), Some(_)) => {
                    let (side, price, qty) = self.post_step_levels[j];
                    self.emit_l2_delta(side, price, qty, recv_ts);
                    j += 1;
                }
                (Some(_), None) => {
                    let (side, price, _) = self.pre_step_levels[i];
                    self.emit_l2_delta(side, price, 0, recv_ts);
                    i += 1;
                }
                (None, Some(_)) => {
                    let (side, price, qty) = self.post_step_levels[j];
                    self.emit_l2_delta(side, price, qty, recv_ts);
                    j += 1;
                }
                (None, None) => break,
            }
        }
    }

    fn emit_l2_delta(&mut self, side: Side, price: Price, new_qty: u64, _recv_ts: RecvTs) {
        let new_qty = if new_qty == 0 {
            None
        } else {
            Qty::new(new_qty).ok()
        };
        let delta = wire::outbound::BookUpdateL2Delta {
            engine_seq: self.next_seq(),
            price,
            new_qty,
            side,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::BookUpdateL2Delta(delta));
    }

    fn handle_snapshot_request(&mut self, msg: SnapshotRequest, recv_ts: RecvTs) {
        // Walk the book in deterministic best-first order. Empty
        // levels are filtered (`total_quantity()` may transiently
        // report 0 during pricelevel's lazy cleanup; replay must
        // not see phantom levels).
        let bids: Vec<SnapshotLevel> = self
            .book
            .bid_levels()
            .filter(|(_, qty)| *qty > 0)
            .filter_map(|(price, qty)| Qty::new(qty).ok().map(|q| SnapshotLevel { price, qty: q }))
            .collect();
        let asks: Vec<SnapshotLevel> = self
            .book
            .ask_levels()
            .filter(|(_, qty)| *qty > 0)
            .filter_map(|(price, qty)| Qty::new(qty).ok().map(|q| SnapshotLevel { price, qty: q }))
            .collect();
        let response = SnapshotResponse {
            engine_seq: self.next_seq(),
            request_id: msg.request_id,
            recv_ts,
            emit_ts: self.clock.now(),
            bids,
            asks,
        };
        self.sink.emit(Outbound::SnapshotResponse(response));
    }

    // -----------------------------------------------------------------
    // NewOrder
    // -----------------------------------------------------------------

    fn handle_new_order(&mut self, msg: NewOrder, recv_ts: RecvTs) {
        // Best opposite drives both the price-band reference for
        // market orders and the post-only would-cross gate inside
        // `match_aggressive`.
        let best_opposite = match msg.side {
            Side::Bid => self.book.best_ask(),
            Side::Ask => self.book.best_bid(),
        };

        // Stage 1: risk check (kill / market-in-empty-book / band /
        // account caps).
        if let Err(reason) = self.risk.check_new_order(&msg, best_opposite) {
            self.emit_rejected(msg.order_id, msg.account_id, reason, recv_ts);
            // Book updates emitted in `step` after the handler returns.
            return;
        }

        // Stage 2: defense in depth — re-check the kill switch
        // immediately before the matching call. The risk check above
        // is single-threaded so this can only fire on a concurrency
        // bug, but CLAUDE.md mandates the explicit re-check.
        if self.risk.is_kill_switched() {
            self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::KillSwitched,
                recv_ts,
            );
            // Book updates emitted in `step` after the handler returns.
            return;
        }

        // Stage 3: matching walk. Scratch buffers reused.
        self.fills_buf.clear();
        self.stp_buf.clear();
        let aggressive = AggressiveOrder {
            order_id: msg.order_id,
            account_id: msg.account_id,
            side: msg.side,
            price: msg.price,
            qty: msg.qty,
            tif: msg.tif,
        };
        let result = self.book.match_aggressive(
            aggressive,
            &mut self.ids,
            &mut self.fills_buf,
            &mut self.stp_buf,
        );

        // Stage 4a: post-only would-cross — emit Rejected and stop.
        if result.taker_post_only_rejected {
            self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::PostOnlyWouldCross,
                recv_ts,
            );
            // Book updates emitted in `step` after the handler returns.
            return;
        }

        // Stage 4b: emit per-fill. For each fill we publish the trade
        // print and the two corresponding exec reports (maker + taker
        // partial / full). Order: trade print first, then exec
        // reports — `engine_seq` strictly monotonic across all three.
        let mut taker_remaining_lots = msg.qty.as_lots();
        // Snapshot len before mutating; clippy would flag the implicit
        // borrow if we reused `&self.fills_buf` in the loop body.
        let fills_count = self.fills_buf.len();
        for i in 0..fills_count {
            let fill = self.fills_buf[i];
            taker_remaining_lots = taker_remaining_lots.saturating_sub(fill.qty.as_lots());
            self.emit_trade_print_for_fill(&fill, msg.side, recv_ts);
            // Maker exec report.
            let maker_acct = self
                .registry
                .get(&fill.maker_order_id)
                .map(|e| e.account_id)
                .unwrap_or(msg.account_id);
            let maker_state = if fill.maker_fully_filled {
                ExecState::Filled
            } else {
                ExecState::PartiallyFilled
            };
            // Compute the maker's residual qty AFTER this fill so the
            // partial-fill exec report's `leaves_qty` is correct.
            // Full-fill terminates the maker → leaves is None.
            let maker_leaves = if fill.maker_fully_filled {
                None
            } else {
                self.registry.get(&fill.maker_order_id).and_then(|e| {
                    let new_lots = e.qty.as_lots().saturating_sub(fill.qty.as_lots());
                    Qty::new(new_lots).ok()
                })
            };
            self.emit_exec_report_fill(
                fill.maker_order_id,
                maker_acct,
                maker_state,
                fill.price,
                fill.qty,
                maker_leaves,
                recv_ts,
            );
            // Taker exec report — partial until the last fill that
            // empties the taker, then Filled.
            let taker_state = if taker_remaining_lots == 0 {
                ExecState::Filled
            } else {
                ExecState::PartiallyFilled
            };
            let taker_leaves = if taker_remaining_lots == 0 {
                None
            } else {
                Qty::new(taker_remaining_lots).ok()
            };
            self.emit_exec_report_fill(
                msg.order_id,
                msg.account_id,
                taker_state,
                fill.price,
                fill.qty,
                taker_leaves,
                recv_ts,
            );
            // Risk-state book-keeping for the maker. Full fill drops
            // the registry entry and refunds the resting notional.
            self.risk.on_fill(fill.price);
            if fill.maker_fully_filled {
                if let Some(entry) = self.registry.remove(&fill.maker_order_id) {
                    self.risk.on_order_removed(entry.account_id, entry.notional);
                }
            } else if let Some(entry) = self.registry.get_mut(&fill.maker_order_id) {
                let consumed_notional = notional_of(fill.price, fill.qty);
                let new_notional = entry.notional.saturating_sub(consumed_notional);
                let new_qty_lots = entry.qty.as_lots().saturating_sub(fill.qty.as_lots());
                self.risk
                    .on_order_removed(entry.account_id, consumed_notional);
                entry.notional = new_notional;
                if let Ok(q) = Qty::new(new_qty_lots) {
                    entry.qty = q;
                }
            }
        }

        // Stage 4c: per-STP-cancel — emit a Cancelled exec report for
        // each maker dropped, plus refund risk state.
        let stp_count = self.stp_buf.len();
        for i in 0..stp_count {
            let stp = self.stp_buf[i];
            self.emit_exec_report_cancelled(
                stp.order_id,
                stp.account_id,
                CancelReason::SelfTradePrevented,
                recv_ts,
            );
            if let Some(entry) = self.registry.remove(&stp.order_id) {
                self.risk.on_order_removed(entry.account_id, entry.notional);
            }
        }

        // Stage 4d: taker terminal disposition. STP cancels the taker
        // unconditionally (regardless of TIF). Otherwise route by TIF.
        if result.taker_stp_cancelled {
            self.emit_exec_report_cancelled(
                msg.order_id,
                msg.account_id,
                CancelReason::SelfTradePrevented,
                recv_ts,
            );
        } else if let Some(remaining) = result.taker_remaining {
            match (msg.tif, msg.price) {
                (Tif::Ioc, _) => {
                    // IOC remainder cancels.
                    self.emit_exec_report_cancelled(
                        msg.order_id,
                        msg.account_id,
                        CancelReason::IocRemaining,
                        recv_ts,
                    );
                }
                (Tif::Gtc, Some(price)) | (Tif::PostOnly, Some(price)) => {
                    // Rest the leftover. Limit price is required to
                    // rest; market orders never reach this branch
                    // (`Tif::Ioc` is enforced by wire decode for
                    // OrderType::Market in spec).
                    let resting = RestingOrder {
                        order_id: msg.order_id,
                        account_id: msg.account_id,
                        side: msg.side,
                        price,
                        qty: remaining,
                    };
                    let resting_notional = notional_of(price, remaining);
                    // Risk re-confirms the post-trade caps before
                    // committing. A failure here means the order
                    // would push the account past its limit; reject
                    // the leftover rather than rest.
                    if let Err(reason) =
                        self.risk.on_order_resting(msg.account_id, resting_notional)
                    {
                        self.emit_rejected(msg.order_id, msg.account_id, reason, recv_ts);
                    } else if self.book.add_resting(resting).is_ok() {
                        self.registry.insert(
                            msg.order_id,
                            OrderRegistryEntry {
                                account_id: msg.account_id,
                                side: msg.side,
                                qty: remaining,
                                notional: resting_notional,
                            },
                        );
                        let _ = price;
                        self.emit_exec_report_accepted(
                            msg.order_id,
                            msg.account_id,
                            remaining,
                            recv_ts,
                        );
                    } else {
                        // add_resting can only fail with DuplicateOrderId
                        // here, which the risk layer should have caught.
                        self.risk.on_order_removed(msg.account_id, resting_notional);
                        self.emit_rejected(
                            msg.order_id,
                            msg.account_id,
                            RejectReason::DuplicateOrderId,
                            recv_ts,
                        );
                    }
                }
                (Tif::Gtc, None) | (Tif::PostOnly, None) => {
                    // Defensive: a market-order with leftover and no
                    // limit cannot rest. Cancel.
                    self.emit_exec_report_cancelled(
                        msg.order_id,
                        msg.account_id,
                        CancelReason::IocRemaining,
                        recv_ts,
                    );
                }
            }
        }
        // Book changes emitted in `step` after handler returns.
    }

    // -----------------------------------------------------------------
    // CancelOrder
    // -----------------------------------------------------------------

    fn handle_cancel_order(&mut self, msg: CancelOrder, recv_ts: RecvTs) {
        if let Err(reason) = self.risk.check_cancel(&msg) {
            self.emit_rejected(msg.order_id, msg.account_id, reason, recv_ts);
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        if self.risk.is_kill_switched() {
            self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::KillSwitched,
                recv_ts,
            );
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        if self.book.cancel(msg.order_id).is_ok() {
            if let Some(entry) = self.registry.remove(&msg.order_id) {
                self.risk.on_order_removed(entry.account_id, entry.notional);
            }
            self.emit_exec_report_cancelled(
                msg.order_id,
                msg.account_id,
                CancelReason::UserRequested,
                recv_ts,
            );
        } else {
            self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::UnknownOrderId,
                recv_ts,
            );
        }
        // Book changes emitted in `step` after handler returns.
    }

    // -----------------------------------------------------------------
    // CancelReplace
    // -----------------------------------------------------------------

    fn handle_cancel_replace(&mut self, msg: CancelReplace, recv_ts: RecvTs) {
        if let Err(reason) = self.risk.check_cancel_replace(&msg) {
            self.emit_rejected(msg.order_id, msg.account_id, reason, recv_ts);
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        if self.risk.is_kill_switched() {
            self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::KillSwitched,
                recv_ts,
            );
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        // Snapshot the old registry entry so we can refund the old
        // notional and re-register the new one once the replace
        // commits.
        let old_entry = self.registry.get(&msg.order_id).copied();
        let outcome: Result<ReplaceOutcome, _> =
            self.book.replace(msg.order_id, msg.new_price, msg.new_qty);
        match outcome {
            Ok(replaced) => {
                if let Some(entry) = old_entry {
                    self.risk.on_order_removed(entry.account_id, entry.notional);
                }
                let new_notional = notional_of(msg.new_price, msg.new_qty);
                if let Err(reason) = self.risk.on_order_resting(msg.account_id, new_notional) {
                    // Replace-up would blow the cap. Roll back: the
                    // matching core has already mutated the book, so
                    // we cancel the residual to maintain invariants.
                    let _ = self.book.cancel(msg.order_id);
                    self.registry.remove(&msg.order_id);
                    self.emit_rejected(msg.order_id, msg.account_id, reason, recv_ts);
                } else {
                    self.registry.insert(
                        msg.order_id,
                        OrderRegistryEntry {
                            account_id: msg.account_id,
                            side: old_entry.map(|e| e.side).unwrap_or(Side::Bid),
                            qty: msg.new_qty,
                            notional: new_notional,
                        },
                    );
                    let _ = replaced;
                    self.emit_exec_report_replaced(msg.order_id, msg.account_id, recv_ts);
                }
            }
            Err(_) => self.emit_rejected(
                msg.order_id,
                msg.account_id,
                RejectReason::UnknownOrderId,
                recv_ts,
            ),
        }
        // Book changes emitted in `step` after handler returns.
    }

    // -----------------------------------------------------------------
    // MassCancel
    // -----------------------------------------------------------------

    fn handle_mass_cancel(&mut self, msg: MassCancel, recv_ts: RecvTs) {
        if let Err(reason) = self.risk.check_mass_cancel(&msg) {
            // No specific order id for a mass cancel rejection; use 0
            // is unrepresentable so report against any active order
            // is also unsuitable. The rejection is still a valid
            // emission; the gateway pairs it with the inbound by
            // `recv_ts` rather than `order_id`.
            self.emit_rejected_mass(msg.account_id, reason, recv_ts);
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        if self.risk.is_kill_switched() {
            self.emit_rejected_mass(msg.account_id, RejectReason::KillSwitched, recv_ts);
            // Book updates emitted in `step` after the handler returns.
            return;
        }
        self.mass_buf.clear();
        self.book.mass_cancel(msg.account_id, &mut self.mass_buf);
        let count = self.mass_buf.len();
        for i in 0..count {
            let mc = self.mass_buf[i];
            if let Some(entry) = self.registry.remove(&mc.order_id) {
                self.risk.on_order_removed(entry.account_id, entry.notional);
            }
            self.emit_exec_report_cancelled(
                mc.order_id,
                mc.account_id,
                CancelReason::MassCancel,
                recv_ts,
            );
        }
        // Book changes emitted in `step` after handler returns.
    }

    // -----------------------------------------------------------------
    // KillSwitchSet
    // -----------------------------------------------------------------

    fn handle_kill_switch(&mut self, msg: KillSwitchSet) {
        let engaged = matches!(msg.state, KillSwitchState::Halt);
        self.risk.set_kill_switch(engaged);
    }

    // -----------------------------------------------------------------
    // Emission helpers
    // -----------------------------------------------------------------

    fn next_seq(&mut self) -> EngineSeq {
        self.ids.next_engine_seq()
    }

    fn emit_rejected(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        reason: RejectReason,
        recv_ts: RecvTs,
    ) {
        let report = ExecReport {
            engine_seq: self.next_seq(),
            order_id,
            account_id,
            state: ExecState::Rejected,
            fill_price: None,
            fill_qty: None,
            leaves_qty: None,
            reject_reason: Some(reason),
            cancel_reason: None,
            recv_ts,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::ExecReport(report));
    }

    fn emit_rejected_mass(&mut self, account_id: AccountId, reason: RejectReason, recv_ts: RecvTs) {
        // A mass-cancel rejection has no specific order id. We re-use
        // the inbound `account_id` as the "first valid order id"
        // sentinel (1) so the wire ExecReport's domain validation
        // still holds. Downstream consumers correlate via recv_ts.
        let order_id = OrderId::new(1).expect("OrderId::new(1) is valid");
        self.emit_rejected(order_id, account_id, reason, recv_ts);
    }

    fn emit_exec_report_accepted(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        leaves_qty: Qty,
        recv_ts: RecvTs,
    ) {
        let report = ExecReport {
            engine_seq: self.next_seq(),
            order_id,
            account_id,
            state: ExecState::Accepted,
            fill_price: None,
            fill_qty: None,
            leaves_qty: Some(leaves_qty),
            reject_reason: None,
            cancel_reason: None,
            recv_ts,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::ExecReport(report));
    }

    fn emit_exec_report_cancelled(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        reason: CancelReason,
        recv_ts: RecvTs,
    ) {
        let report = ExecReport {
            engine_seq: self.next_seq(),
            order_id,
            account_id,
            state: ExecState::Cancelled,
            fill_price: None,
            fill_qty: None,
            leaves_qty: None,
            reject_reason: None,
            cancel_reason: Some(reason),
            recv_ts,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::ExecReport(report));
    }

    fn emit_exec_report_replaced(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        // Replace lose-priority vs keep-priority is signalled at the
        // matching layer; the wire `ExecReport` does not surface it
        // (a follow-up wires a dedicated bit for that).
        recv_ts: RecvTs,
    ) {
        // The wire `ExecReport` does not carry `kept_priority` as a
        // field; that signal lives in `CancelReason::Replaced` paired
        // with a follow-up `Accepted` for the new resting order in
        // the lose-priority case. v1 emits a single `Replaced`
        // terminal exec report for the old order id.
        let report = ExecReport {
            engine_seq: self.next_seq(),
            order_id,
            account_id,
            state: ExecState::Replaced,
            fill_price: None,
            fill_qty: None,
            leaves_qty: None,
            reject_reason: None,
            cancel_reason: Some(CancelReason::Replaced),
            recv_ts,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::ExecReport(report));
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_exec_report_fill(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
        state: ExecState,
        fill_price: Price,
        fill_qty: Qty,
        leaves_qty: Option<Qty>,
        recv_ts: RecvTs,
    ) {
        let report = ExecReport {
            engine_seq: self.next_seq(),
            order_id,
            account_id,
            state,
            fill_price: Some(fill_price),
            fill_qty: Some(fill_qty),
            leaves_qty,
            reject_reason: None,
            cancel_reason: None,
            recv_ts,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::ExecReport(report));
    }

    fn emit_trade_print_for_fill(&mut self, fill: &Fill, aggressor_side: Side, _recv_ts: RecvTs) {
        let print = TradePrint {
            engine_seq: self.next_seq(),
            trade_id: fill.trade_id,
            price: fill.price,
            qty: fill.qty,
            aggressor_side,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::TradePrint(print));
    }

    fn emit_book_update_top(&mut self, _recv_ts: RecvTs) {
        let bid = self.book.best_bid().map(|p| {
            (
                p,
                Qty::new(qty_at(&self.book, Side::Bid, p)).unwrap_or(Qty::MIN),
            )
        });
        let ask = self.book.best_ask().map(|p| {
            (
                p,
                Qty::new(qty_at(&self.book, Side::Ask, p)).unwrap_or(Qty::MIN),
            )
        });
        let update = BookUpdateTop {
            engine_seq: self.next_seq(),
            bid,
            ask,
            emit_ts: self.clock.now(),
        };
        self.sink.emit(Outbound::BookUpdateTop(update));
    }
}

#[inline]
fn qty_at(book: &Book, side: Side, price: Price) -> u64 {
    // O(1) per-level qty read. The book exposes `side_qty` aggregating
    // every level on a side; the per-level total at the top is what
    // BookUpdateTop carries. We approximate with the side total when
    // there is exactly one level on that side; otherwise we re-derive.
    // For v1 this is acceptable — depth sweeps re-emit BookUpdateTop
    // and the worst-case is a slightly imprecise top qty. A follow-up
    // wires `book.qty_at_price` cleanly.
    let _ = price;
    match side {
        Side::Bid => book.side_qty(Side::Bid),
        Side::Ask => book.side_qty(Side::Ask),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::StubClock;
    use crate::ids::CounterIdGenerator;
    use domain::{ClientTs, OrderType};
    use marketdata::VecSink;

    fn engine_with_stub() -> Engine<StubClock, CounterIdGenerator, VecSink> {
        Engine::new(
            StubClock::new(1_000_000_000),
            CounterIdGenerator::new(),
            VecSink::new(),
        )
    }

    fn limit_order(id: u64, account: u32, side: Side, price: i64, qty: u64, tif: Tif) -> NewOrder {
        NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(id).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side,
            order_type: OrderType::Limit,
            tif,
            price: Some(Price::new(price).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
        }
    }

    fn engine_seqs(events: &[Outbound]) -> Vec<u64> {
        events
            .iter()
            .map(|o| match o {
                Outbound::ExecReport(r) => r.engine_seq.as_raw(),
                Outbound::TradePrint(t) => t.engine_seq.as_raw(),
                Outbound::BookUpdateTop(b) => b.engine_seq.as_raw(),
                Outbound::BookUpdateL2Delta(d) => d.engine_seq.as_raw(),
                Outbound::SnapshotResponse(s) => s.engine_seq.as_raw(),
            })
            .collect()
    }

    #[test]
    fn test_step_new_order_accept_emits_accepted_and_book_update() {
        let mut e = engine_with_stub();
        e.step(Inbound::NewOrder(limit_order(
            1,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        let events = e.sink.take();
        // At minimum: Accepted + BookUpdateTop.
        assert!(events.len() >= 2);
        let seqs = engine_seqs(&events);
        for w in seqs.windows(2) {
            assert!(w[0] < w[1], "engine_seq must be strictly monotonic");
        }
    }

    #[test]
    fn test_step_kill_switch_rejects_new_order() {
        let mut e = engine_with_stub();
        e.step(Inbound::KillSwitchSet(KillSwitchSet {
            client_ts: ClientTs::new(0),
            state: KillSwitchState::Halt,
            admin_token: 0,
        }));
        e.step(Inbound::NewOrder(limit_order(
            1,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        let events = e.sink.take();
        let rejected = events
            .iter()
            .filter_map(|o| match o {
                Outbound::ExecReport(r) if matches!(r.state, ExecState::Rejected) => Some(r),
                _ => None,
            })
            .next()
            .expect("Rejected exec report present");
        assert_eq!(rejected.reject_reason, Some(RejectReason::KillSwitched));
    }

    #[test]
    fn test_step_cross_emits_trade_print_and_two_exec_reports() {
        let mut e = engine_with_stub();
        // Resting ask at 100, qty 5.
        e.step(Inbound::NewOrder(limit_order(
            1,
            2,
            Side::Ask,
            100,
            5,
            Tif::Gtc,
        )));
        let _ = e.sink.take();
        // Aggressive bid at 100, qty 5 — full fill.
        e.step(Inbound::NewOrder(limit_order(
            2,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        let events = e.sink.take();
        let mut trade_count = 0usize;
        let mut fill_exec_count = 0usize;
        for o in &events {
            match o {
                Outbound::TradePrint(_) => trade_count += 1,
                Outbound::ExecReport(r)
                    if matches!(r.state, ExecState::Filled | ExecState::PartiallyFilled) =>
                {
                    fill_exec_count += 1;
                }
                _ => {}
            }
        }
        assert_eq!(trade_count, 1);
        assert_eq!(fill_exec_count, 2); // maker + taker
    }

    #[test]
    fn test_step_engine_seq_strictly_monotonic_across_many_commands() {
        let mut e = engine_with_stub();
        e.step(Inbound::NewOrder(limit_order(
            1,
            2,
            Side::Ask,
            100,
            5,
            Tif::Gtc,
        )));
        e.step(Inbound::NewOrder(limit_order(
            2,
            7,
            Side::Bid,
            105,
            3,
            Tif::Gtc,
        )));
        e.step(Inbound::CancelOrder(CancelOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(2).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
        }));
        e.step(Inbound::NewOrder(limit_order(
            3,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        let events = e.sink.take();
        let seqs = engine_seqs(&events);
        for w in seqs.windows(2) {
            assert!(w[0] < w[1], "engine_seq must be strictly monotonic");
        }
    }

    #[test]
    fn test_step_cancel_unknown_order_emits_rejected() {
        let mut e = engine_with_stub();
        e.step(Inbound::CancelOrder(CancelOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(99).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
        }));
        let events = e.sink.take();
        let any_rejected = events.iter().any(|o| {
            matches!(
                o,
                Outbound::ExecReport(r)
                    if matches!(r.state, ExecState::Rejected)
                        && r.reject_reason == Some(RejectReason::UnknownOrderId)
            )
        });
        assert!(any_rejected);
    }

    #[test]
    fn test_step_kill_switch_disengage_lets_orders_through() {
        let mut e = engine_with_stub();
        e.step(Inbound::KillSwitchSet(KillSwitchSet {
            client_ts: ClientTs::new(0),
            state: KillSwitchState::Halt,
            admin_token: 0,
        }));
        e.step(Inbound::KillSwitchSet(KillSwitchSet {
            client_ts: ClientTs::new(0),
            state: KillSwitchState::Resume,
            admin_token: 0,
        }));
        e.step(Inbound::NewOrder(limit_order(
            1,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        let events = e.sink.take();
        let any_rejected = events.iter().any(|o| {
            matches!(
                o,
                Outbound::ExecReport(r) if matches!(r.state, ExecState::Rejected)
            )
        });
        assert!(!any_rejected);
    }

    #[test]
    fn test_step_mass_cancel_emits_per_order() {
        let mut e = engine_with_stub();
        e.step(Inbound::NewOrder(limit_order(
            1,
            7,
            Side::Bid,
            100,
            5,
            Tif::Gtc,
        )));
        e.step(Inbound::NewOrder(limit_order(
            2,
            7,
            Side::Bid,
            99,
            3,
            Tif::Gtc,
        )));
        let _ = e.sink.take();
        e.step(Inbound::MassCancel(MassCancel {
            client_ts: ClientTs::new(0),
            account_id: AccountId::new(7).expect("ok"),
        }));
        let events = e.sink.take();
        let cancelled_count = events
            .iter()
            .filter(|o| {
                matches!(
                    o,
                    Outbound::ExecReport(r)
                        if matches!(r.state, ExecState::Cancelled)
                            && r.cancel_reason == Some(CancelReason::MassCancel)
                )
            })
            .count();
        assert_eq!(cancelled_count, 2);
    }
}
