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

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_util::codec::{Decoder, Encoder};
#[path = "codec_type_probe.rs"]
mod type_probe;
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};

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
            // Reserve up-front so subsequent reads fill the right
            // buffer without reallocating mid-frame.
            src.reserve(total_frame - src.len());
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
/// object, used by the two-pass decode to structurally distinguish
/// unknown message types from malformed known messages.
#[derive(serde::Deserialize)]
struct TypeProbe {
    #[serde(rename = "type")]
    ty: String,
}

/// Decode a frame payload into a typed protocol message.
///
/// Uses a two-pass structural approach to classify decode failures:
///
/// 1. Attempt full deserialization into `T`.
/// 2. On failure, probe the `type` field:
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
    let text = std::str::from_utf8(bytes).map_err(|_| FrameError::InvalidUtf8)?;
    if let Some(ty) = type_probe::probe_first_type_field(text) {
        if !known_types.contains(&ty.as_str()) {
            return Err(IpcError::Protocol(ProtocolError::UnknownMessageType(ty)));
        }
    }
    match serde_json::from_slice::<T>(bytes) {
        Ok(msg) => Ok(msg),
        Err(full_err) => {
            // Two-pass: probe the type field structurally.
            if let Ok(TypeProbe { ty }) = serde_json::from_str::<TypeProbe>(text) {
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
    serde_json::to_vec(msg)
        .map(Bytes::from)
        .map_err(|e| IpcError::serialize(&e))
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
mod tests {
    use super::{
        FrameCodec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decode_container_to_host,
        decode_typed_message, encode_message,
    };
    use crate::error::{FrameError, IpcError, ProtocolError};
    use crate::message::{ContainerToHost, ReadyPayload};
    use bytes::{Bytes, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn encode_writes_big_endian_prefix() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(Bytes::from_static(b"hello"), &mut buf)
            .expect("encode");
        assert_eq!(&buf[..LENGTH_PREFIX_BYTES], &[0, 0, 0, 5]);
        assert_eq!(&buf[LENGTH_PREFIX_BYTES..], b"hello");
    }

    #[test]
    fn encode_rejects_empty_frame() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        let err = codec
            .encode(Bytes::new(), &mut buf)
            .expect_err("empty frame should error");
        assert!(matches!(err, IpcError::Frame(FrameError::EmptyFrame)));
        assert!(buf.is_empty());
    }

    #[test]
    fn encode_rejects_oversize_payload() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        let big = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
        let err = codec
            .encode(big, &mut buf)
            .expect_err("oversize payload should error");
        assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));
    }

    #[test]
    fn encode_accepts_max_frame_exactly() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(Bytes::from(vec![0u8; MAX_FRAME_BYTES]), &mut buf)
            .expect("encode at max");
        assert_eq!(buf.len(), LENGTH_PREFIX_BYTES + MAX_FRAME_BYTES);
    }

    #[test]
    fn decode_returns_none_on_empty_buffer() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        assert!(codec.decode(&mut buf).expect("decode").is_none());
    }

    #[test]
    fn decode_waits_for_full_header() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0, 0][..]);
        assert!(codec.decode(&mut buf).expect("decode").is_none());
        assert_eq!(buf.len(), 3); // buffer untouched
    }

    #[test]
    fn decode_waits_for_full_payload() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0, 0, 5, b'h', b'i'][..]);
        assert!(codec.decode(&mut buf).expect("decode").is_none());
        // Length prefix untouched because we had fewer than 5 payload bytes.
        assert_eq!(buf.len(), 6);
    }

    #[test]
    fn decode_yields_payload_on_full_frame() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0, 0, 5, b'h', b'e', b'l', b'l', b'o'][..]);
        let frame = codec.decode(&mut buf).expect("decode").expect("some");
        assert_eq!(&frame[..], b"hello");
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_handles_multiple_frames_in_buffer() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec
            .encode(Bytes::from_static(b"ab"), &mut buf)
            .expect("encode");
        codec
            .encode(Bytes::from_static(b"cde"), &mut buf)
            .expect("encode");
        let first = codec.decode(&mut buf).expect("decode").expect("some");
        let second = codec.decode(&mut buf).expect("decode").expect("some");
        assert_eq!(&first[..], b"ab");
        assert_eq!(&second[..], b"cde");
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_rejects_zero_length_frame() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0, 0, 0][..]);
        let err = codec
            .decode(&mut buf)
            .expect_err("zero-length frame should error");
        assert!(matches!(err, IpcError::Frame(FrameError::EmptyFrame)));
        // Header was consumed so subsequent decodes don't re-trigger.
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_rejects_oversize_declared_length() {
        let mut codec = FrameCodec::new();
        // 0x00_C0_00_00 = 12 MiB > 10 MiB cap.
        let mut buf = BytesMut::from(&[0x00, 0xC0, 0x00, 0x00][..]);
        let err = codec
            .decode(&mut buf)
            .expect_err("oversize declared length should error");
        assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));
        // Header consumed but oversized payload was never buffered.
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_eof_on_truncated_header_errors() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0][..]);
        let err = codec
            .decode_eof(&mut buf)
            .expect_err("truncated header should error");
        assert!(matches!(
            err,
            IpcError::Frame(FrameError::Truncated {
                expected: 4,
                got: 2
            })
        ));
    }

    #[test]
    fn decode_eof_on_truncated_payload_errors() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::from(&[0u8, 0, 0, 10, b'x', b'y', b'z'][..]);
        let err = codec
            .decode_eof(&mut buf)
            .expect_err("truncated payload should error");
        assert!(matches!(
            err,
            IpcError::Frame(FrameError::Truncated {
                expected: 10,
                got: 3
            })
        ));
    }

    #[test]
    fn decode_eof_on_empty_buffer_is_none() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        assert!(codec.decode_eof(&mut buf).expect("decode_eof").is_none());
    }

    #[test]
    fn encode_then_decode_roundtrip_variable_sizes() {
        let mut codec = FrameCodec::new();
        for size in [1usize, 2, 4, 128, 4096, 65536] {
            let payload = vec![0xABu8; size];
            let mut buf = BytesMut::new();
            codec
                .encode(Bytes::from(payload.clone()), &mut buf)
                .expect("encode");
            let decoded = codec.decode(&mut buf).expect("decode").expect("some");
            assert_eq!(&decoded[..], &payload[..]);
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn decode_rejects_non_utf8() {
        let bytes = &[0xFFu8, 0xFE, 0xFD][..];
        let err = decode_container_to_host(bytes).expect_err("non-UTF8 should error");
        assert!(matches!(err, IpcError::Frame(FrameError::InvalidUtf8)));
    }

    #[test]
    fn decode_rejects_malformed_json() {
        let bytes = b"{not json";
        let err = decode_container_to_host(bytes).expect_err("malformed JSON should error");
        assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
    }

    #[test]
    fn decode_unknown_type_is_protocol_error() {
        let bytes = br#"{"type":"definitely_not_a_real_message"}"#;
        let err = decode_container_to_host(bytes).expect_err("unknown type should error");
        assert!(
            matches!(
                &err,
                IpcError::Protocol(ProtocolError::UnknownMessageType(n))
                    if n == "definitely_not_a_real_message"
            ),
            "expected UnknownMessageType, got {err:?}"
        );
    }

    #[test]
    fn decode_known_type_missing_field_is_malformed() {
        // "ready" is known, but the payload is missing required fields.
        let bytes = br#"{"type":"ready"}"#;
        let err = decode_container_to_host(bytes).expect_err("missing field should error");
        assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
    }

    #[test]
    fn decode_no_type_field_is_malformed() {
        let bytes = br#"{"adapter":"x"}"#;
        let err = decode_container_to_host(bytes).expect_err("missing type should error");
        assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
    }

    #[test]
    fn encode_message_roundtrips_through_decode() {
        let msg = ContainerToHost::Ready(ReadyPayload {
            adapter: "claude-code".to_owned(),
            adapter_version: "1.0.0".to_owned(),
            protocol_version: "1.0".to_owned(),
        });
        let bytes = encode_message(&msg).expect("encode");
        let back = decode_container_to_host(&bytes).expect("decode");
        assert_eq!(back, msg);
    }

    #[test]
    fn two_pass_uses_known_types_not_serde_wording() {
        // Verify structural detection: type "ready" is known, so a
        // missing field classifies as MalformedJson not UnknownMessageType,
        // regardless of serde's error message wording.
        let bytes = br#"{"type":"ready"}"#;
        let err = decode_typed_message::<ContainerToHost>(bytes, ContainerToHost::KNOWN_TYPES)
            .expect_err("should error");
        assert!(
            matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
            "known type with bad payload should be MalformedJson, got {err:?}"
        );
    }
}
