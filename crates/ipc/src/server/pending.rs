//! Pre-handshake server connection type.
//!
//! [`PendingConnection`] is returned by [`super::IpcServer::accept`]
//! and only exposes the handshake. A successful handshake consumes
//! the pending connection and returns an [`super::IpcConnection`]
//! with the full post-handshake API.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::Instant;

use crate::codec::{decode_container_to_host_with_tally, encode_host_to_container_frame};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, GroupInfo, HostToContainer, InitPayload, ReadyPayload};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownTrafficBudget;
use crate::semantics::ProtocolSemantics;
use crate::transport::{FrameReader, FrameWriter};
use crate::util::truncate_for_log;
use crate::util::{SharedWriteHalf, ShutdownHandle};
use crate::version::{NegotiatedProtocolVersion, PROTOCOL_VERSION, negotiate};

use super::UnknownTrafficLimitConfig;
use super::listener::UnauthorizedCommandLimitConfig;
use super::protocol::{
    log_fatal_protocol_error, log_outbound_validation_rejection, log_unknown_message,
};
use super::{ConnectionRuntimeOptions, ConnectionTransport, IpcConnection};

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
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
    writer: FrameWriter<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
    poisoned: bool,
    unknown_budget: UnknownTrafficBudget,
    unknown_log_sampler: crate::util::sampler::SampledCounter,
    last_frame_len: usize,
    identity: Arc<SessionIdentity>,
    unauthorized_limit: UnauthorizedCommandLimitConfig,
    unknown_traffic_limit: UnknownTrafficLimitConfig,
    write_timeout: Duration,
    idle_read_timeout: Option<Duration>,
}

impl PendingConnection {
    pub(crate) fn from_stream(
        stream: UnixStream,
        identity: Arc<SessionIdentity>,
        unauthorized_limit: UnauthorizedCommandLimitConfig,
        unknown_traffic_limit: UnknownTrafficLimitConfig,
        write_timeout: Duration,
        idle_read_timeout: Option<Duration>,
    ) -> Self {
        let (read_half, write_half) = stream.into_split();
        let (shared_write, shutdown_handle) = SharedWriteHalf::new(write_half);
        Self {
            reader: FrameReader::new(read_half),
            writer: FrameWriter::new(shared_write),
            shutdown_handle,
            poisoned: false,
            unknown_budget: UnknownTrafficBudget::new(Instant::now(), unknown_traffic_limit),
            unknown_log_sampler: crate::util::sampler::SampledCounter::new(
                crate::policy::UNKNOWN_LOG_BURST,
                crate::policy::UNKNOWN_LOG_EVERY,
            ),
            last_frame_len: 0,
            identity,
            unauthorized_limit,
            unknown_traffic_limit,
            write_timeout,
            idle_read_timeout,
        }
    }

    /// Returns the session identity.
    #[must_use]
    pub fn identity(&self) -> &Arc<SessionIdentity> {
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
        let active_job_id = init.job_id.clone();
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
            Ok((ready, negotiated_version)) => {
                tracing::debug!(
                    target: "forgeclaw_ipc::server",
                    adapter = %truncate_for_log(ready.adapter.as_ref()),
                    adapter_version = %truncate_for_log(ready.adapter_version.as_ref()),
                    protocol_version = %truncate_for_log(ready.protocol_version.as_ref()),
                    "IPC handshake complete"
                );
                let conn = IpcConnection::from_parts(
                    ConnectionTransport {
                        reader: self.reader,
                        writer: self.writer,
                        shutdown_handle: self.shutdown_handle,
                    },
                    self.identity,
                    active_job_id,
                    negotiated_version,
                    ConnectionRuntimeOptions {
                        unauthorized_limit: self.unauthorized_limit,
                        unknown_traffic_limit: self.unknown_traffic_limit,
                        write_timeout: self.write_timeout,
                        idle_read_timeout: self.idle_read_timeout,
                    },
                );
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
        let session_group = self.session_group();
        if init.context.group.id != session_group.id {
            return Err(IpcError::Protocol(ProtocolError::GroupMismatch {
                init_group_id: init.context.group.id,
                session_group_id: session_group.id,
            }));
        }
        init.context.group = session_group;
        Ok(init)
    }

    fn session_group(&self) -> GroupInfo {
        self.identity.group().clone()
    }

    /// Validate the handshake protocol under a shared deadline:
    /// receive `Ready`, check version, and send `Init`.
    async fn handshake_inner(
        &mut self,
        init: InitPayload,
        deadline: Instant,
        timeout: Duration,
    ) -> Result<(ReadyPayload, NegotiatedProtocolVersion), IpcError> {
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
        let negotiated = negotiate(ready.protocol_version.as_ref()).ok_or_else(|| {
            IpcError::Protocol(ProtocolError::UnsupportedVersion {
                peer: ready.protocol_version.to_string(),
                local: PROTOCOL_VERSION,
            })
        })?;
        match tokio::time::timeout_at(deadline, self.send_host(&HostToContainer::Init(init))).await
        {
            Ok(result) => result?,
            Err(_elapsed) => return Err(IpcError::Timeout(timeout)),
        }
        Ok((ready, negotiated))
    }

    /// Cleanly close the connection without completing the handshake.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.writer.shutdown().await?;
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
        self.shutdown_handle.trigger().await;
    }

    async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let result = self
            .writer
            .send_with(|buf| encode_host_to_container_frame(msg, buf))
            .await;
        if let Err(ref e) = result {
            if matches!(
                e,
                IpcError::Protocol(ProtocolError::OutboundValidation { .. })
            ) {
                log_outbound_validation_rejection(&self.identity, "handshake", e);
            }
        }
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv_container_strict(&mut self) -> Result<ContainerToHost, IpcError> {
        self.check_poisoned()?;
        let frame = match self.reader.recv_frame().await {
            Ok(frame) => frame,
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, "handshake", &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_container_to_host_with_tally(&frame);
        let (msg, tally) = match result {
            Ok((msg, tally)) => (msg, tally),
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, "handshake", &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        // Pre-handshake: no negotiated version yet, so apply baseline
        // (V1_0) semantics for the ignored-field budget.
        if let Err(e) =
            self.unknown_budget
                .on_ignored_container(&msg, tally, ProtocolSemantics::V1_0)
        {
            log_fatal_protocol_error(&self.identity, "handshake", &e);
            self.poison().await;
            return Err(e);
        }
        Ok(msg)
    }

    async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset_consecutive();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(e) = self
                        .unknown_budget
                        .on_unknown(self.last_frame_len, Instant::now())
                    {
                        log_fatal_protocol_error(&self.identity, "handshake", &e);
                        self.poison().await;
                        return Err(e);
                    }
                    let decision = self.unknown_log_sampler.observe();
                    log_unknown_message(&self.identity, &ty, &self.unknown_budget, decision);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
