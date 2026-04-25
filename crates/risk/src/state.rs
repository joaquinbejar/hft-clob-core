//! Pre-trade risk state and checks.
//!
//! The check order matches `doc/DESIGN.md` § 7:
//!
//! 1. Kill switch — halts all client commands; resting orders are
//!    untouched (the engine pipeline still emits market data and serves
//!    snapshot requests).
//! 2. Message validation — domain newtypes (`Price`, `Qty`, `OrderId`,
//!    `AccountId`) guarantee tick / lot / range invariants by
//!    construction at the wire-decode boundary, so any rejection here
//!    is `MalformedMessage` from the wire crate. Risk does not re-check
//!    those.
//! 3. Price band — `|price - last_trade_price| <= last * PRICE_BAND_BPS / 10_000`.
//!    Skipped while `last_trade_price` is `None` (warm-up / freshly
//!    started session).
//! 4. Per-account limits — open-orders count + aggregate notional.
//!    Market orders use the best opposite price (the engine passes it
//!    in) as the "far-touch worst-case" reference. A market order
//!    arriving against a one-sided book is rejected with
//!    `MarketOrderInEmptyBook` before the limit check.
//!
//! State mutations happen *after* matching:
//!
//! - [`RiskState::on_order_resting`] — bumps the per-account open count
//!   and notional when an order rests on the book (limit GTC, or limit
//!   IOC with leftover, or post-only that did not cross).
//! - [`RiskState::on_order_removed`] — drops the per-account open count
//!   and notional on cancel / full-fill / STP / mass-cancel.
//! - [`RiskState::on_fill`] — refreshes `last_trade_price` for the
//!   price band on the next inbound order.

use std::collections::HashMap;

use domain::consts::{MAX_NOTIONAL_PER_ACCT, MAX_OPEN_ORDERS_PER_ACCT, PRICE_BAND_BPS};
use domain::{AccountId, OrderType, Price, Qty, RejectReason};
use wire::inbound::{CancelOrder, CancelReplace, MassCancel, NewOrder};

/// Per-account aggregate state tracked for limit checks.
///
/// Both fields are scalar counters; the engine pipeline updates them in
/// lock-step with the matching outcome (rest / fill / cancel).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AccountState {
    /// Number of resting open orders on the book.
    pub open_orders: u32,
    /// Aggregate notional of resting orders (`price_ticks * qty_lots`).
    pub notional: u128,
}

/// Risk and operational state. Single-writer per the engine's pipeline.
///
/// Iteration of `per_account` is **never** allowed to reach an outbound
/// message — CLAUDE.md "no unordered iteration into outputs". The
/// `HashMap` is point-lookup only (`get` / `entry`); every accessor
/// here keys by an explicit `AccountId`.
pub struct RiskState {
    kill_switch: bool,
    per_account: HashMap<AccountId, AccountState>,
    last_trade_price: Option<Price>,
}

impl RiskState {
    /// Construct an empty risk state with the kill switch disengaged
    /// and no last-trade reference (price band skipped until the first
    /// fill).
    #[must_use]
    pub fn new() -> Self {
        Self {
            kill_switch: false,
            per_account: HashMap::new(),
            last_trade_price: None,
        }
    }

    /// `true` when the kill switch is engaged. Inbound client commands
    /// are rejected with `KillSwitched`; admin kill-switch toggles
    /// remain accepted.
    #[must_use]
    #[inline]
    pub fn is_kill_switched(&self) -> bool {
        self.kill_switch
    }

    /// Engage / disengage the kill switch. The engine routes the
    /// `KillSwitchSet` admin message here directly — never gated by
    /// the kill switch itself.
    #[inline]
    pub fn set_kill_switch(&mut self, engaged: bool) {
        self.kill_switch = engaged;
    }

    /// Most-recent traded price; `None` while the session has not
    /// produced a fill yet.
    #[must_use]
    #[inline]
    pub fn last_trade_price(&self) -> Option<Price> {
        self.last_trade_price
    }

    /// Refresh the price band reference. The engine calls this once per
    /// fill emitted by the matching core.
    #[inline]
    pub fn on_fill(&mut self, trade_price: Price) {
        self.last_trade_price = Some(trade_price);
    }

