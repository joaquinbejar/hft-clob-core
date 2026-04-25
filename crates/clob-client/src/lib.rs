//! Localhost test / example client over the engine's framed TCP wire.
//!
//! Wraps a `tokio::net::TcpStream` with helpers for sending typed
//! [`wire::inbound::Inbound`] commands and reading typed
//! [`wire::outbound::Outbound`] events. Reuses the encoders / decoders
//! from `crates/wire/` and `crates/marketdata/`; never hand-rolls
//! bytes.
//!
//! Intended for two consumers:
//!
//! - `crates/clob-client/examples/localhost/*.rs` — one example per
//!   public-functionality bullet from issue #53.
//! - `crates/engine/tests/` — integration tests that drive the same
//!   wire path against a live engine listener.

#![warn(missing_docs)]

use std::time::Duration;

use bytes::BytesMut;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use marketdata::encoder;
use wire::framing::{FRAME_LEN_BYTES, Frame, MessageKind};
use wire::inbound::{
    self, CancelOrder, CancelReplace, Inbound, KillSwitchSet, MassCancel, NewOrder,
    SnapshotRequest, new_order,
};
use wire::outbound::Outbound;

/// Errors surfaced by the client helper.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Underlying TCP I/O failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Wire decode failed on the inbound buffer.
    #[error("wire error: {0}")]
    Wire(#[from] wire::WireError),
    /// Read timeout elapsed before the next outbound event.
    #[error("timed out waiting for outbound event after {0:?}")]
    Timeout(Duration),
    /// Peer closed the connection.
    #[error("peer closed connection")]
    Closed,
}

/// Wrapper around a `TcpStream` that speaks the engine's framed TCP
/// protocol. Holds a `BytesMut` decode buffer that survives across
/// `recv_*` calls so partial frames do not get dropped.
pub struct Client {
    stream: TcpStream,
    rx_buf: BytesMut,
}

impl Client {
    /// Connect to the engine listener at `addr`.
    pub async fn connect(addr: &str) -> Result<Self, ClientError> {
        let stream = TcpStream::connect(addr).await?;
        Ok(Self {
            stream,
            rx_buf: BytesMut::with_capacity(8192),
        })
    }

    /// Submit one inbound command. Encodes via the wire crate and
    /// writes the framed bytes to the socket.
    pub async fn send(&mut self, msg: &Inbound) -> Result<(), ClientError> {
        let mut payload = Vec::with_capacity(64);
        let kind = match msg {
            Inbound::NewOrder(n) => {
                inbound::new_order::encode(n, &mut payload);
                MessageKind::NewOrder
            }
            Inbound::CancelOrder(c) => {
                inbound::cancel_order::encode(c, &mut payload);
                MessageKind::CancelOrder
            }
            Inbound::CancelReplace(r) => {
                inbound::cancel_replace::encode(r, &mut payload);
                MessageKind::CancelReplace
            }
            Inbound::MassCancel(m) => {
                inbound::mass_cancel::encode(m, &mut payload);
                MessageKind::MassCancel
            }
            Inbound::KillSwitchSet(k) => {
                inbound::kill_switch::encode(k, &mut payload);
                MessageKind::KillSwitchSet
            }
            Inbound::SnapshotRequest(s) => {
                inbound::snapshot_request::encode(s, &mut payload);
                MessageKind::SnapshotRequest
            }
        };
        let mut frame = Vec::with_capacity(8 + payload.len());
        Frame::write(kind, &payload, &mut frame)?;
        self.stream.write_all(&frame).await?;
        Ok(())
    }

    /// Convenience: send a `NewOrder`.
    pub async fn send_new_order(&mut self, order: NewOrder) -> Result<(), ClientError> {
        let mut payload = Vec::with_capacity(64);
        new_order::encode(&order, &mut payload);
        let mut frame = Vec::with_capacity(8 + payload.len());
        Frame::write(MessageKind::NewOrder, &payload, &mut frame)?;
        self.stream.write_all(&frame).await?;
        Ok(())
    }

    /// Convenience: send a `CancelOrder`.
    pub async fn send_cancel(&mut self, msg: CancelOrder) -> Result<(), ClientError> {
        self.send(&Inbound::CancelOrder(msg)).await
    }

    /// Convenience: send a `CancelReplace`.
    pub async fn send_cancel_replace(&mut self, msg: CancelReplace) -> Result<(), ClientError> {
        self.send(&Inbound::CancelReplace(msg)).await
    }

    /// Convenience: send a `MassCancel`.
    pub async fn send_mass_cancel(&mut self, msg: MassCancel) -> Result<(), ClientError> {
        self.send(&Inbound::MassCancel(msg)).await
    }

    /// Convenience: send a `KillSwitchSet`.
    pub async fn send_kill_switch(&mut self, msg: KillSwitchSet) -> Result<(), ClientError> {
        self.send(&Inbound::KillSwitchSet(msg)).await
    }

    /// Convenience: send a `SnapshotRequest`.
    pub async fn send_snapshot_request(&mut self, msg: SnapshotRequest) -> Result<(), ClientError> {
        self.send(&Inbound::SnapshotRequest(msg)).await
    }

    /// Read the next outbound event, blocking until one arrives or the
    /// timeout elapses.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Outbound, ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Try to decode from whatever bytes we already have.
            if self.rx_buf.len() >= FRAME_LEN_BYTES {
                match encoder::decode(&self.rx_buf) {
                    Ok((msg, total)) => {
                        let _ = self.rx_buf.split_to(total);
                        return Ok(msg);
                    }
                    Err(wire::WireError::Truncated) => {
                        // Need more bytes.
                    }
                    Err(other) => return Err(ClientError::Wire(other)),
                }
            }
            // Read more bytes from the socket, bounded by the deadline.
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(ClientError::Timeout(timeout));
            }
            let remaining = deadline - now;
            let read_fut = self.stream.read_buf(&mut self.rx_buf);
            match tokio::time::timeout(remaining, read_fut).await {
                Ok(Ok(0)) => return Err(ClientError::Closed),
                Ok(Ok(_n)) => {}
                Ok(Err(e)) => return Err(ClientError::Io(e)),
                Err(_) => return Err(ClientError::Timeout(timeout)),
            }
        }
    }

    /// Read up to `max` outbound events or until the timeout elapses.
    /// Returns whatever was collected. Useful for examples that want
    /// "drain everything for the next 200ms".
    pub async fn recv_until_timeout(
        &mut self,
        timeout: Duration,
        max: usize,
    ) -> Result<Vec<Outbound>, ClientError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut out = Vec::new();
        while out.len() < max {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.recv(remaining).await {
                Ok(msg) => out.push(msg),
                Err(ClientError::Timeout(_)) => break,
                Err(other) => return Err(other),
            }
        }
        Ok(out)
    }

    /// Close the connection's write half (signals EOF to the engine).
    pub async fn shutdown(&mut self) -> Result<(), ClientError> {
        self.stream.shutdown().await?;
        Ok(())
    }
}

/// Default timeout for example outbound reads.
pub const DEFAULT_RECV_TIMEOUT: Duration = Duration::from_millis(500);
