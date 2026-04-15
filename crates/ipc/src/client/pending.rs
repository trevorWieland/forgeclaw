//! Pre-handshake client connection type.
//!
//! [`PendingClient`] is returned by [`super::IpcClient::connect`]
//! and only exposes the handshake. A successful handshake consumes
//! the pending client and returns an [`super::IpcClient`] with the
//! full post-handshake API.

use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::Instant;

use crate::codec::{
    DEFAULT_MAX_UNKNOWN_SKIPS, decode_host_to_container, encode_container_to_host_frame,
};
use crate::error::{IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer, InitPayload, ReadyPayload};
use crate::policy::{DEFAULT_MAX_UNKNOWN_BYTES, UnknownTrafficBudget};
use crate::transport::{FrameReader, FrameWriter};
use crate::util::{SharedWriteHalf, ShutdownHandle, truncate_for_log};

use super::{IpcClient, IpcClientOptions};

fn log_outbound_validation_rejection(err: &IpcError) {
    let IpcError::Protocol(ProtocolError::OutboundValidation {
        direction,
        message_type,
        field_path,
        reason,
    }) = err
    else {
        return;
    };
    tracing::warn!(
        target: "forgeclaw_ipc::client",
        phase = "handshake",
        direction,
        message_type,
        field_path = %truncate_for_log(field_path),
        reason = %truncate_for_log(reason),
        "rejected outbound IPC message before serialization"
    );
}

/// A container-side client that has not yet completed the handshake.
///
/// Only [`handshake`](Self::handshake) and [`close`](Self::close)
/// are available. A successful handshake consumes `self` and returns
/// an [`IpcClient`] with the full post-handshake API.
#[derive(Debug)]
pub struct PendingClient {
    reader: FrameReader<tokio::net::unix::OwnedReadHalf>,
    writer: FrameWriter<SharedWriteHalf<tokio::net::unix::OwnedWriteHalf>>,
    shutdown_handle: ShutdownHandle<tokio::net::unix::OwnedWriteHalf>,
    poisoned: bool,
    unknown_budget: UnknownTrafficBudget,
    last_frame_len: usize,
    options: IpcClientOptions,
}

impl PendingClient {
    pub(crate) fn from_stream(stream: UnixStream, options: IpcClientOptions) -> Self {
        let (read_half, write_half) = stream.into_split();
        let (shared_write, shutdown_handle) = SharedWriteHalf::new(write_half);
        Self {
            reader: FrameReader::new(read_half),
            writer: FrameWriter::new(shared_write),
            shutdown_handle,
            poisoned: false,
            unknown_budget: UnknownTrafficBudget::new(
                Instant::now(),
                options.unknown_traffic_limit,
            ),
            last_frame_len: 0,
            options,
        }
    }

    /// Perform the container-side handshake with a deadline.
    ///
    /// Consumes `self` and returns a fully established [`IpcClient`]
    /// paired with the host's [`InitPayload`].
    ///
    /// On any failure (including timeout) the underlying socket is
    /// shut down.
    pub async fn handshake(
        mut self,
        ready: ReadyPayload,
        timeout: Duration,
    ) -> Result<(IpcClient, InitPayload), IpcError> {
        match tokio::time::timeout(timeout, self.handshake_inner(ready)).await {
            Ok(Ok(init)) => {
                let client = IpcClient::from_parts(
                    self.reader,
                    self.writer,
                    self.shutdown_handle,
                    init.job_id.clone(),
                    self.options,
                );
                Ok((client, init))
            }
            Ok(Err(e)) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                Err(e)
            }
            Err(_elapsed) => {
                self.poison().await;
                Err(IpcError::Timeout(timeout))
            }
        }
    }

    async fn handshake_inner(&mut self, ready: ReadyPayload) -> Result<InitPayload, IpcError> {
        self.send(&ContainerToHost::Ready(ready)).await?;
        let first = self.recv().await?;
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

    async fn send(&mut self, msg: &ContainerToHost) -> Result<(), IpcError> {
        self.check_poisoned()?;
        let result = self
            .writer
            .send_with(|buf| encode_container_to_host_frame(msg, buf))
            .await;
        if let Err(ref e) = result {
            if matches!(
                e,
                IpcError::Protocol(ProtocolError::OutboundValidation { .. })
            ) {
                log_outbound_validation_rejection(e);
            }
        }
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv_strict(&mut self) -> Result<HostToContainer, IpcError> {
        self.check_poisoned()?;
        let frame = match self.reader.recv_frame().await {
            Ok(frame) => frame,
            Err(e) => {
                if e.is_fatal() {
                    self.poison().await;
                }
                return Err(e);
            }
        };
        self.last_frame_len = frame.len();
        let result = decode_host_to_container(&frame);
        if let Err(ref e) = result {
            if e.is_fatal() {
                self.poison().await;
            }
        }
        result
    }

    async fn recv(&mut self) -> Result<HostToContainer, IpcError> {
        loop {
            match self.recv_strict().await {
                Ok(msg) => {
                    self.unknown_budget.reset_consecutive();
                    return Ok(msg);
                }
                Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty))) => {
                    if let Err(err) = self
                        .unknown_budget
                        .on_unknown(self.last_frame_len, Instant::now())
                    {
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
                        total_skip_count = self.unknown_budget.total_count(),
                        total_skip_bytes = self.unknown_budget.total_bytes(),
                        "ignoring unknown message type (forward compatibility)"
                    );
                }
                Err(e) => return Err(e),
            }
        }
    }
}