    /// Read-only view of one account's state. Returns `None` when the
    /// account has never placed an order.
    #[must_use]
    #[inline]
    pub fn account(&self, account: AccountId) -> Option<AccountState> {
        self.per_account.get(&account).copied()
    }

    /// Bump the per-account counters after the matching core leaves a
    /// portion of the order resting on the book. `notional` is the
    /// resting portion's notional in `price_ticks * qty_lots` units.
    ///
    /// # Errors
    /// [`RejectReason::MaxNotional`] is returned when the per-account
    /// notional would overflow the documented ceiling. The caller must
    /// not have rested the order yet — this hook also serves as the
    /// final pre-rest invariant check.
    pub fn on_order_resting(
        &mut self,
        account: AccountId,
        notional: u128,
    ) -> Result<(), RejectReason> {
        let entry = self.per_account.entry(account).or_default();
        let new_open = entry
            .open_orders
            .checked_add(1)
            .ok_or(RejectReason::MaxOpenOrders)?;
        if new_open > MAX_OPEN_ORDERS_PER_ACCT {
            return Err(RejectReason::MaxOpenOrders);
        }
        let new_notional = entry
            .notional
            .checked_add(notional)
            .ok_or(RejectReason::MaxNotional)?;
        if new_notional > MAX_NOTIONAL_PER_ACCT {
            return Err(RejectReason::MaxNotional);
        }
        entry.open_orders = new_open;
        entry.notional = new_notional;
        Ok(())
    }

    /// Drop the per-account counters when an order leaves the book
    /// (explicit cancel, STP cancel, mass-cancel, full fill,
    /// cancel-replace lose-priority cancel).
    pub fn on_order_removed(&mut self, account: AccountId, notional: u128) {
        if let Some(entry) = self.per_account.get_mut(&account) {
            entry.open_orders = entry.open_orders.saturating_sub(1);
            entry.notional = entry.notional.saturating_sub(notional);
            if entry.open_orders == 0 && entry.notional == 0 {
                self.per_account.remove(&account);
            }
        }
    }

    /// Pre-trade check for a `NewOrder`. Order matches DESIGN.md § 7:
    /// kill → market-in-empty-book → price band → per-account limits.
    ///
    /// `best_opposite` is the engine's view of the opposite-side best
    /// price at command-arrival time. `None` means the opposite side
    /// is empty.
    ///
    /// # Errors
    /// One of [`RejectReason::KillSwitched`],
    /// [`RejectReason::MarketOrderInEmptyBook`],
    /// [`RejectReason::PriceBand`], [`RejectReason::MaxOpenOrders`],
    /// [`RejectReason::MaxNotional`].
    pub fn check_new_order(
        &self,
        msg: &NewOrder,
        best_opposite: Option<Price>,
    ) -> Result<(), RejectReason> {
        if self.kill_switch {
            return Err(RejectReason::KillSwitched);
        }

        // Resolve the effective price for downstream checks.
        // - Limit: the order's own limit price.
        // - Market: the best opposite (far-touch worst-case bound).
        let effective_price = match msg.order_type {
            OrderType::Limit => match msg.price {
                Some(p) => p,
                // Domain newtype guarantees Limit + price both decoded
                // from wire; this branch is structurally unreachable.
                None => return Err(RejectReason::MalformedMessage),
            },
            OrderType::Market => match best_opposite {
                Some(p) => p,
                None => return Err(RejectReason::MarketOrderInEmptyBook),
            },
        };

        check_price_band(effective_price, self.last_trade_price)?;
        self.check_account_limits(msg.account_id, effective_price, msg.qty)?;
        Ok(())
    }

    /// Pre-trade check for a `CancelOrder`. Only the kill switch
    /// gates a cancel — the actual order-id existence check lives in
    /// the matching core (`UnknownOrderId`).
    ///
    /// # Errors
    /// [`RejectReason::KillSwitched`].
    #[inline]
    pub fn check_cancel(&self, _msg: &CancelOrder) -> Result<(), RejectReason> {
        if self.kill_switch {
            return Err(RejectReason::KillSwitched);
        }
        Ok(())
    }

