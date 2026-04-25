//! Domain types — newtypes wrapping integer primitives and stable
//! enums crossing the wire.

mod account_id;
mod client_ts;
mod engine_seq;
mod order_id;
mod price;
mod qty;
mod recv_ts;
mod trade_id;

mod cancel_reason;
mod order_type;
mod reject_reason;
mod side;
mod tif;

pub use account_id::{AccountId, AccountIdError};
pub use client_ts::ClientTs;
pub use engine_seq::{EngineSeq, EngineSeqError};
pub use order_id::{OrderId, OrderIdError};
pub use price::{Price, PriceError};
pub use qty::{Qty, QtyError};
pub use recv_ts::RecvTs;
pub use trade_id::{TradeId, TradeIdError};

pub use cancel_reason::{CancelReason, CancelReasonError};
pub use order_type::{OrderType, OrderTypeError};
pub use reject_reason::{RejectReason, RejectReasonError};
pub use side::{Side, SideError};
pub use tif::{Tif, TifError};
