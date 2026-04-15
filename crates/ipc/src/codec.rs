//! Frame codec for the IPC wire format.
//!
//! The wire format is exactly:
//!
//! ```text
//! ┌──────────────┬──────────────────────────┐
//! │ Length (4B)  │ JSON Payload (N bytes)   │
//! │ big-endian u32│                         │
//! └──────────────┴──────────────────────────┘
//! ```
//!
//! - **Length** is a 4-byte unsigned big-endian integer, equal to the
//!   byte length of the payload that follows (it does **not** include
//!   the four length bytes themselves).
//! - **Payload** is UTF-8 encoded JSON. The codec additionally verifies
//!   it is a known [`crate::message::ContainerToHost`] or
//!   [`crate::message::HostToContainer`] message via the
//!   crate-private `decode_container_to_host` /
//!   `decode_host_to_container` helpers, which the server and client
//!   wrappers call inside their `recv_*` methods.
//! - **Maximum frame size** is [`MAX_FRAME_BYTES`] (10 MiB per
//!   `docs/IPC_PROTOCOL.md`). Frames whose declared length exceeds
//!   this cap are rejected immediately — the codec does **not**
//!   allocate the oversized buffer.
//!
//! The codec is implemented by hand (not via
//! [`tokio_util::codec::LengthDelimitedCodec`]) so the wire contract
//! is obvious at the call site rather than hidden in builder options.

use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use tokio_util::codec::{Decoder, Encoder};

/// Result alias used across the crate.
pub(crate) type CodecResult<T> = Result<T, IpcError>;

/// Size of the length prefix in bytes.
pub const LENGTH_PREFIX_BYTES: usize = 4;

/// Hard upper bound on the payload size of a single frame.
///
/// Per [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md) §Framing,
/// frames whose declared length exceeds this value are rejected with
/// a protocol error and the socket is torn down.
pub const MAX_FRAME_BYTES: usize = 10 * 1024 * 1024;

/// Default cap on consecutive unknown-type frames before poisoning.
pub(crate) const DEFAULT_MAX_UNKNOWN_SKIPS: usize = 32;

/// Length-prefixed JSON frame codec.
///
/// Implements [`tokio_util::codec::Encoder<Bytes>`] and
/// [`tokio_util::codec::Decoder`]. On encode, the caller owns the raw
/// payload bytes; on decode, a successful frame yields the payload
/// bytes (length prefix already stripped).
///
/// The codec produces / consumes opaque byte payloads, not typed
/// messages. Typed encoding and decoding live in the `encode_*` and
/// `decode_*` free functions below, keeping the codec itself thin and
/// letting tests exercise framing behavior without a full protocol
/// roundtrip.
#[derive(Debug, Default, Clone, Copy)]
pub struct FrameCodec;

impl FrameCodec {
    /// Build a new [`FrameCodec`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Encoder<Bytes> for FrameCodec {
    type Error = IpcError;

    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> CodecResult<()> {
        if item.is_empty() {
            return Err(IpcError::Frame(FrameError::EmptyFrame));
        }
        if item.len() > MAX_FRAME_BYTES {
            return Err(IpcError::Frame(FrameError::Oversize {
                size: item.len(),
                max: MAX_FRAME_BYTES,
            }));
        }
        dst.reserve(LENGTH_PREFIX_BYTES + item.len());
        // `item.len()` is already bounded by `MAX_FRAME_BYTES` (10 MiB)
        // so the `try_from` never fails in practice but we surface a
        // protocol error if it ever did.
        let len_u32 = u32::try_from(item.len()).map_err(|_| {
            IpcError::Frame(FrameError::Oversize {
                size: item.len(),
                max: MAX_FRAME_BYTES,
            })
        })?;
        dst.put_u32(len_u32);
        dst.extend_from_slice(&item);
        Ok(())
    }
}

impl Decoder for FrameCodec {
    type Item = Bytes;
    type Error = IpcError;

    fn decode(&mut self, src: &mut BytesMut) -> CodecResult<Option<Self::Item>> {
        if src.len() < LENGTH_PREFIX_BYTES {
            // Wait for more bytes to arrive.
            return Ok(None);
        }
        // Peek at the length header without consuming it yet; we want
        // to leave the buffer untouched if the whole frame has not
        // arrived.
        let mut header = [0u8; LENGTH_PREFIX_BYTES];
        header.copy_from_slice(&src[..LENGTH_PREFIX_BYTES]);
        let declared = u32::from_be_bytes(header) as usize;

        if declared == 0 {
            // Advance past the header so subsequent reads don't see
            // the same empty frame again, then surface the error.
            src.advance(LENGTH_PREFIX_BYTES);
            return Err(IpcError::Frame(FrameError::EmptyFrame));
        }

        if declared > MAX_FRAME_BYTES {
            // Advance past the header only. We deliberately do not
            // consume (or allocate) the oversized payload.
            src.advance(LENGTH_PREFIX_BYTES);
            return Err(IpcError::Frame(FrameError::Oversize {
                size: declared,
                max: MAX_FRAME_BYTES,
            }));
        }

        let total_frame = LENGTH_PREFIX_BYTES + declared;
        if src.len() < total_frame {
            // Wait for the remainder without reserving the full
            // declared frame size up front. This avoids memory
            // amplification from peers that declare large frames but
            // trickle or stall payload bytes.
            return Ok(None);
        }

        // Full frame present — drop the length prefix and take the
        // payload bytes.
        src.advance(LENGTH_PREFIX_BYTES);
        let payload = src.split_to(declared).freeze();
        Ok(Some(payload))
    }

