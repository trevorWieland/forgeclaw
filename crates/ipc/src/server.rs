//! Host-side Unix socket server and connection types.
//!
//! The [`IpcServer`] owns the bind lifecycle: it creates the socket
//! file at a caller-supplied path, unlinks any stale *socket* at that
//! path first, and cleans up on drop. Each accepted peer becomes an
//! [`IpcConnection`], which wraps a [`tokio_util::codec::Framed`]
//! over the accepted [`tokio::net::UnixStream`] with a
//! [`crate::codec::FrameCodec`] on top.
//!
//! The handshake lifecycle (`Ready → Init`) is encapsulated in
//! [`IpcConnection::handshake`] so higher-level crates never reach
//! into the raw send/recv primitives just to establish a session.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::codec::{FrameCodec, decode_container_to_host, encode_message};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer, InitPayload, ReadyPayload};
use crate::util::{SharedWriteHalf, ShutdownHandle, truncate_for_log};
use crate::version::{PROTOCOL_VERSION, is_compatible};

/// Unix-socket server for the host side of the IPC protocol.
///
/// An [`IpcServer`] owns a single [`tokio::net::UnixListener`] and the
/// filesystem path the listener is bound to. Accepted peers are
/// returned as [`IpcConnection`]s; one container equals one
/// connection, and the connection's lifetime equals the container's
/// session lifetime.
#[derive(Debug)]
pub struct IpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl IpcServer {
    /// Bind a Unix socket at `path`.
    ///
    /// If a **stale Unix socket** already exists at the path, it is
    /// removed first so that a host restart succeeds without manual
    /// intervention. If a non-socket file (regular file, directory,
    /// symlink, etc.) exists at the path, the call fails with
    /// [`std::io::ErrorKind::AlreadyExists`] rather than silently
    /// deleting an unrelated file.
    ///
    /// Parent directories are NOT created — the caller is responsible
    /// for ensuring the workspace directory exists (Lane 1.1 will
    /// wire that through container spec mounts).
    pub fn bind(path: impl AsRef<Path>) -> Result<Self, IpcError> {
        let socket_path = path.as_ref().to_path_buf();
        // `bind` is a startup-time operation so the short blocking
        // metadata check + unlink here is fine.
        clean_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        tracing::debug!(
            target: "forgeclaw_ipc::server",
            path = %socket_path.display(),
            "bound IPC server"
        );
        Ok(Self {
            listener,
            socket_path,
        })
    }

    /// Accept one container connection.
    ///
    /// Returns when a peer has fully connected; the returned
    /// [`IpcConnection`] is ready to call
    /// [`IpcConnection::handshake`].
    pub async fn accept(&self) -> Result<IpcConnection, IpcError> {
        let (stream, _addr) = self.listener.accept().await?;
        Ok(IpcConnection::from_stream(stream))
    }

    /// Returns the filesystem path this server is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Only unlink if the path is still a Unix socket (defensive
        // against the file being replaced between bind and drop).
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            match std::fs::symlink_metadata(&self.socket_path) {
                Ok(meta) if meta.file_type().is_socket() => {
                    if let Err(e) = std::fs::remove_file(&self.socket_path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(
                                target: "forgeclaw_ipc::server",
                                path = %self.socket_path.display(),
                                error = %e,
                                "failed to remove IPC socket file on drop"
                            );
                        }
                    }
                }
                Ok(_) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::server",
                        path = %self.socket_path.display(),
                        "socket path replaced with non-socket; skipping cleanup"
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::server",
                        path = %self.socket_path.display(),
                        error = %e,
                        "failed to stat IPC socket file on drop"
                    );
                }
            }
        }
    }
}

/// Inspect the filesystem at `path` and remove a stale Unix socket
/// if present. Errors on non-socket entries; ignores missing paths.
#[cfg(unix)]
fn clean_stale_socket(path: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::FileTypeExt;

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            std::fs::remove_file(path)?;
            tracing::debug!(
                target: "forgeclaw_ipc::server",
                path = %path.display(),
                "removed stale socket file"
            );
            Ok(())
        }
        Ok(_) => Err(IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("path exists and is not a Unix socket: {}", path.display()),
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IpcError::Io(e)),
    }
}

