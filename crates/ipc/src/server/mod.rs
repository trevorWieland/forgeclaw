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
mod listener;
mod pending;
mod protocol;

pub use listener::IpcServer;
pub use pending::PendingConnection;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::Instant;
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::codec::{FrameCodec, decode_container_to_host, encode_message};
use crate::error::{IpcError, ProtocolError};
use crate::message::authorized::AuthorizedCommand;
use crate::message::command::ClassifiedCommand;
use crate::message::{ContainerToHost, HostToContainer};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownFrameBudget;
use crate::util::{SharedWriteHalf, ShutdownHandle};

use self::protocol::{
    ConnectionState, enforce_inbound_state, enforce_outbound_state, heartbeat_timeout_error,
    log_fatal_protocol_error, log_unknown_message, recv_deadline,
};

/// An established host-side connection to a single container.
///
/// Created by [`PendingConnection::handshake`] after a successful
/// handshake. Provides typed send/receive,
/// [`into_split`](IpcConnection::into_split) for full-duplex, and
/// [`recv_command`](IpcConnection::recv_command) with the built-in
/// authorization matrix. Fatal errors poison the connection and shut
/// down the socket.
#[derive(Debug)]
pub struct IpcConnection {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
    state: ConnectionState,
    identity: Arc<std::sync::Mutex<SessionIdentity>>,
}

impl IpcConnection {
    /// Construct from an already-handshaked transport.
    pub(crate) fn from_parts(
        framed: Framed<UnixStream, FrameCodec>,
        identity: Arc<std::sync::Mutex<SessionIdentity>>,
    ) -> Self {
        Self {
            framed,
            poisoned: false,
            unknown_budget: UnknownFrameBudget::default(),
            last_frame_len: 0,
            state: ConnectionState::new(Instant::now()),
            identity,
        }
    }

