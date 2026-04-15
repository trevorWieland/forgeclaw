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
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use forgeclaw_core::JobId;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::Instant;

use crate::error::{IpcError, ProtocolError};
use crate::message::authorized::AuthorizedCommand;
use crate::message::command::ClassifiedCommand;
use crate::message::{ContainerToHost, HostToContainer};
use crate::peer_cred::SessionIdentity;
use crate::policy::UnknownTrafficBudget;
use crate::recv_policy::UnknownTypePolicy;
use crate::transport::{FrameReader, FrameWriter};
use crate::util::{SharedWriteHalf, ShutdownHandle};
use crate::version::NegotiatedProtocolVersion;

use self::protocol::{ConnectionState, log_unknown_message};

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

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnectionRuntimeOptions {
    pub(crate) unauthorized_limit: UnauthorizedCommandLimitConfig,
    pub(crate) unknown_traffic_limit: UnknownTrafficLimitConfig,
    pub(crate) write_timeout: Duration,
    pub(crate) idle_read_timeout: Option<Duration>,
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
    writer: IpcConnectionWriter,
    reader: IpcConnectionReader,
    negotiated_version: NegotiatedProtocolVersion,
}

impl IpcConnection {
    /// Construct from an already-handshaked transport.
    pub(crate) fn from_parts(
        transport: ConnectionTransport,
        identity: Arc<SessionIdentity>,
        active_job_id: JobId,
        negotiated_version: NegotiatedProtocolVersion,
        options: ConnectionRuntimeOptions,
    ) -> Self {
        let poisoned = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AsyncMutex::new(ConnectionState::new(
            Instant::now(),
            active_job_id,
            negotiated_version,
            options.unauthorized_limit,
            options.idle_read_timeout,
        )));
        let shutdown_handle = transport.shutdown_handle;
        let writer = IpcConnectionWriter {
            writer: transport.writer,
            poisoned: Arc::clone(&poisoned),
            identity: Arc::clone(&identity),
            state: Arc::clone(&state),
            negotiated_version,
            write_timeout: options.write_timeout,
            shutdown_handle: shutdown_handle.clone(),
        };
        let reader = IpcConnectionReader {
            reader: transport.reader,
            poisoned,
            unknown_budget: UnknownTrafficBudget::new(
                Instant::now(),
                options.unknown_traffic_limit,
            ),
            last_frame_len: 0,
            state,
            identity,
            negotiated_version,
            shutdown_handle,
        };
        Self {
            writer,
            reader,
            negotiated_version,
        }
    }

    /// Returns the session identity (shared with split halves).
    #[must_use]
    pub fn identity(&self) -> &Arc<SessionIdentity> {
        self.writer.identity()
    }

    /// Negotiated protocol version for this established connection.
    #[must_use]
    pub fn negotiated_protocol_version(&self) -> NegotiatedProtocolVersion {
        self.negotiated_version
    }

    /// Send a [`HostToContainer`] message to the peer.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
        self.writer.send_host(msg).await
    }

    /// Receive one inbound event with command authorization applied
    /// when relevant.
    pub async fn recv_event_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<IpcInboundEvent, IpcError> {
        self.reader.recv_event_with_policy(policy).await
    }

    /// Receive one inbound event with command authorization applied
    /// when relevant.
    pub async fn recv_event(&mut self) -> Result<IpcInboundEvent, IpcError> {
        self.reader.recv_event().await
    }

    /// Receive the next command with the same authorization semantics
    /// as [`IpcConnectionReader::recv_command`].
    ///
    /// Returns [`ProtocolError::NotCommand`] when the next inbound
    /// frame is a valid non-command message.
    pub async fn recv_command(&mut self) -> Result<AuthorizedCommand, IpcError> {
        self.reader.recv_command().await
    }

    /// Receive the next command and fail strictly on non-command
    /// frames.
    ///
    /// Returns [`ProtocolError::UnexpectedMessage`] if the next frame
    /// is not a `command`.
    pub async fn recv_command_strict(&mut self) -> Result<AuthorizedCommand, IpcError> {
        self.reader.recv_command_strict().await
    }

    /// Split into independent read/write halves for full-duplex.
    ///
    /// Preserves buffered bytes and the session identity. A fatal
    /// error on either half poisons both.
    pub fn into_split(self) -> (IpcConnectionWriter, IpcConnectionReader) {
        (self.writer, self.reader)
    }

    /// Cleanly close the connection.
    pub async fn close(self) -> Result<(), IpcError> {
        self.writer.close().await
    }
}