/// An accepted host-side connection to a single container.
///
/// Exposes typed send / receive primitives plus a single-call
/// [`handshake`](IpcConnection::handshake) that performs the `Ready →
/// Init` exchange documented in `docs/IPC_PROTOCOL.md` §Lifecycle.
///
/// After handshake, call [`into_split`](IpcConnection::into_split) to
/// get independent read/write halves for full-duplex operation.
///
/// On any fatal error (frame, I/O, version mismatch, unexpected
/// message), the connection is automatically poisoned: the
/// underlying socket is shut down and all subsequent calls return
/// [`IpcError::Closed`].
#[derive(Debug)]
pub struct IpcConnection {
    framed: Framed<UnixStream, FrameCodec>,
    poisoned: bool,
}

impl IpcConnection {
    fn from_stream(stream: UnixStream) -> Self {
        Self {
            framed: Framed::new(stream, FrameCodec::new()),
            poisoned: false,
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

    /// Send a [`HostToContainer`] message to the peer.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
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

    /// Receive a [`ContainerToHost`] message from the peer.
    ///
    /// On fatal errors the connection is poisoned and the underlying
    /// socket is shut down. Returns [`IpcError::Closed`] if the peer
    /// disconnected cleanly or the connection was previously poisoned.
    pub async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
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
        let result = decode_container_to_host(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    /// Like [`recv_container`](Self::recv_container) but
    /// forward-compatible: unknown message types are logged at warn
    /// level and skipped per `docs/IPC_PROTOCOL.md` §Error Handling.
    pub async fn recv_container_lossy(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container().await {
                Ok(msg) => return Ok(msg),
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::server",
                        message_type = %truncate_for_log(&ty),
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Perform the host-side handshake.
    ///
    /// On any failure the connection is poisoned automatically.
    pub async fn handshake(&mut self, init: InitPayload) -> Result<ReadyPayload, IpcError> {
        let result = self.handshake_inner(init).await;
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn handshake_inner(&mut self, init: InitPayload) -> Result<ReadyPayload, IpcError> {
        let first = self.recv_container_lossy().await?;
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
        self.send_host(&HostToContainer::Init(init)).await?;
        tracing::debug!(
            target: "forgeclaw_ipc::server",
            adapter = %truncate_for_log(&ready.adapter),
            adapter_version = %truncate_for_log(&ready.adapter_version),
            protocol_version = %truncate_for_log(&ready.protocol_version),
            "IPC handshake complete"
        );
        Ok(ready)
    }

    /// Split into independent read and write halves for full-duplex
    /// operation. Use after handshake.
    ///
    /// Any bytes already buffered by the internal `Framed` reader
    /// (e.g. pipelined by the peer during handshake) are preserved in
    /// the returned reader half. A fatal error on either half poisons
    /// both and shuts down the transport.
    pub fn into_split(self) -> (IpcConnectionWriter, IpcConnectionReader) {
        let poisoned = Arc::new(AtomicBool::new(self.poisoned));
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
            },
            IpcConnectionReader {
                reader,
                poisoned,
                shutdown_handle,
            },
        )
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
}

impl IpcConnectionWriter {
    /// Send a [`HostToContainer`] message.
    pub async fn send_host(&mut self, msg: &HostToContainer) -> Result<(), IpcError> {
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

/// Read half of a split [`IpcConnection`].
///
/// On fatal errors the reader immediately shuts down the shared write
/// half so the peer observes EOF without waiting for the writer task.
#[derive(Debug)]
pub struct IpcConnectionReader {
    reader: FramedRead<tokio::net::unix::OwnedReadHalf, FrameCodec>,
    poisoned: Arc<AtomicBool>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
}

impl IpcConnectionReader {
    /// Poison the connection and shut down the write half immediately.
    async fn poison(&mut self) {
        self.poisoned.store(true, Ordering::Release);
        self.shutdown_handle.trigger().await;
    }

    /// Receive a [`ContainerToHost`] message.
    pub async fn recv_container(&mut self) -> Result<ContainerToHost, IpcError> {
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
        let result = decode_container_to_host(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    /// Forward-compatible receive: skip unknown message types.
    pub async fn recv_container_lossy(&mut self) -> Result<ContainerToHost, IpcError> {
        loop {
            match self.recv_container().await {
                Ok(msg) => return Ok(msg),
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::server",
                        message_type = %truncate_for_log(&ty),
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }
}
