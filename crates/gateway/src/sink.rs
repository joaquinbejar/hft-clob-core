//! Outbound channel sink. Engine emits typed [`wire::outbound::Outbound`]
//! into the sink; an async writer task drains the channel and pushes
//! framed bytes to every subscribed TCP session.
//!
//! This module bridges the synchronous engine thread to the async
//! TCP writers. The sink itself is sync — it only enqueues onto a
//! `tokio::sync::broadcast` channel via the channel's `send` (which
//! tolerates being called outside the runtime).
//!
//! Backpressure: the broadcast channel buffers up to a configurable
//! depth; subscribers that fall behind by more than the buffer
//! receive `RecvError::Lagged` and are expected to disconnect /
//! re-subscribe. The engine never blocks.

use bytes::Bytes;
use marketdata::{OutboundSink, encoder};
use tokio::sync::broadcast;
use tracing::warn;
use wire::outbound::Outbound;

use crate::listener::OutboundSubscriber;

/// Channel-backed [`OutboundSink`]. The engine thread holds the
/// `Sender`; tokio writer tasks `subscribe()` to receive framed
/// bytes ready for TCP `write_all`.
///
/// `Clone` is cheap — the inner `broadcast::Sender` is itself
/// `Clone`, and every clone broadcasts onto the same underlying
/// channel. The engine binary clones the sink into the listener
/// (Arc-wrapped, subscribe-only) so producer and subscribers share
/// one stream.
#[derive(Clone)]
pub struct ChannelSink {
    tx: broadcast::Sender<Bytes>,
}

impl ChannelSink {
    /// Construct a sink with the given broadcast channel capacity.
    /// Larger capacity tolerates short writer hiccups; smaller
    /// capacity favours liveness over backlog.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Subscribe to the framed outbound byte stream. Each subscriber
    /// receives every frame emitted *after* its subscription.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        self.tx.subscribe()
    }

    /// Number of currently active subscribers. Useful for liveness
    /// metrics; the sink itself does not gate emission on this.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

impl OutboundSubscriber for ChannelSink {
    fn subscribe(&self) -> broadcast::Receiver<Bytes> {
        ChannelSink::subscribe(self)
    }
}

impl OutboundSink for ChannelSink {
    fn emit(&mut self, msg: Outbound) {
        // Encode into a fresh Vec — the byte buffer is moved into
        // `Bytes` and shared across subscribers without further
        // copying. The encoder allocates one Vec per emission;
        // pooling that allocation is a follow-up.
        let mut buf = Vec::with_capacity(64);
        if let Err(err) = encoder::encode(&msg, &mut buf) {
            warn!(error = %err, "gateway: outbound encode failed, dropping message");
            return;
        }
        let frame = Bytes::from(buf);
        // `send` returns `Err(SendError(_))` only when no receivers
        // are subscribed — no listeners is normal during boot, not
        // an emission failure.
        let _ = self.tx.send(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{EngineSeq, Price, Qty, RecvTs, Side, TradeId};
    use wire::outbound::TradePrint;

    fn sample_trade(seq: u64, trade_id: u64) -> Outbound {
        Outbound::TradePrint(TradePrint {
            engine_seq: EngineSeq::new(seq),
            trade_id: TradeId::new(trade_id),
            price: Price::new(100).expect("ok"),
            qty: Qty::new(1).expect("ok"),
            aggressor_side: Side::Bid,
            emit_ts: RecvTs::new(0),
        })
    }

    #[tokio::test]
    async fn test_channel_sink_subscriber_receives_framed_bytes() {
        let mut sink = ChannelSink::new(16);
        let mut rx = sink.subscribe();
        sink.emit(sample_trade(1, 1));
        let frame = rx.recv().await.expect("frame");
        let (decoded, _total) = encoder::decode(&frame).expect("decode");
        match decoded {
            Outbound::TradePrint(t) => {
                assert_eq!(t.engine_seq.as_raw(), 1);
                assert_eq!(t.trade_id.as_raw(), 1);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_channel_sink_emit_without_subscribers_does_not_panic() {
        let mut sink = ChannelSink::new(4);
        sink.emit(sample_trade(1, 1));
        // Subscribing after emission must NOT see the dropped frame
        // — broadcast semantics.
        let mut rx = sink.subscribe();
        sink.emit(sample_trade(2, 2));
        let frame = rx.recv().await.expect("frame");
        let (decoded, _) = encoder::decode(&frame).expect("decode");
        match decoded {
            Outbound::TradePrint(t) => assert_eq!(t.engine_seq.as_raw(), 2),
            _ => panic!("unexpected variant"),
        }
    }

    #[tokio::test]
    async fn test_channel_sink_engine_seq_monotonic_through_byte_stream() {
        let mut sink = ChannelSink::new(64);
        let mut rx = sink.subscribe();
        for i in 1..=10 {
            sink.emit(sample_trade(i, i));
        }
        let mut last = 0u64;
        for _ in 1..=10 {
            let frame = rx.recv().await.expect("frame");
            let (decoded, _) = encoder::decode(&frame).expect("decode");
            let seq = match decoded {
                Outbound::TradePrint(t) => t.engine_seq.as_raw(),
                _ => panic!("expected TradePrint"),
            };
            assert!(seq > last, "engine_seq must be strictly monotonic");
            last = seq;
        }
    }
}
