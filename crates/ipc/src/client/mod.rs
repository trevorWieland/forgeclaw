//! Container-side Unix socket client.
//!
//! [`IpcClient::connect`] returns a [`PendingClient`]; a successful
//! handshake promotes it to an [`IpcClient`] with the full
//! post-handshake API.
//!
//! On any fatal error the client is automatically poisoned: the
//! underlying socket is shut down and all subsequent calls return
//! [`IpcError::Closed`].

mod pending;
mod protocol;

pub use pending::PendingClient;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use forgeclaw_core::JobId;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::codec::{
    DEFAULT_MAX_UNKNOWN_SKIPS, FrameCodec, decode_host_to_container, encode_container_to_host,
};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};
use crate::policy::{DEFAULT_MAX_UNKNOWN_BYTES, UnknownFrameBudget};
use crate::recv_policy::UnknownTypePolicy;
use crate::util::{SharedWriteHalf, ShutdownHandle, truncate_for_log};

use self::protocol::{ClientConnectionState, enforce_inbound_state, enforce_outbound_state};

/// An established container-side IPC client.
///
/// Created by [`PendingClient::handshake`] after a successful
/// handshake. On any fatal error the underlying socket is shut down
/// and the client is poisoned — all subsequent calls return
/// [`IpcError::Closed`].
#[derive(Debug)]
pub struct IpcClient {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
    state: Arc<std::sync::Mutex<ClientConnectionState>>,
}

impl IpcClient {
    /// Connect to an IPC server previously bound at `path`.
    ///
    /// Returns a [`PendingClient`] that must complete the handshake
    /// before send/recv operations become available.
    pub async fn connect(path: impl AsRef<Path>) -> Result<PendingClient, IpcError> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        Ok(PendingClient::from_stream(stream))
    }

    /// Construct from an already-handshaked transport.
    pub(crate) fn from_parts(framed: Framed<UnixStream, FrameCodec>, active_job_id: JobId) -> Self {
        Self {
            framed,
            poisoned: false,
            unknown_budget: UnknownFrameBudget::default(),
            last_frame_len: 0,
            state: Arc::new(std::sync::Mutex::new(ClientConnectionState::new(
                active_job_id,
            ))),
        }
    }

    fn check_poisoned(&self) -> Result<(), IpcError> {
        if self.poisoned {
            return Err(IpcError::Closed);
        }
        Ok(())
    }

    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut ClientConnectionState) -> Result<T, IpcError>,
    ) -> Result<T, IpcError> {
        let mut state = self.state.lock().map_err(|_| IpcError::Closed)?;
        f(&mut state)
    }

    async fn poison(&mut self) {
        self.poisoned = true;
        let _ = self.framed.get_mut().shutdown().await;
    }

    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let close_after_send = match self.with_state_mut(|state| enforce_outbound_state(state, msg))
        {
            Ok(action) => action,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
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
        } else if close_after_send.should_close_after_frame() {
            self.poison().await;
        }
        result
    }

    async fn recv_one(&mut self) -> Result<HostToContainer, IpcError> {
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
        let msg = match decode_host_to_container(&frame) {
            Ok(msg) => msg,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        let close_after_recv = match self.with_state_mut(|state| enforce_inbound_state(state, &msg))
        {
            Ok(action) => action,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        if close_after_recv.should_close_after_frame() {
            self.poison().await;
        }
        Ok(msg)
    }

    /// Receive a [`HostToContainer`] message with explicit unknown-type
    /// handling policy.
    pub async fn recv_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<HostToContainer, IpcError> {
        match policy {
            UnknownTypePolicy::Strict => self.recv_one().await,
            UnknownTypePolicy::SkipBounded => loop {
                match self.recv_one().await {
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
            },
        }
    }

    /// Receive a [`HostToContainer`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to both:
    /// - 32 consecutive frames, and
    /// - 1 MiB cumulative bytes across the current unknown streak.
    ///
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        self.recv_with_policy(UnknownTypePolicy::SkipBounded).await
    }

    /// Split into independent read and write halves for full-duplex.
    ///
    /// Any bytes already buffered by the internal `Framed` reader
    /// are preserved in the returned reader half.
    pub fn into_split(self) -> (IpcClientWriter, IpcClientReader) {
        let poisoned = Arc::new(AtomicBool::new(self.poisoned));
        let parts = self.framed.into_parts();
        let (read_half, write_half) = parts.io.into_split();
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
            IpcClientWriter {
                writer,
                poisoned: Arc::clone(&poisoned),
                state: Arc::clone(&self.state),
                shutdown_handle: shutdown_handle.clone(),
            },
            IpcClientReader {
                reader,
                poisoned,
                shutdown_handle,
                unknown_budget: UnknownFrameBudget::default(),
                last_frame_len: 0,
                state: self.state,
            },
        )
    }

    /// Cleanly close the connection.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.framed.close().await?;
        Ok(())
    }
}

