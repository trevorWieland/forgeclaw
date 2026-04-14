//! Pre-handshake server connection type.
//!
//! [`PendingConnection`] is returned by [`super::IpcServer::accept`]
//! and only exposes the handshake. A successful handshake consumes
//! the pending connection and returns an [`super::IpcConnection`]
//! with the full post-handshake API.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::Instant;
use tokio_util::codec::Framed;

use crate::codec::{FrameCodec, decode_container_to_host, encode_message};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, GroupInfo, HostToContainer, InitPayload, ReadyPayload};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownFrameBudget;
use crate::util::truncate_for_log;
use crate::version::{PROTOCOL_VERSION, is_compatible};

use super::IpcConnection;
use super::protocol::{log_fatal_protocol_error, log_unknown_message};

/// A server-side connection that has not yet completed the handshake.
///
/// Only [`handshake`](Self::handshake) and [`close`](Self::close)
/// are available. A successful handshake consumes `self` and returns
/// an [`IpcConnection`] with the full post-handshake API.
///
/// This enforces the compile-time guarantee that `send_host`,
/// `recv_command`, and `into_split` cannot be called before the
/// protocol handshake completes.
#[derive(Debug)]
pub struct PendingConnection {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
    identity: Arc<std::sync::Mutex<SessionIdentity>>,
}

impl PendingConnection {
    pub(crate) fn from_stream(
        stream: UnixStream,
        identity: Arc<std::sync::Mutex<SessionIdentity>>,
    ) -> Self {
        Self {
            framed: Framed::new(stream, FrameCodec::new()),
            poisoned: false,
            unknown_budget: UnknownFrameBudget::default(),
            last_frame_len: 0,
            identity,
        }
    }

    /// Returns the session identity.
    #[must_use]
    pub fn identity(&self) -> &Arc<std::sync::Mutex<SessionIdentity>> {
        &self.identity
    }

    /// Perform the host-side handshake with a deadline.
    ///
    /// Consumes `self` and returns a fully established
    /// [`IpcConnection`] paired with the peer's [`ReadyPayload`].
    ///
    /// `init.context.group` is normalized to the accept-time session
    /// identity. A mismatched group ID is rejected as
    /// [`ProtocolError::GroupMismatch`].
    ///
    /// On any failure (including timeout) the underlying socket is
    /// shut down.
    pub async fn handshake(
        mut self,
        init: InitPayload,
        timeout: Duration,
    ) -> Result<(IpcConnection, ReadyPayload), IpcError> {
        let init = match self.bind_authoritative_group(init) {
            Ok(init) => init,
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, "handshake", &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };

        let deadline = Instant::now() + timeout;
        match self.handshake_inner(init, deadline, timeout).await {
            Ok(ready) => {
                tracing::debug!(
                    target: "forgeclaw_ipc::server",
                    adapter = %truncate_for_log(&ready.adapter),
                    adapter_version = %truncate_for_log(&ready.adapter_version),
                    protocol_version = %truncate_for_log(&ready.protocol_version),
                    "IPC handshake complete"
                );
                let conn = IpcConnection::from_parts(self.framed, self.identity);
                Ok((conn, ready))
            }
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, "handshake", &e);
                    self.poison().await;
                }
                Err(e)
            }
        }
    }

    fn bind_authoritative_group(&self, mut init: InitPayload) -> Result<InitPayload, IpcError> {
        let session_group = self.session_group()?;
        if init.context.group.id != session_group.id {
            return Err(IpcError::Protocol(ProtocolError::GroupMismatch {
                init_group_id: init.context.group.id,
                session_group_id: session_group.id,
            }));
        }
        init.context.group = session_group;
        Ok(init)
    }

    fn session_group(&self) -> Result<GroupInfo, IpcError> {
        self.identity
            .lock()
            .map_err(|_| IpcError::Closed)
            .map(|guard| guard.group().clone())
    }

    /// Validate the handshake protocol under a shared deadline:
    /// receive `Ready`, check version, and send `Init`.
    async fn handshake_inner(
        &mut self,
        init: InitPayload,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<ReadyPayload, IpcError> {
        let first = match tokio::time::timeout_at(deadline, self.recv_container()).await {
            Ok(result) => result?,
            Err(_elapsed) => return Err(IpcError::Timeout(timeout)),
        };
        let ready = match first {
            ContainerToHost::Ready(payload) => payload,
            other => {
                return Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                    expected: "ready",
                    got: other.type_name(),
                }));
            }
        };
        if !is_compatible(&ready.protocol_version) {
            return Err(IpcError::Protocol(ProtocolError::UnsupportedVersion {
                peer: ready.protocol_version.clone(),
                local: PROTOCOL_VERSION,
            }));
        }
        match tokio::time::timeout_at(deadline, self.send_host(&HostToContainer::Init(init))).await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(IpcError::Timeout(timeout)),
        }
        Ok(ready)
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

    async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let bytes: Bytes = encode_message(msg)?;
        let result = self.framed.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv_container_strict(&mut self) -> Result<ContainerToHost, IpcError> {
        self.check_poisoned()?;
        let frame = match self.framed.next().await.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poisoned = true;
                return Err(IpcError::Closed);
            }
            Err(e) => {
                log_fatal_protocol_error(&self.identity, "handshake", &e);
                self.poison().await;
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_container_to_host(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, "handshake", e);
                self.poison().await;
            }
        }
        result
    }

    async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(e) = self.unknown_budget.on_unknown(self.last_frame_len) {
                        log_fatal_protocol_error(&self.identity, "handshake", &e);
                        self.poison().await;
                        return Err(e);
                    }
                    log_unknown_message(&self.identity, &ty, &self.unknown_budget);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
