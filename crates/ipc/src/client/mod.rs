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

pub use pending::PendingClient;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::codec::{
    DEFAULT_MAX_UNKNOWN_SKIPS, FrameCodec, decode_host_to_container, encode_message,
};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};
use crate::util::{SharedWriteHalf, ShutdownHandle, truncate_for_log};

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
    unknown_skip_count: usize,
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
    pub(crate) fn from_parts(framed: Framed<UnixStream, FrameCodec>) -> Self {
        Self {
            framed,
            poisoned: false,
            unknown_skip_count: 0,
        }
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

    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
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

    /// Receive a single [`HostToContainer`] frame without
    /// forward-compatibility skipping.
    ///
    /// Returns [`ProtocolError::UnknownMessageType`] for unrecognized
    /// `type` discriminators. Most callers should use [`recv`](Self::recv)
    /// instead, which silently skips unknown types per the spec.
    pub async fn recv_strict(&mut self) -> Result<HostToContainer, IpcError> {
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
        let result = decode_host_to_container(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    /// Receive a [`HostToContainer`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to 32 consecutive
    /// frames. Use [`recv_strict`](Self::recv_strict) if you need raw
    /// decode errors for every frame.
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv_strict().await {
                Ok(msg) => {
                    self.unknown_skip_count = 0;
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    self.unknown_skip_count += 1;
                    if self.unknown_skip_count > DEFAULT_MAX_UNKNOWN_SKIPS {
                        let err = IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
                            count: self.unknown_skip_count,
                            limit: DEFAULT_MAX_UNKNOWN_SKIPS,
                        });
                        self.poison().await;
                        return Err(err);
                    }
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %truncate_for_log(&ty),
                        skip_count = self.unknown_skip_count,
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
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
            },
            IpcClientReader {
                reader,
                poisoned,
                shutdown_handle,
                unknown_skip_count: 0,
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
}

impl IpcClientWriter {
    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let bytes: Bytes = encode_message(msg)?;
        let result = self.writer.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poisoned.store(true, Ordering::Release);
            }
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
    unknown_skip_count: usize,
}

impl IpcClientReader {
    async fn poison(&mut self) {
        self.poisoned.store(true, Ordering::Release);
        self.shutdown_handle.trigger().await;
    }

    /// Receive a single [`HostToContainer`] frame without
    /// forward-compatibility skipping.
    ///
    /// Returns [`ProtocolError::UnknownMessageType`] for unrecognized
    /// `type` discriminators. Most callers should use [`recv`](Self::recv)
    /// instead, which silently skips unknown types per the spec.
    pub async fn recv_strict(&mut self) -> Result<HostToContainer, IpcError> {
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
        let result = decode_host_to_container(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    /// Receive a [`HostToContainer`] message.
    ///
    /// This is the spec-compliant default: unknown message types are
    /// silently skipped (with a warning log) up to 32 consecutive
    /// frames. Use [`recv_strict`](Self::recv_strict) if you need raw
    /// decode errors for every frame.
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv_strict().await {
                Ok(msg) => {
                    self.unknown_skip_count = 0;
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    self.unknown_skip_count += 1;
                    if self.unknown_skip_count > DEFAULT_MAX_UNKNOWN_SKIPS {
                        let err = IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
                            count: self.unknown_skip_count,
                            limit: DEFAULT_MAX_UNKNOWN_SKIPS,
                        });
                        self.poison().await;
                        return Err(err);
                    }
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %truncate_for_log(&ty),
                        skip_count = self.unknown_skip_count,
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }
}
