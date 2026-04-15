use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
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
use crate::util::ShutdownHandle;
use crate::version::NegotiatedProtocolVersion;

use super::protocol::{
    ConnectionState, UnauthorizedEnforcementAction, log_fatal_protocol_error,
    log_outbound_validation_rejection, log_unauthorized_command, record_unauthorized_rejection,
    recv_deadline, recv_timeout_error,
};
use super::transport_core::{decode_and_enforce_inbound, preflight_and_enforce_outbound};
use super::{
    CommandReceiveBehavior, IpcConnection, IpcInboundEvent, auth, authorized_result_to_event,
    classify_command_message, handle_unknown_inbound,
};

impl IpcConnection {
    /// Split into independent read/write halves for full-duplex.
    ///
    /// Preserves buffered bytes and the session identity. A fatal
    /// error on either half poisons both.
    pub fn into_split(self) -> (IpcConnectionWriter, IpcConnectionReader) {
        let poisoned = Arc::new(AtomicBool::new(self.poisoned));
        let negotiated_version = self.state.negotiated_version();
        let state = Arc::new(AsyncMutex::new(self.state));
        let writer = self.writer;
        let reader = self.reader;
        let shutdown_handle = self.shutdown_handle;
        (
            IpcConnectionWriter {
                writer,
                poisoned: Arc::clone(&poisoned),
                identity: Arc::clone(&self.identity),
                state: Arc::clone(&state),
                negotiated_version,
                write_timeout: self.write_timeout,
                shutdown_handle: shutdown_handle.clone(),
            },
            IpcConnectionReader {
                reader,
                poisoned,
                unknown_budget: self.unknown_budget,
                last_frame_len: 0,
                state,
                identity: self.identity,
                negotiated_version,
                shutdown_handle,
            },
        )
    }
}

/// Write half of a split [`IpcConnection`].
#[derive(Debug)]
pub struct IpcConnectionWriter {
    writer: FrameWriter<crate::util::SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>>,
    poisoned: Arc<AtomicBool>,
    identity: Arc<SessionIdentity>,
    state: Arc<AsyncMutex<ConnectionState>>,
    negotiated_version: NegotiatedProtocolVersion,
    write_timeout: Duration,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcConnectionWriter {
    /// Returns the session identity shared with the reader half.
    #[must_use]
    pub fn identity(&self) -> &Arc<SessionIdentity> {
        &self.identity
    }

    /// Negotiated protocol version for this connection.
    pub fn negotiated_protocol_version(&self) -> Result<NegotiatedProtocolVersion, IpcError> {
        Ok(self.negotiated_version)
    }

    /// Send a [`HostToContainer`] message.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let phase_name = {
            let state = self.state.lock().await;
            state.phase_name()
        };
        {
            let mut state = self.state.lock().await;
            if let Err(e) = preflight_and_enforce_outbound(&mut state, msg, Instant::now()) {
                if matches!(
                    e,
                    IpcError::Protocol(ProtocolError::OutboundValidation { .. })
                ) {
                    log_outbound_validation_rejection(&self.identity, phase_name, &e);
                } else {
                    self.poisoned.store(true, Ordering::Release);
                    log_fatal_protocol_error(&self.identity, phase_name, &e);
                    self.shutdown_handle.trigger().await;
                }
                return Err(e);
            }
        };
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
                log_outbound_validation_rejection(&self.identity, phase_name, e);
            }
        }
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poisoned.store(true, Ordering::Release);
                log_fatal_protocol_error(&self.identity, phase_name, e);
                self.shutdown_handle.trigger().await;
            }
        }
        result
    }
}