/// Write half of a split [`IpcClient`].
#[derive(Debug)]
pub struct IpcClientWriter {
    writer: FramedWrite<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>, FrameCodec>,
    poisoned: Arc<AtomicBool>,
    state: Arc<std::sync::Mutex<ClientConnectionState>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcClientWriter {
    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut ClientConnectionState) -> Result<T, IpcError>,
    ) -> Result<T, IpcError> {
        let mut state = self.state.lock().map_err(|_| IpcError::Closed)?;
        f(&mut state)
    }

    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let close_after_send = match self.with_state_mut(|state| enforce_outbound_state(state, msg))
        {
            Ok(action) => action,
            Err(e) => {
                if e.is_fatal() {
                    self.poisoned.store(true, Ordering::Release);
                    self.shutdown_handle.trigger().await;
                }
                return Err(e);
            }
        };
        let bytes: Bytes = match encode_container_to_host(msg) {
            Ok(bytes) => bytes,
            Err(e) => {
                if e.is_fatal() {
                    self.poisoned.store(true, Ordering::Release);
                    self.shutdown_handle.trigger().await;
                }
                return Err(e);
            }
        };
        let result = self.writer.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poisoned.store(true, Ordering::Release);
                self.shutdown_handle.trigger().await;
            }
        } else if close_after_send.should_close_after_frame() {
            self.poisoned.store(true, Ordering::Release);
            self.shutdown_handle.trigger().await;
        }
        result
    }
}

/// Read half of a split [`IpcClient`].
///
/// On fatal errors the reader immediately shuts down the shared write
/// half so the peer observes EOF without waiting for the writer task.
#[derive(Debug)]
pub struct IpcClientReader {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, FrameCodec>,
    poisoned: Arc<AtomicBool>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
    unknown_budget: UnknownFrameBudget,
    last_frame_len: usize,
    state: Arc<std::sync::Mutex<ClientConnectionState>>,
}

impl IpcClientReader {
    fn with_state_mut<T>(
        &self,
        f: impl FnOnce(&mut ClientConnectionState) -> Result<T, IpcError>,
    ) -> Result<T, IpcError> {
        let mut state = self.state.lock().map_err(|_| IpcError::Closed)?;
        f(&mut state)
    }

    async fn poison(&mut self) {
        self.poisoned.store(true, Ordering::Release);
        self.shutdown_handle.trigger().await;
    }

    async fn recv_one(&mut self) -> Result<HostToContainer, IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let frame = match self.reader.next().await.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poison().await;
                return Err(IpcError::Closed);
            }
            Err(e) => {
                self.poison().await;
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let msg = match decode_host_to_container(&frame) {
            Ok(msg) => msg,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        let close_after_recv = match self.with_state_mut(|state| enforce_inbound_state(state, &msg))
        {
            Ok(action) => action,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        if close_after_recv.should_close_after_frame() {
            self.poison().await;
        }
        Ok(msg)
    }

    /// Receive a [`HostToContainer`] message with explicit unknown-type
    /// handling policy.
    pub async fn recv_with_policy(
        &mut self,
        policy: UnknownTypePolicy,
    ) -> Result<HostToContainer, IpcError> {
        match policy {
            UnknownTypePolicy::Strict => self.recv_one().await,
            UnknownTypePolicy::SkipBounded => loop {
                match self.recv_one().await {
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
            },
        }
    }

    /// Receive a [`HostToContainer`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to both:
    /// - 32 consecutive frames, and
    /// - 1 MiB cumulative bytes across the current unknown streak.
    ///
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        self.recv_with_policy(UnknownTypePolicy::SkipBounded).await
    }
}