    /// Returns the session identity (shared with split halves).
    #[must_use]
    pub fn identity(&self) -> &Arc<std::sync::Mutex<SessionIdentity>> {
        &self.identity
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

    /// Send a [`HostToContainer`] message to the peer.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let phase_name = self.state.phase_name();
        if let Err(e) = enforce_outbound_state(&mut self.state, msg, Instant::now()) {
            log_fatal_protocol_error(&self.identity, phase_name, &e);
            self.poison().await;
            return Err(e);
        }
        let bytes: Bytes = encode_message(msg)?;
        let result = self.framed.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), e);
                self.poison().await;
            }
        }
        result
    }

    /// Receive a single [`ContainerToHost`] frame without
    /// forward-compatibility skipping.
    ///
    /// Returns [`ProtocolError::UnknownMessageType`] for unrecognized
    /// `type` discriminators. Most callers should use
    /// [`recv_container`](Self::recv_container) instead, which silently
    /// skips unknown types per the spec.
    pub async fn recv_container_strict(&mut self) -> Result<ContainerToHost, IpcError> {
        self.check_poisoned()?;
        let next_frame = if let Some(deadline) = recv_deadline(&self.state) {
            match tokio::time::timeout_at(deadline, self.framed.next()).await {
                Ok(next) => next,
                Err(_elapsed) => return Err(heartbeat_timeout_error(&self.state)),
            }
        } else {
            self.framed.next().await
        };
        let frame = match next_frame.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poisoned = true;
                return Err(IpcError::Closed);
            }
            Err(e) => {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                self.poison().await;
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_container_to_host(&frame);
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                if e.is_fatal() {
                    log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        if let Err(e) = enforce_inbound_state(&mut self.state, &msg, Instant::now()) {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                self.poison().await;
            }
            return Err(e);
        }
        Ok(msg)
    }

    /// Receive a [`ContainerToHost`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to spec limits
    /// (count and cumulative-byte budgets).
    pub async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(e) = self.unknown_budget.on_unknown(self.last_frame_len) {
                        log_fatal_protocol_error(&self.identity, self.state.phase_name(), &e);
                        self.poison().await;
                        return Err(e);
                    }
                    log_unknown_message(&self.identity, &ty, &self.unknown_budget);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Split into independent read/write halves for full-duplex.
    ///
    /// Preserves buffered bytes and the session identity. A fatal
    /// error on either half poisons both.
    pub fn into_split(self) -> (IpcConnectionWriter, IpcConnectionReader) {
        let poisoned = Arc::new(AtomicBool::new(self.poisoned));
        let state = Arc::new(std::sync::Mutex::new(self.state));
        let parts = self.framed.into_parts();
        let (read_half, write_half) = parts.io.into_split();
        // Wrap write half so the reader can shut it down directly.
        let (shared_write, shutdown_handle) = SharedWriteHalf::new(write_half);
        let mut writer = FramedWrite::new(shared_write, FrameCodec::new());
        if !parts.write_buf.is_empty() {
            *writer.write_buffer_mut() = parts.write_buf;
        }
        let mut reader = FramedRead::new(read_half, FrameCodec::new());
        if !parts.read_buf.is_empty() {
            *reader.read_buffer_mut() = parts.read_buf;
        }
        (
            IpcConnectionWriter {
                writer,
                poisoned: Arc::clone(&poisoned),
                identity: Arc::clone(&self.identity),
                state: Arc::clone(&state),
                shutdown_handle: shutdown_handle.clone(),
            },
            IpcConnectionReader {
                reader,
                poisoned,
                unknown_budget: UnknownFrameBudget::default(),
                last_frame_len: 0,
                state,
                identity: self.identity,
                shutdown_handle,
            },
        )
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
    /// Returns [`ProtocolError::UnexpectedMessage`] for non-command
    /// messages.
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self.recv_command_unchecked().await?;
        auth::authorize_command(classified, &self.identity)
    }

    /// Receive the next command without authorization checks.
    pub(crate) async fn recv_command_unchecked(&mut self) -> Result<ClassifiedCommand, IpcError> {
        let msg = self.recv_container().await?;
        let type_name = msg.type_name();
        msg.classify_command()
            .ok_or(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "command",
                got: type_name,
            }))
    }

    /// Cleanly close the connection.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.framed.close().await?;
        Ok(())
    }
}

