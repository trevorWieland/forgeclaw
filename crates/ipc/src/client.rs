//! Container-side Unix socket client.
//!
//! Mirrors [`crate::server::IpcConnection`] but from the container's
//! perspective: [`IpcClient::connect`] dials the server, then
//! [`IpcClient::handshake`] sends the initial [`ReadyPayload`] and
//! awaits the host's [`InitPayload`] reply.
//!
//! After handshake, call [`IpcClient::into_split`] to get
//! independent read/write halves for full-duplex operation.
//!
//! On any fatal error the client is automatically poisoned: the
//! underlying socket is shut down and all subsequent calls return
//! [`IpcError::Closed`].

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::codec::{FrameCodec, decode_host_to_container, encode_message};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer, InitPayload, ReadyPayload};
use crate::util::truncate_for_log;

/// Container-side IPC client.
///
/// On any fatal error the underlying socket is shut down and the
/// client is poisoned — all subsequent calls return
/// [`IpcError::Closed`].
#[derive(Debug)]
pub struct IpcClient {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
}

impl IpcClient {
    /// Connect to an IPC server previously bound at `path`.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, IpcError> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        Ok(Self {
            framed: Framed::new(stream, FrameCodec::new()),
            poisoned: false,
        })
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

    /// Receive a [`HostToContainer`] message.
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
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

    /// Forward-compatible receive: skip unknown message types.
    pub async fn recv_lossy(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv().await {
                Ok(msg) => return Ok(msg),
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %truncate_for_log(&ty),
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Perform the container-side handshake. Poisons on failure.
    pub async fn handshake(&mut self, ready: ReadyPayload) -> Result<InitPayload, IpcError> {
        self.send(&ContainerToHost::Ready(ready)).await?;
        let first = self.recv_lossy().await?;
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

    /// Split into independent read and write halves for full-duplex.
    ///
    /// Any bytes already buffered by the internal `Framed` reader
    /// are preserved in the returned reader half.
    pub fn into_split(self) -> (IpcClientWriter, IpcClientReader) {
        let poisoned = Arc::new(AtomicBool::new(self.poisoned));
        let parts = self.framed.into_parts();
        let (read_half, write_half) = parts.io.into_split();
        let mut reader = FramedRead::new(read_half, FrameCodec::new());
        if !parts.read_buf.is_empty() {
            *reader.read_buffer_mut() = parts.read_buf;
        }
        let mut writer = FramedWrite::new(write_half, FrameCodec::new());
        if !parts.write_buf.is_empty() {
            *writer.write_buffer_mut() = parts.write_buf;
        }
        (
            IpcClientWriter {
                writer,
                poisoned: Arc::clone(&poisoned),
            },
            IpcClientReader { reader, poisoned },
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
    writer: FramedWrite<tokio::net::unix::OwnedWriteHalf, FrameCodec>,
    poisoned: Arc<AtomicBool>,
}

impl IpcClientWriter {
    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            let _ = self.writer.get_mut().shutdown().await;
            return Err(IpcError::Closed);
        }
        let bytes: Bytes = encode_message(msg)?;
        let result = self.writer.send(bytes).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poisoned.store(true, Ordering::Release);
                let _ = self.writer.get_mut().shutdown().await;
            }
        }
        result
    }
}

/// Read half of a split [`IpcClient`].
#[derive(Debug)]
pub struct IpcClientReader {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, FrameCodec>,
    poisoned: Arc<AtomicBool>,
}

impl IpcClientReader {
    /// Receive a [`HostToContainer`] message.
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        if self.poisoned.load(Ordering::Acquire) {
            return Err(IpcError::Closed);
        }
        let frame = match self.reader.next().await.transpose() {
            Ok(Some(f)) => f,
            Ok(None) => {
                self.poisoned.store(true, Ordering::Release);
                return Err(IpcError::Closed);
            }
            Err(e) => {
                self.poisoned.store(true, Ordering::Release);
                return Err(e);
            }
        };
        let result = decode_host_to_container(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poisoned.store(true, Ordering::Release);
            }
        }
        result
    }

    /// Forward-compatible receive: skip unknown message types.
    pub async fn recv_lossy(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv().await {
                Ok(msg) => return Ok(msg),
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %truncate_for_log(&ty),
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }
}
