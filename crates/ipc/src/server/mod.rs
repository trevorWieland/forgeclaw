//! Host-side Unix socket server and connection types.
//!
//! The [`IpcServer`] owns the bind lifecycle: it creates the socket
//! file at a caller-supplied path, unlinks any stale *socket* at that
//! path first, and cleans up on drop. Each accepted peer becomes a
//! [`PendingConnection`], and a successful handshake promotes it to
//! an [`IpcConnection`] with the full post-handshake API.
//!
//! The handshake lifecycle (`Ready → Init`) is encapsulated in
//! [`PendingConnection::handshake`] so higher-level crates never
//! reach into raw send/recv primitives just to establish a session.

mod auth;
mod connection_split;
mod listener;
mod pending;
mod protocol;
mod transport_core;

pub use crate::policy::UnknownTrafficLimitConfig;
pub use connection_split::{IpcConnectionReader, IpcConnectionWriter};
pub use listener::{
    IpcServer, IpcServerOptions, PeerCredentialPolicy, PeerCredentialPolicyError,
    UnauthorizedCommandLimitConfig,
};
pub use pending::PendingConnection;

use std::sync::Arc;
use std::time::Duration;

use forgeclaw_core::JobId;
use tokio::time::Instant;

use crate::codec::encode_host_to_container_frame;
use crate::error::{IpcError, ProtocolError};
use crate::message::authorized::AuthorizedCommand;
use crate::message::command::ClassifiedCommand;
use crate::message::{ContainerToHost, HostToContainer};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownTrafficBudget;
use crate::recv_policy::UnknownTypePolicy;
use crate::semantics::validate_classified_command;
use crate::transport::{FrameReader, FrameWriter};
use crate::util::{SharedWriteHalf, ShutdownHandle};
use crate::version::NegotiatedProtocolVersion;

use self::protocol::{
    ConnectionState, UnauthorizedEnforcementAction, log_fatal_protocol_error,
    log_outbound_validation_rejection, log_unauthorized_command, log_unknown_message,
    record_unauthorized_rejection, recv_deadline, recv_timeout_error,
};
use self::transport_core::{decode_and_enforce_inbound, preflight_and_enforce_outbound};

#[derive(Debug, Clone, Copy)]
enum CommandReceiveBehavior {
    NotCommand,
    UnexpectedMessage,
}

fn classify_command_message(
    msg: ContainerToHost,
    behavior: CommandReceiveBehavior,
) -> Result<ClassifiedCommand, IpcError> {
    match msg {
        ContainerToHost::Command(cmd) => Ok(cmd.body.classify()),
        other => {
            let got = other.type_name();
            match behavior {
                CommandReceiveBehavior::NotCommand => {
                    Err(IpcError::Protocol(ProtocolError::NotCommand { got }))
                }
                CommandReceiveBehavior::UnexpectedMessage => {
                    Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                        expected: "command",
                        got,
                    }))
                }
            }
        }
    }
}

async fn authorize_classified_command(
    classified: ClassifiedCommand,
    identity: &Arc<SessionIdentity>,
    state: &mut ConnectionState,
) -> Result<AuthorizedCommand, IpcError> {
    validate_classified_command(state.semantics(), &classified)?;
    let result = auth::authorize_command(classified, identity.as_ref());
    if let Err(IpcError::Protocol(ProtocolError::Unauthorized { command, reason })) = &result {
        let phase = state.phase_name();
        let outcome = record_unauthorized_rejection(state);
        log_unauthorized_command(identity, phase, command, reason, outcome.log);
        match outcome.action {
            UnauthorizedEnforcementAction::Allow => {}
            UnauthorizedEnforcementAction::Backoff => {
                if let Some(delay) = outcome.backoff {
                    tokio::time::sleep(delay).await;
                }
            }
            UnauthorizedEnforcementAction::Disconnect => {
                return Err(IpcError::Protocol(
                    ProtocolError::UnauthorizedCommandAbuse {
                        command,
                        reason,
                        strikes: outcome.log.strikes,
                        disconnect_after_strikes: outcome.log.disconnect_after_strikes,
                    },
                ));
            }
        }
    }
    result
}

async fn classify_inbound_event(
    msg: ContainerToHost,
    identity: &Arc<SessionIdentity>,
    state: &mut ConnectionState,
) -> Result<IpcInboundEvent, IpcError> {
    match msg {
        ContainerToHost::Command(cmd) => authorized_result_to_event(
            authorize_classified_command(cmd.body.classify(), identity, state).await,
        ),
        other => Ok(IpcInboundEvent::Message(other)),
    }
}

