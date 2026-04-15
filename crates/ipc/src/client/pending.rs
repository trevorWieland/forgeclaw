//! Pre-handshake client connection type.
//!
//! [`PendingClient`] is returned by [`super::IpcClient::connect`]
//! and only exposes the handshake. A successful handshake consumes
//! the pending client and returns an [`super::IpcClient`] with the
//! full post-handshake API.

use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use crate::codec::{
    DEFAULT_MAX_UNKNOWN_SKIPS, FrameCodec, decode_host_to_container, encode_container_to_host,
};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer, InitPayload, ReadyPayload};
use crate::policy::{DEFAULT_MAX_UNKNOWN_BYTES, UnknownFrameBudget};
use crate::util::truncate_for_log;

use super::IpcClient;

/// A container-side client that has not yet completed the handshake.
///
/// Only [`handshake`](Self::handshake) and [`close`](Self::close)
/// are available. A successful handshake consumes `self` and returns
/// an [`IpcClient`] with the full post-handshake API.
#[derive(Debug)]
pub struct PendingClient {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
}

impl PendingClient {
    pub(crate) fn from_stream(stream: UnixStream) -> Self {
        Self {
            framed: Framed::new(stream, FrameCodec::new()),
            poisoned: false,
            unknown_budget: UnknownFrameBudget::default(),
            last_frame_len: 0,
        }
    }

    /// Perform the container-side handshake with a deadline.
    ///
    /// Consumes `self` and returns a fully established [`IpcClient`]
    /// paired with the host's [`InitPayload`].
    ///
    /// On any failure (including timeout) the underlying socket is
    /// shut down.
    pub async fn handshake(
        mut self,
        ready: ReadyPayload,
        timeout: Duration,
    ) -> Result<(IpcClient, InitPayload), IpcError> {
        match tokio::time::timeout(timeout, self.handshake_inner(ready)).await {
            Ok(Ok(init)) => {
                let client = IpcClient::from_parts(self.framed, init.job_id.clone());
                Ok((client, init))
            }
            Ok(Err(e)) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                Err(e)
            }
            Err(_elapsed) => {
                self.poison().await;
                Err(IpcError::Timeout(timeout))
            }
        }
    }

    async fn handshake_inner(&mut self, ready: ReadyPayload) -> Result<InitPayload, IpcError> {
        self.send(&ContainerToHost::Ready(ready)).await?;
        let first = self.recv().await?;
        match first {
            HostToContainer::Init(payload) => Ok(payload),
            other => {
                let err = IpcError::Protocol(ProtocolError::UnexpectedMessage {
                    expected: "init",
                    got: other.type_name(),
                });
                self.poison().await;
                Err(err)
            }
        }
    }

    /// Cleanly close the connection without completing the handshake.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.framed.close().await?;
        Ok(())
    }

    fn check_poisoned(&self) -> Result<(), IpcError> {
        if self.poisoned {
            return Err(IpcError::Closed);
        }
        Ok(())
    }

    async fn poison(&mut self) {
        self.poisoned = true;
        let _ = self.framed.get_mut().shutdown().await;
    }

    async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let bytes: Bytes = match encode_container_to_host(msg) {
            Ok(bytes) => bytes,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        let result = self.framed.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv_strict(&mut self) -> Result<HostToContainer, IpcError> {
        self.check_poisoned()?;
        let frame = match self.framed.next().await.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poisoned = true;
                return Err(IpcError::Closed);
            }
            Err(e) => {
                self.poison().await;
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_host_to_container(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(err) = self.unknown_budget.on_unknown(self.last_frame_len) {
                        self.poison().await;
                        return Err(err);
                    }
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %truncate_for_log(&ty),
                        skip_count = self.unknown_budget.count(),
                        skip_count_limit = DEFAULT_MAX_UNKNOWN_SKIPS,
                        skip_bytes = self.unknown_budget.bytes(),
                        skip_bytes_limit = DEFAULT_MAX_UNKNOWN_BYTES,
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }
}