    /// Pre-trade check for a `CancelReplace`. Kill switch + the new
    /// `(price, qty)` pass the price band and the new notional fits
    /// the per-account cap given the *replacement delta* (resting
    /// notional after cancel - notional before).
    ///
    /// The engine pipeline must:
    ///
    /// 1. Call this method first.
    /// 2. Call the matching core's `replace`.
    /// 3. Adjust `RiskState` via `on_order_removed` / `on_order_resting`
    ///    to reflect the actual replacement.
    ///
    /// # Errors
    /// [`RejectReason::KillSwitched`], [`RejectReason::PriceBand`],
    /// [`RejectReason::MaxNotional`].
    pub fn check_cancel_replace(&self, msg: &CancelReplace) -> Result<(), RejectReason> {
        if self.kill_switch {
            return Err(RejectReason::KillSwitched);
        }
        check_price_band(msg.new_price, self.last_trade_price)?;
        // Replace-up notional is best-checked after the cancel side
        // is committed; here we only confirm the new (price, qty)
        // does not by itself blow the cap. The engine's
        // post-matching call to `on_order_resting` re-checks the
        // resulting state against the cap and rejects if the delta
        // overflows.
        let order_notional = notional_of(msg.new_price, msg.new_qty);
        if order_notional > MAX_NOTIONAL_PER_ACCT {
            return Err(RejectReason::MaxNotional);
        }
        Ok(())
    }

    /// Pre-trade check for a `MassCancel`. Only the kill switch
    /// applies — the operation itself targets resting orders the
    /// matching core will iterate in deterministic order.
    ///
    /// # Errors
    /// [`RejectReason::KillSwitched`].
    #[inline]
    pub fn check_mass_cancel(&self, _msg: &MassCancel) -> Result<(), RejectReason> {
        if self.kill_switch {
            return Err(RejectReason::KillSwitched);
        }
        Ok(())
    }

    fn check_account_limits(
        &self,
        account: AccountId,
        effective_price: Price,
        qty: Qty,
    ) -> Result<(), RejectReason> {
        let entry = self.per_account.get(&account).copied().unwrap_or_default();
        let new_open = entry
            .open_orders
            .checked_add(1)
            .ok_or(RejectReason::MaxOpenOrders)?;
        if new_open > MAX_OPEN_ORDERS_PER_ACCT {
            return Err(RejectReason::MaxOpenOrders);
        }
        let order_notional = notional_of(effective_price, qty);
        let new_notional = entry
            .notional
            .checked_add(order_notional)
            .ok_or(RejectReason::MaxNotional)?;
        if new_notional > MAX_NOTIONAL_PER_ACCT {
            return Err(RejectReason::MaxNotional);
        }
        Ok(())
    }
}

impl Default for RiskState {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute `price_ticks * qty_lots` in `u128`, never overflowing for
/// any valid `Price` × `Qty` pair (`i64::MAX` × `u64::MAX` fits in
/// `u128`).
#[must_use]
#[inline]
pub fn notional_of(price: Price, qty: Qty) -> u128 {
    // `Price` is invariant `> 0`, so the cast to `u128` is exact.
    (price.as_ticks() as u128) * (qty.as_lots() as u128)
}

/// Apply the price-band check. `last` is the last-trade reference.
/// `None` signals warm-up — the band check is skipped.
fn check_price_band(price: Price, last: Option<Price>) -> Result<(), RejectReason> {
    let Some(reference) = last else {
        return Ok(());
    };
    let p = price.as_ticks() as i128;
    let r = reference.as_ticks() as i128;
    let diff = (p - r).unsigned_abs();
    // Reformulated to avoid division: |price - r| * 10_000 <= r * BPS.
    let lhs = diff.checked_mul(10_000).ok_or(RejectReason::PriceBand)?;
    let rhs = (r as u128)
        .checked_mul(PRICE_BAND_BPS as u128)
        .ok_or(RejectReason::PriceBand)?;
    if lhs > rhs {
        return Err(RejectReason::PriceBand);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{ClientTs, OrderId, Side, Tif};

    fn new_order_limit(account: u32, price: i64, qty: u64) -> NewOrder {
        NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side: Side::Bid,
            order_type: OrderType::Limit,
            tif: Tif::Gtc,
            price: Some(Price::new(price).expect("ok")),
            qty: Qty::new(qty).expect("ok"),
        }
    }

    fn new_order_market(account: u32, qty: u64) -> NewOrder {
        NewOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(account).expect("ok"),
            side: Side::Bid,
            order_type: OrderType::Market,
            tif: Tif::Ioc,
            price: None,
            qty: Qty::new(qty).expect("ok"),
        }
    }