pub(super) fn authorized_result_to_event(
    result: Result<AuthorizedCommand, IpcError>,
) -> Result<IpcInboundEvent, IpcError> {
    match result {
        Ok(command) => Ok(IpcInboundEvent::Command(command)),
        Err(IpcError::Protocol(ProtocolError::Unauthorized { command, reason })) => Ok(
            IpcInboundEvent::Unauthorized(UnauthorizedCommandRejection { command, reason }),
        ),
        Err(err) => Err(err),
    }
}

pub(super) fn handle_unknown_inbound(
    identity: &Arc<SessionIdentity>,
    unknown_budget: &mut UnknownTrafficBudget,
    last_frame_len: usize,
    ty: &str,
) -> Result<(), IpcError> {
    unknown_budget.on_unknown(last_frame_len, Instant::now())?;
    log_unknown_message(identity, ty, unknown_budget);
    Ok(())
}

/// Non-fatal unauthorized command rejection surfaced via
/// [`IpcInboundEvent::Unauthorized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnauthorizedCommandRejection {
    /// The rejected wire command name.
    pub command: &'static str,
    /// The authorization rejection reason.
    pub reason: &'static str,
}

/// One inbound item from the container stream.
///
/// Use [`IpcConnection::recv_event`] and
/// [`IpcConnectionReader::recv_event`] when callers need to process
/// both command and non-command traffic without tearing down healthy
/// sessions on legal interleaving or single unauthorized commands.
#[derive(Debug)]
pub enum IpcInboundEvent {
    /// A command that passed IPC-layer authorization.
    Command(AuthorizedCommand),
    /// A command that was rejected by IPC authorization while keeping
    /// the connection usable.
    Unauthorized(UnauthorizedCommandRejection),
    /// A non-command container message (`output_delta`, `heartbeat`,
    /// etc.).
    Message(ContainerToHost),
}

#[derive(Debug)]
pub(crate) struct ConnectionTransport {
    pub(crate) reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
    pub(crate) writer: FrameWriter<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>>,
    pub(crate) shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

/// An established host-side connection to a single container.
///
/// Created by [`PendingConnection::handshake`] after a successful
/// handshake. Provides typed send/receive,
/// [`into_split`](IpcConnection::into_split) for full-duplex, and
/// command-oriented helpers
/// ([`recv_command`](IpcConnection::recv_command),
/// [`recv_event`](IpcConnection::recv_event)). Fatal errors poison the
/// connection and shut down the socket.
#[derive(Debug)]
pub struct IpcConnection {
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
    writer: FrameWriter<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
    poisoned: bool,
    unknown_budget: UnknownTrafficBudget,
    last_frame_len: usize,
    state: ConnectionState,
    identity: Arc<SessionIdentity>,
    write_timeout: Duration,
}

impl IpcConnection {
    /// Construct from an already-handshaked transport.
    pub(crate) fn from_parts(
        transport: ConnectionTransport,
        identity: Arc<SessionIdentity>,
        active_job_id: JobId,
        negotiated_version: NegotiatedProtocolVersion,
        unauthorized_limit: UnauthorizedCommandLimitConfig,
        unknown_traffic_limit: UnknownTrafficLimitConfig,
        write_timeout: Duration,
    ) -> Self {
        Self {
            reader: transport.reader,
            writer: transport.writer,
            shutdown_handle: transport.shutdown_handle,
            poisoned: false,
            unknown_budget: UnknownTrafficBudget::new(Instant::now(), unknown_traffic_limit),
            last_frame_len: 0,
            state: ConnectionState::new(
                Instant::now(),
                active_job_id,
                negotiated_version,
                unauthorized_limit,
            ),
            identity,
            write_timeout,
        }
    }

    /// Returns the session identity (shared with split halves).
    #[must_use]
    pub fn identity(&self) -> &Arc<SessionIdentity> {
        &self.identity
    }

    /// Negotiated protocol version for this established connection.
    #[must_use]
    pub fn negotiated_protocol_version(&self) -> NegotiatedProtocolVersion {
        self.state.negotiated_version()
    }

