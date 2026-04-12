//! Container-side Unix socket client.
//!
//! Mirrors [`crate::server::IpcConnection`] but from the container's
//! perspective: [`IpcClient::connect`] dials the server, then
//! [`IpcClient::handshake`] sends the initial [`ReadyPayload`] and
//! awaits the host's [`InitPayload`] reply.

use std::path::Path;

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::Framed;

use crate::codec::{FrameCodec, decode_host_to_container, encode_message};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer, InitPayload, ReadyPayload};

/// Container-side IPC client.
///
/// Wraps a [`tokio_util::codec::Framed`] over a
/// [`tokio::net::UnixStream`] with the same
/// [`crate::codec::FrameCodec`] the host uses. Send/receive are
/// strongly typed in terms of the protocol enums.
#[derive(Debug)]
pub struct IpcClient {
    framed: Framed<UnixStream, FrameCodec>,
}

impl IpcClient {
    /// Connect to an IPC server previously bound at `path`.
    ///
    /// The socket path inside the container is
    /// `/workspace/ipc.sock` by convention (see
    /// [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md)
    /// §Transport), but this crate does not hardcode the path —
    /// callers pass whatever their container runtime mounted.
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, IpcError> {
        let stream = UnixStream::connect(path.as_ref()).await?;
        Ok(Self {
            framed: Framed::new(stream, FrameCodec::new()),
        })
    }

    /// Send a [`ContainerToHost`] message.
    pub async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        let bytes: Bytes = encode_message(msg)?;
        self.framed.send(bytes).await?;
        Ok(())
    }

    /// Receive a [`HostToContainer`] message.
    ///
    /// Returns [`IpcError::Closed`] if the host disconnected cleanly
    /// between frames.
    pub async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        let Some(frame) = self.framed.next().await.transpose()? else {
            return Err(IpcError::Closed);
        };
        decode_host_to_container(&frame)
    }

    /// Like [`recv`](Self::recv) but forward-compatible: unknown
    /// message types are logged at warn level and skipped, per
    /// `docs/IPC_PROTOCOL.md` §Error Handling ("Unknown message type:
    /// Ignored with a warning log").
    ///
    /// All other errors (IO, frame, malformed JSON, closed) are
    /// propagated as-is.
    pub async fn recv_lossy(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv().await {
                Ok(msg) => return Ok(msg),
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    tracing::warn!(
                        target: "forgeclaw_ipc::client",
                        message_type = %ty,
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Perform the container-side handshake.
    ///
    /// Sends the caller-supplied [`ReadyPayload`], then waits for the
    /// host's [`HostToContainer::Init`] response.
    ///
    /// Unknown message types received before `Init` are silently
    /// skipped per `docs/IPC_PROTOCOL.md` §Error Handling (forward
    /// compatibility). The first *known* non-`Init` message is a
    /// protocol violation and surfaces as
    /// [`ProtocolError::UnexpectedMessage`].
    pub async fn handshake(&mut self, ready: ReadyPayload) -> Result<InitPayload, IpcError> {
        self.send(&ContainerToHost::Ready(ready)).await?;
        let first = self.recv_lossy().await?;
        match first {
            HostToContainer::Init(payload) => Ok(payload),
            other => Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "init",
                got: other.type_name(),
            })),
        }
    }

    /// Cleanly close the connection.
    pub async fn close(mut self) -> Result<(), IpcError> {
        self.framed.close().await?;
        Ok(())
    }
}