/// Write half of a split [`IpcConnection`].
#[derive(Debug)]
pub struct IpcConnectionWriter {
    writer: FramedWrite<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>, FrameCodec>,
    poisoned: Arc<AtomicBool>,
    identity: Arc<std::sync::Mutex<SessionIdentity>>,
    state: Arc<std::sync::Mutex<ConnectionState>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcConnectionWriter {
    /// Returns the session identity shared with the reader half.
    #[must_use]
    pub fn identity(&self) -> &Arc<std::sync::Mutex<SessionIdentity>> {
        &self.identity
    }

    fn enforce_phase_for_send(
        &self,
        msg: &HostToContainer,
    ) -> Result<&'static str, (&'static str, IpcError)> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ("unknown", IpcError::Closed))?;
        let phase_name = state.phase_name();
        if let Err(e) = enforce_outbound_state(&mut state, msg, Instant::now()) {
            return Err((phase_name, e));
        }
        Ok(phase_name)
    }

    /// Send a [`HostToContainer`] message.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let phase_name = match self.enforce_phase_for_send(msg) {
            Ok(phase_name) => phase_name,
            Err((phase_name, e)) => {
                self.poisoned.store(true, Ordering::Release);
                log_fatal_protocol_error(&self.identity, phase_name, &e);
                self.shutdown_handle.trigger().await;
                return Err(e);
            }
        };
        let bytes: Bytes = encode_message(msg)?;
        let result = self.writer.send(bytes).await;
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
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, FrameCodec>,
    poisoned: Arc<AtomicBool>,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
    state: Arc<std::sync::Mutex<ConnectionState>>,
    identity: Arc<std::sync::Mutex<SessionIdentity>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcConnectionReader {
    /// Returns the session identity shared with the writer half.
    #[must_use]
    pub fn identity(&self) -> &Arc<std::sync::Mutex<SessionIdentity>> {
        &self.identity
    }

    /// Poison the connection and shut down the write half immediately.
    async fn poison(&mut self) {
        self.poisoned.store(true, Ordering::Release);
        self.shutdown_handle.trigger().await;
    }

    /// Receive a single [`ContainerToHost`] frame without
    /// forward-compatibility skipping.
    ///
    /// Returns [`ProtocolError::UnknownMessageType`] for unrecognized
    /// `type` discriminators. Most callers should use
    /// [`recv_container`](Self::recv_container) instead, which silently
    /// skips unknown types per the spec.
    pub async fn recv_container_strict(&mut self) -> Result<ContainerToHost, IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let next_frame = if let Some(deadline) = self.current_recv_deadline()? {
            match tokio::time::timeout_at(deadline, self.reader.next()).await {
                Ok(next) => next,
                Err(_elapsed) => return self.heartbeat_timeout_error(),
            }
        } else {
            self.reader.next().await
        };
        let frame = match next_frame.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poison().await;
                return Err(IpcError::Closed);
            }
            Err(e) => {
                let phase_name = self.current_phase_name()?;
                log_fatal_protocol_error(&self.identity, phase_name, &e);
                self.poison().await;
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_container_to_host(&frame);
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                if e.is_fatal() {
                    let phase_name = self.current_phase_name()?;
                    log_fatal_protocol_error(&self.identity, phase_name, &e);
                    self.poison().await;
                }
                return Err(e);
            }
        };
        let phase_name = self.current_phase_name()?;
        if let Err(e) =
            self.with_state_mut(|state| enforce_inbound_state(state, &msg, Instant::now()))
        {
            if e.is_fatal() {
                log_fatal_protocol_error(&self.identity, phase_name, &e);
                self.poison().await;
            }
            return Err(e);
        }
        Ok(msg)
    }

    /// Receive a [`ContainerToHost`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to spec limits
    /// (count and cumulative-byte budgets).
    pub async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(e) = self.unknown_budget.on_unknown(self.last_frame_len) {
                        let phase_name = self.current_phase_name()?;
                        log_fatal_protocol_error(&self.identity, phase_name, &e);
                        self.poison().await;
                        return Err(e);
                    }
                    log_unknown_message(&self.identity, &ty, &self.unknown_budget);
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut ConnectionState) -> Result<T, IpcError>,
    ) -> Result<T, IpcError> {
        let mut state = self.state.lock().map_err(|_| IpcError::Closed)?;
        f(&mut state)
    }

    fn current_phase_name(&self) -> Result<&'static str, IpcError> {
        self.state
            .lock()
            .map(|state| state.phase_name())
            .map_err(|_| IpcError::Closed)
    }

    fn current_recv_deadline(&self) -> Result<Option<Instant>, IpcError> {
        self.state
            .lock()
            .map(|state| recv_deadline(&state))
            .map_err(|_| IpcError::Closed)
    }

    fn heartbeat_timeout_error(&self) -> Result<ContainerToHost, IpcError> {
        self.state
            .lock()
            .map(|state| Err(heartbeat_timeout_error(&state)))
            .map_err(|_| IpcError::Closed)?
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
    /// Returns [`ProtocolError::UnexpectedMessage`] for non-command
    /// messages.
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand, IpcError> {
        let classified = self.recv_command_unchecked().await?;
        auth::authorize_command(classified, &self.identity)
    }

    /// Receive the next command without authorization checks.
    pub(crate) async fn recv_command_unchecked(&mut self) -> Result<ClassifiedCommand, IpcError> {
        let msg = self.recv_container().await?;
        let type_name = msg.type_name();
        msg.classify_command()
            .ok_or(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "command",
                got: type_name,
            }))
    }
}