    async fn finalize_authorized_result(
        &mut self,
        result: Result<AuthorizedCommand, IpcError>,
    ) -> Result<AuthorizedCommand, IpcError> {
        if let Err(ref e) = result {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), e);
                self.poison().await;
            }
        }
        result
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

    /// Send a [`HostToContainer`] message to the peer.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let phase_name_before = self.state.phase_name();
        if let Err(e) = preflight_and_enforce_outbound(&mut self.state, msg, Instant::now()) {
            if matches!(
                e,
                IpcError::Protocol(ProtocolError::OutboundValidation { .. })
            ) {
                log_outbound_validation_rejection(&self.identity, phase_name_before, &e);
            } else {
                log_fatal_protocol_error(&self.identity, phase_name_before, &e);
                self.poison().await;
            }
            return Err(e);
        }
        let result = match tokio::time::timeout(
            self.write_timeout,
            self.writer
                .send_with(|buf| encode_host_to_container_frame(msg, buf)),
        )
        .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(IpcError::Timeout(self.write_timeout)),
        };
        if let Err(ref e) = result {
            if matches!(
                e,
                IpcError::Protocol(ProtocolError::OutboundValidation { .. })
            ) {
                log_outbound_validation_rejection(&self.identity, phase_name_before, e);
            }
        }
        if let Err(ref e) = result {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), e);
                self.poison().await;
            }
        }
        result
    }

    async fn recv_container_unchecked_one(&mut self) -> Result<ContainerToHost, IpcError> {
        self.check_poisoned()?;
        let frame_result = if let Some(deadline) = recv_deadline(&self.state) {
            match tokio::time::timeout_at(deadline, self.reader.recv_frame()).await {
                Ok(result) => result,
                Err(_elapsed) => return Err(recv_timeout_error(&self.state)),
            }
        } else {
            self.reader.recv_frame().await
        };
        let frame = match frame_result {
            Ok(frame) => frame,
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let (msg, lifecycle_action) =
            match decode_and_enforce_inbound(&mut self.state, &frame, Instant::now()) {
                Ok(result) => result,
                Err(e) => {
                    if e.is_fatal() {
                        log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                        self.poison().await;
                    }
                    return Err(e);
                }
            };
        if lifecycle_action.should_close_after_frame() {
            self.poison().await;
        }
        Ok(msg)
    }

    /// Receive a [`ContainerToHost`] message with explicit unknown-type
    /// handling policy, without IPC-layer authorization.
    ///
    /// This API is intentionally marked `unchecked`: if the returned
    /// message is `command`, the command has not been authorized yet.
    pub(crate) async fn recv_container_unchecked_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<ContainerToHost, IpcError> {
        match policy {
            UnknownTypePolicy::Strict => self.recv_container_unchecked_one().await,
            UnknownTypePolicy::SkipBounded => loop {
                match self.recv_container_unchecked_one().await {
                    Ok(msg) => {
                        self.unknown_budget.reset_consecutive();
                        return Ok(msg);
                    }
                    Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                        if let Err(e) = handle_unknown_inbound(
                            &self.identity,
                            &mut self.unknown_budget,
                            self.last_frame_len,
                            &ty,
                        ) {
                            log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                            self.poison().await;
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            },
        }
    }

    /// Receive a [`ContainerToHost`] message.
    ///
    /// This is the `unchecked` default: unknown message types are
    /// silently skipped (with a warning log) up to protocol limits:
    /// consecutive count/bytes plus independent lifetime/rate guards.
    ///
    /// Command authorization is not applied. Prefer
    /// [`recv_event`](Self::recv_event) or
    /// [`recv_command`](Self::recv_command) for the safe default.
    pub(crate) async fn recv_container_unchecked(&mut self) -> Result<ContainerToHost, IpcError> {
        self.recv_container_unchecked_with_policy(UnknownTypePolicy::SkipBounded)
            .await
    }

    /// Receive one inbound event with command authorization applied
    /// when relevant.
    pub async fn recv_event_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<IpcInboundEvent, IpcError> {
        let msg = self.recv_container_unchecked_with_policy(policy).await?;
        let result = classify_inbound_event(msg, &self.identity, &mut self.state).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), e);
                self.poison().await;
            }
        }
        result
    }

    /// Receive one inbound event with command authorization applied
    /// when relevant.
    pub async fn recv_event(&mut self) -> Result<IpcInboundEvent, IpcError> {
        self.recv_event_with_policy(UnknownTypePolicy::SkipBounded)
            .await
    }

    /// Receive the next command with the same authorization semantics
    /// as [`IpcConnectionReader::recv_command`].
    ///
    /// Returns [`ProtocolError::NotCommand`] when the next inbound
    /// frame is a valid non-command message.
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self
            .recv_command_unchecked(CommandReceiveBehavior::NotCommand)
            .await?;
        let result =
            authorize_classified_command(classified, &self.identity, &mut self.state).await;
        self.finalize_authorized_result(result).await
    }

    /// Receive the next command and fail strictly on non-command
    /// frames.
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if the next frame
    /// is not a `command`.
    pub async fn recv_command_strict(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self
            .recv_command_unchecked(CommandReceiveBehavior::UnexpectedMessage)
            .await?;
        let result =
            authorize_classified_command(classified, &self.identity, &mut self.state).await;
        self.finalize_authorized_result(result).await
    }

    /// Receive the next command without authorization checks.
    async fn recv_command_unchecked(
        &mut self,
        behavior: CommandReceiveBehavior,
    ) -> Result<ClassifiedCommand, IpcError> {
        let msg = self.recv_container_unchecked().await?;
        classify_command_message(msg, behavior)
    }

    /// Cleanly close the connection.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.writer.shutdown().await?;
        Ok(())
    }
}
