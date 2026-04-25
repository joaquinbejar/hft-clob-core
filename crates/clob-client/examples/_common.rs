//! Shared example helpers — order-id sequencer, account / price /
//! qty helpers, panic-on-error wrappers used across the
//! `examples/localhost/*` binaries.
//!
//! Each example file `mod common;` includes this file via
//! `#[path = "_common.rs"]`. Cargo does not surface examples'
//! sibling modules automatically, so the `#[path]` attribute is
//! the standard pattern.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use clob_client::Client;
use domain::{AccountId, ClientTs, OrderId, OrderType, Price, Qty, Side, Tif};
use wire::inbound::{
    CancelOrder, CancelReplace, KillSwitchSet, KillSwitchState, MassCancel, NewOrder,
    SnapshotRequest,
};

/// Default address the engine binary listens on.
pub const ENGINE_ADDR: &str = "127.0.0.1:9000";

/// Override via `CLOB_ENGINE_ADDR` for CI / docker.
pub fn engine_addr() -> String {
    std::env::var("CLOB_ENGINE_ADDR").unwrap_or_else(|_| ENGINE_ADDR.to_string())
}

/// Returns a unique `OrderId` per call within the process. The engine
/// rejects duplicates with `RejectReason::DuplicateOrderId`.
pub fn next_order_id() -> OrderId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let v = NEXT.fetch_add(1, Ordering::Relaxed);
    OrderId::new(v).expect("OrderId > 0")
}

/// Connect to the engine. Panics with a clear message if the engine is
/// not running.
pub async fn connect() -> Client {
    let addr = engine_addr();
    Client::connect(&addr)
        .await
        .unwrap_or_else(|e| panic!("connect to {addr} failed: {e}; is the engine running?"))
}

/// Build a typed `NewOrder` limit.
pub fn limit(id: OrderId, account: u32, side: Side, price: i64, qty: u64, tif: Tif) -> NewOrder {
    NewOrder {
        client_ts: ClientTs::new(0),
        order_id: id,
        account_id: AccountId::new(account).expect("account > 0"),
        side,
        order_type: OrderType::Limit,
        tif,
        price: Some(Price::new(price).expect("price > 0")),
        qty: Qty::new(qty).expect("qty > 0"),
    }
}

/// Build a typed `NewOrder` market.
pub fn market(id: OrderId, account: u32, side: Side, qty: u64) -> NewOrder {
    NewOrder {
        client_ts: ClientTs::new(0),
        order_id: id,
        account_id: AccountId::new(account).expect("account > 0"),
        side,
        order_type: OrderType::Market,
        tif: Tif::Ioc,
        price: None,
        qty: Qty::new(qty).expect("qty > 0"),
    }
}

/// Build a typed `CancelOrder`.
pub fn cancel(id: OrderId, account: u32) -> CancelOrder {
    CancelOrder {
        client_ts: ClientTs::new(0),
        order_id: id,
        account_id: AccountId::new(account).expect("account > 0"),
    }
}

/// Build a typed `CancelReplace`.
pub fn cancel_replace(id: OrderId, account: u32, new_price: i64, new_qty: u64) -> CancelReplace {
    CancelReplace {
        client_ts: ClientTs::new(0),
        order_id: id,
        account_id: AccountId::new(account).expect("account > 0"),
        new_price: Price::new(new_price).expect("price > 0"),
        new_qty: Qty::new(new_qty).expect("qty > 0"),
    }
}

/// Build a typed `MassCancel`.
pub fn mass_cancel(account: u32) -> MassCancel {
    MassCancel {
        client_ts: ClientTs::new(0),
        account_id: AccountId::new(account).expect("account > 0"),
    }
}

/// Build a typed `KillSwitchSet`.
pub fn kill_switch(state: KillSwitchState) -> KillSwitchSet {
    KillSwitchSet {
        client_ts: ClientTs::new(0),
        state,
        admin_token: 0,
    }
}

/// Build a typed `SnapshotRequest`.
pub fn snapshot_request(request_id: u64) -> SnapshotRequest {
    SnapshotRequest { request_id }
}