    // -----------------------------------------------------------------
    // Kill switch
    // -----------------------------------------------------------------

    #[test]
    fn test_kill_switch_rejects_new_order() {
        let mut s = RiskState::new();
        s.set_kill_switch(true);
        let msg = new_order_limit(7, 100, 5);
        assert_eq!(
            s.check_new_order(&msg, Some(Price::new(100).expect("ok"))),
            Err(RejectReason::KillSwitched)
        );
    }

    #[test]
    fn test_kill_switch_rejects_cancel() {
        let mut s = RiskState::new();
        s.set_kill_switch(true);
        let msg = CancelOrder {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
        };
        assert_eq!(s.check_cancel(&msg), Err(RejectReason::KillSwitched));
    }

    #[test]
    fn test_kill_switch_rejects_cancel_replace() {
        let mut s = RiskState::new();
        s.set_kill_switch(true);
        let msg = CancelReplace {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            new_price: Price::new(100).expect("ok"),
            new_qty: Qty::new(5).expect("ok"),
        };
        assert_eq!(
            s.check_cancel_replace(&msg),
            Err(RejectReason::KillSwitched)
        );
    }

    #[test]
    fn test_kill_switch_rejects_mass_cancel() {
        let mut s = RiskState::new();
        s.set_kill_switch(true);
        let msg = MassCancel {
            client_ts: ClientTs::new(0),
            account_id: AccountId::new(7).expect("ok"),
        };
        assert_eq!(s.check_mass_cancel(&msg), Err(RejectReason::KillSwitched));
    }

    #[test]
    fn test_disengage_kill_switch_lets_new_order_pass() {
        let mut s = RiskState::new();
        s.set_kill_switch(true);
        s.set_kill_switch(false);
        let msg = new_order_limit(7, 100, 5);
        assert_eq!(s.check_new_order(&msg, None), Ok(()));
    }

    // -----------------------------------------------------------------
    // Price band
    // -----------------------------------------------------------------

    #[test]
    fn test_price_band_skipped_until_first_fill() {
        let s = RiskState::new();
        // Way out of any band — passes because last_trade_price is None.
        let msg = new_order_limit(7, 1_000_000, 1);
        assert_eq!(s.check_new_order(&msg, None), Ok(()));
    }

    #[test]
    fn test_price_band_rejects_above_band() {
        let mut s = RiskState::new();
        s.on_fill(Price::new(100).expect("ok"));
        // PRICE_BAND_BPS = 500 → 5 % band → max 105.
        let msg = new_order_limit(7, 106, 1);
        assert_eq!(s.check_new_order(&msg, None), Err(RejectReason::PriceBand));
    }

    #[test]
    fn test_price_band_rejects_below_band() {
        let mut s = RiskState::new();
        s.on_fill(Price::new(100).expect("ok"));
        let msg = new_order_limit(7, 94, 1);
        assert_eq!(s.check_new_order(&msg, None), Err(RejectReason::PriceBand));
    }

    #[test]
    fn test_price_band_accepts_at_boundary() {
        let mut s = RiskState::new();
        s.on_fill(Price::new(100).expect("ok"));
        // Exactly +5 % is allowed (`<=`).
        let msg = new_order_limit(7, 105, 1);
        assert_eq!(s.check_new_order(&msg, None), Ok(()));
    }

    // -----------------------------------------------------------------
    // Market order in empty book
    // -----------------------------------------------------------------

    #[test]
    fn test_market_order_in_empty_book_rejected() {
        let s = RiskState::new();
        let msg = new_order_market(7, 5);
        assert_eq!(
            s.check_new_order(&msg, None),
            Err(RejectReason::MarketOrderInEmptyBook)
        );
    }

    #[test]
    fn test_market_order_with_best_opposite_passes() {
        let s = RiskState::new();
        let msg = new_order_market(7, 5);
        assert_eq!(
            s.check_new_order(&msg, Some(Price::new(100).expect("ok"))),
            Ok(())
        );
    }