    fn decode_eof(&mut self, src: &mut BytesMut) -> CodecResult<Option<Self::Item>> {
        // If we have some bytes but not a complete frame, that's a
        // truncation — the peer closed mid-frame.
        match self.decode(src)? {
            Some(frame) => Ok(Some(frame)),
            None => {
                if src.is_empty() {
                    Ok(None)
                } else if src.len() < LENGTH_PREFIX_BYTES {
                    Err(IpcError::Frame(FrameError::Truncated {
                        expected: LENGTH_PREFIX_BYTES,
                        got: src.len(),
                    }))
                } else {
                    let mut header = [0u8; LENGTH_PREFIX_BYTES];
                    header.copy_from_slice(&src[..LENGTH_PREFIX_BYTES]);
                    let declared = u32::from_be_bytes(header) as usize;
                    Err(IpcError::Frame(FrameError::Truncated {
                        expected: declared,
                        got: src.len() - LENGTH_PREFIX_BYTES,
                    }))
                }
            }
        }
    }
}

/// A minimal probe that extracts just the `type` field from a JSON
/// object, used to structurally distinguish unknown message types from
/// malformed known messages.
#[derive(serde::Deserialize)]
struct TypeProbe {
    #[serde(rename = "type")]
    ty: String,
}

/// Decode a frame payload into a typed protocol message.
///
/// Uses a single structural fallback to classify decode failures:
///
/// 1. Attempt full deserialization into `T` (hot path).
/// 2. On failure, classify:
///    - Invalid UTF-8 => [`FrameError::InvalidUtf8`].
///    - If the `type` value is not in `known_types`, return
///      [`ProtocolError::UnknownMessageType`].
///    - Otherwise (known type, bad payload), return
///      [`FrameError::MalformedJson`].
///
/// This avoids depending on serde's error wording for forward
/// compatibility.
pub(crate) fn decode_typed_message<T: DeserializeOwned>(
    bytes: &[u8],
    known_types: &[&str],
) -> Result<T, IpcError> {
    match serde_json::from_slice::<T>(bytes) {
        Ok(msg) => Ok(msg),
        Err(full_err) => {
            if std::str::from_utf8(bytes).is_err() {
                return Err(IpcError::Frame(FrameError::InvalidUtf8));
            }
            if let Ok(TypeProbe { ty }) = serde_json::from_slice::<TypeProbe>(bytes) {
                if !known_types.contains(&ty.as_str()) {
                    return Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty)));
                }
            }
            // Known type with bad payload, or no type field at all.
            Err(IpcError::Frame(FrameError::MalformedJson(
                full_err.to_string(),
            )))
        }
    }
}

/// Serialize a protocol message to an owned [`Bytes`] buffer suitable
/// for feeding into a [`FrameCodec`] encoder.
pub(crate) fn encode_message<T: Serialize>(msg: &T) -> Result<Bytes, IpcError> {
    let mut writer = CappedBytesWriter::new(MAX_FRAME_BYTES);
    match serde_json::to_writer(&mut writer, msg) {
        Ok(()) => Ok(writer.into_bytes()),
        Err(e) => {
            if let Some(size) = writer.overflow_size() {
                return Err(IpcError::Frame(FrameError::Oversize {
                    size,
                    max: MAX_FRAME_BYTES,
                }));
            }
            Err(IpcError::serialize(&e))
        }
    }
}

/// Typed alias — serialize a [`ContainerToHost`] message.
pub(crate) fn encode_container_to_host(msg: &ContainerToHost) -> Result<Bytes, IpcError> {
    encode_message(msg)
}

/// Typed alias — serialize a [`HostToContainer`] message.
pub(crate) fn encode_host_to_container(msg: &HostToContainer) -> Result<Bytes, IpcError> {
    encode_message(msg)
}

/// Streaming JSON writer that aborts once `max_bytes` is exceeded.
struct CappedBytesWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
    overflow_size: Option<usize>,
}

impl CappedBytesWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            max_bytes,
            overflow_size: None,
        }
    }

    fn overflow_size(&self) -> Option<usize> {
        self.overflow_size
    }

    fn into_bytes(self) -> Bytes {
        Bytes::from(self.bytes)
    }
}

impl Write for CappedBytesWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.len().saturating_add(buf.len());
        if next > self.max_bytes {
            self.overflow_size = Some(next);
            return Err(std::io::Error::other("serialized frame exceeds max size"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Typed alias — turn a frame into a [`ContainerToHost`].
pub fn decode_container_to_host(bytes: &[u8]) -> Result<ContainerToHost, IpcError> {
    decode_typed_message::<ContainerToHost>(bytes, ContainerToHost::KNOWN_TYPES)
}

/// Typed alias — turn a frame into a [`HostToContainer`].
pub fn decode_host_to_container(bytes: &[u8]) -> Result<HostToContainer, IpcError> {
    decode_typed_message::<HostToContainer>(bytes, HostToContainer::KNOWN_TYPES)
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