/// Read half of a split [`IpcConnection`].
#[derive(Debug)]
pub struct IpcConnectionReader {
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
    poisoned: Arc<AtomicBool>,
    unknown_budget: UnknownTrafficBudget,
    last_frame_len: usize,
    state: Arc<AsyncMutex<ConnectionState>>,
    identity: Arc<SessionIdentity>,
    negotiated_version: NegotiatedProtocolVersion,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcConnectionReader {
    /// Returns the session identity shared with the writer half.
    #[must_use]
    pub fn identity(&self) -> &Arc<SessionIdentity> {
        &self.identity
    }

    /// Negotiated protocol version for this connection.
    pub fn negotiated_protocol_version(&self) -> Result<NegotiatedProtocolVersion, IpcError> {
        Ok(self.negotiated_version)
    }

    /// Poison the connection and shut down the write half immediately.
    async fn poison(&mut self) {
        self.poisoned.store(true, Ordering::Release);
        self.shutdown_handle.trigger().await;
    }

    async fn recv_container_unchecked_one(&mut self) -> Result<ContainerToHost, IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let frame_result = if let Some(deadline) = self.current_recv_deadline().await {
            match tokio::time::timeout_at(deadline, self.reader.recv_frame()).await {
                Ok(result) => result,
                Err(_elapsed) => return self.recv_timeout_error().await,
            }
        } else {
            self.reader.recv_frame().await
        };
        let frame = match frame_result {
            Ok(frame) => frame,
            Err(e) => {
                let phase_name = self.current_phase_name().await;
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, phase_name, &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let phase_name = self.current_phase_name().await;
        let (msg, lifecycle_action) = match self
            .with_state_mut(|state| decode_and_enforce_inbound(state, &frame, Instant::now()))
            .await
        {
            Ok(result) => result,
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, phase_name, &e);
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
                            let phase_name = self.current_phase_name().await;
                            log_fatal_protocol_error(&self.identity, phase_name, &e);
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

    async fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut ConnectionState) -> Result<T, IpcError>,
    ) -> Result<T, IpcError> {
        let mut state = self.state.lock().await;
        f(&mut state)
    }

    async fn current_phase_name(&self) -> &'static str {
        let state = self.state.lock().await;
        state.phase_name()
    }

    async fn current_recv_deadline(&self) -> Option<Instant> {
        let state = self.state.lock().await;
        recv_deadline(&state)
    }

    async fn recv_timeout_error(&self) -> Result<ContainerToHost, IpcError> {
        let state = self.state.lock().await;
        Err(recv_timeout_error(&state))
    }

    async fn authorize_classified(
        &mut self,
        classified: ClassifiedCommand,
    ) -> Result<AuthorizedCommand, IpcError> {
        self.with_state_mut(|state| validate_classified_command(state.semantics(), &classified))
            .await?;
        let result = auth::authorize_command(classified, self.identity.as_ref());
        if let Err(IpcError::Protocol(ProtocolError::Unauthorized { command, reason })) = &result {
            let (phase, outcome) = self
                .with_state_mut(|state| {
                    let phase = state.phase_name();
                    let outcome = record_unauthorized_rejection(state);
                    Ok((phase, outcome))
                })
                .await?;
            log_unauthorized_command(&self.identity, phase, command, reason, outcome.log);
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

    async fn finalize_authorized_result(
        &mut self,
        result: Result<AuthorizedCommand, IpcError>,
    ) -> Result<AuthorizedCommand, IpcError> {
        if let Err(ref e) = result {
            if e.is_fatal() {
                let phase = self.current_phase_name().await;
                log_fatal_protocol_error(&self.identity, phase, e);
                self.poison().await;
            }
        }
        result
    }

    /// Receive the next command with the full authorization matrix.
    ///
    /// Enforces the IPC protocol's command authorization table:
    /// - `register_group`, `dispatch_self_improvement`: main only.
    /// - `send_message`, `schedule_task`: own-group for non-main.
    /// - `dispatch_tanren`: requires tanren capability.
    /// - `pause_task`, `cancel_task`: wrapped in [`crate::message::authorized::OwnershipPending`]
    ///   so the caller must verify task ownership.
    ///
    /// Returns [`ProtocolError::Unauthorized`] for rejected commands.
    /// Returns [`ProtocolError::NotCommand`] for non-command messages.
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self
            .recv_command_unchecked(CommandReceiveBehavior::NotCommand)
            .await?;
        let result = self.authorize_classified(classified).await;
        self.finalize_authorized_result(result).await
    }

    /// Receive one inbound event with command authorization applied
    /// when relevant.
    pub async fn recv_event_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<IpcInboundEvent, IpcError> {
        let msg = self.recv_container_unchecked_with_policy(policy).await?;
        let result = self.classify_inbound_event(msg).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                let phase = self.current_phase_name().await;
                log_fatal_protocol_error(&self.identity, phase, e);
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

    /// Receive the next command and fail strictly on non-command
    /// frames.
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if the next frame
    /// is not a `command`.
    pub async fn recv_command_strict(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self
            .recv_command_unchecked(CommandReceiveBehavior::UnexpectedMessage)
            .await?;
        let result = self.authorize_classified(classified).await;
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

    async fn classify_inbound_event(
        &mut self,
        msg: ContainerToHost,
    ) -> Result<IpcInboundEvent, IpcError> {
        match msg {
            ContainerToHost::Command(cmd) => {
                authorized_result_to_event(self.authorize_classified(cmd.body.classify()).await)
            }
            other => Ok(IpcInboundEvent::Message(other)),
        }
    }
}