    // -----------------------------------------------------------------
    // Per-account limits
    // -----------------------------------------------------------------

    #[test]
    fn test_max_open_orders_rejected_after_ceiling() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        // Bring the account up to the ceiling.
        let entry = s.per_account.entry(acct).or_default();
        entry.open_orders = MAX_OPEN_ORDERS_PER_ACCT;
        let msg = new_order_limit(7, 100, 1);
        assert_eq!(
            s.check_new_order(&msg, None),
            Err(RejectReason::MaxOpenOrders)
        );
    }

    #[test]
    fn test_max_notional_rejected_when_new_order_overflows() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        let entry = s.per_account.entry(acct).or_default();
        entry.notional = MAX_NOTIONAL_PER_ACCT - 50;
        // 100 ticks * 1 lot = 100 notional → exceeds remaining 50.
        let msg = new_order_limit(7, 100, 1);
        assert_eq!(
            s.check_new_order(&msg, None),
            Err(RejectReason::MaxNotional)
        );
    }

    #[test]
    fn test_on_order_resting_increments_account_state() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        s.on_order_resting(acct, 500).expect("rest");
        let st = s.account(acct).expect("present");
        assert_eq!(st.open_orders, 1);
        assert_eq!(st.notional, 500);
    }

    #[test]
    fn test_on_order_removed_decrements_and_prunes_zeroed_account() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        s.on_order_resting(acct, 500).expect("rest");
        s.on_order_removed(acct, 500);
        assert!(s.account(acct).is_none());
    }

    #[test]
    fn test_on_order_removed_saturating_subtract_does_not_panic() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        // Removing without a matching add must not panic and must not
        // create a phantom entry.
        s.on_order_removed(acct, 999);
        assert!(s.account(acct).is_none());
    }

    #[test]
    fn test_on_order_resting_rejects_at_max_open_orders() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        s.per_account.insert(
            acct,
            AccountState {
                open_orders: MAX_OPEN_ORDERS_PER_ACCT,
                notional: 0,
            },
        );
        assert_eq!(
            s.on_order_resting(acct, 1),
            Err(RejectReason::MaxOpenOrders)
        );
    }

    #[test]
    fn test_on_order_resting_rejects_at_max_notional() {
        let mut s = RiskState::new();
        let acct = AccountId::new(7).expect("ok");
        s.per_account.insert(
            acct,
            AccountState {
                open_orders: 1,
                notional: MAX_NOTIONAL_PER_ACCT,
            },
        );
        assert_eq!(s.on_order_resting(acct, 1), Err(RejectReason::MaxNotional));
    }

    // -----------------------------------------------------------------
    // CancelReplace
    // -----------------------------------------------------------------

    #[test]
    fn test_cancel_replace_passes_when_within_band_and_cap() {
        let mut s = RiskState::new();
        s.on_fill(Price::new(100).expect("ok"));
        let msg = CancelReplace {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            new_price: Price::new(102).expect("ok"),
            new_qty: Qty::new(5).expect("ok"),
        };
        assert_eq!(s.check_cancel_replace(&msg), Ok(()));
    }

    #[test]
    fn test_cancel_replace_rejected_outside_price_band() {
        let mut s = RiskState::new();
        s.on_fill(Price::new(100).expect("ok"));
        let msg = CancelReplace {
            client_ts: ClientTs::new(0),
            order_id: OrderId::new(1).expect("ok"),
            account_id: AccountId::new(7).expect("ok"),
            new_price: Price::new(150).expect("ok"),
            new_qty: Qty::new(5).expect("ok"),
        };
        assert_eq!(s.check_cancel_replace(&msg), Err(RejectReason::PriceBand));
    }

    // -----------------------------------------------------------------
    // notional_of helper
    // -----------------------------------------------------------------

    #[test]
    fn test_notional_of_multiplies_price_and_qty() {
        let p = Price::new(7).expect("ok");
        let q = Qty::new(11).expect("ok");
        assert_eq!(notional_of(p, q), 77);
    }

    #[test]
    fn test_notional_of_handles_max_values() {
        // i64::MAX * u64::MAX still fits in u128.
        let p = Price::MAX;
        let q = Qty::MAX;
        // No overflow expected.
        let _ = notional_of(p, q);
    }
}
