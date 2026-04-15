use super::{
    FrameCodec, LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decode_container_to_host,
    decode_typed_message, encode_message_frame,
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
        adapter: "claude-code".parse().expect("valid adapter"),
        adapter_version: "1.0.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
    });
    let mut framed = BytesMut::new();
    encode_message_frame(&msg, &mut framed).expect("encode");
    let payload = &framed[LENGTH_PREFIX_BYTES..];
    let back = decode_container_to_host(payload).expect("decode");
    assert_eq!(back, msg);
}

#[test]
fn encode_message_rejects_oversize_before_full_buffer_materialization() {
    let msg = serde_json::json!({
        "type": "synthetic",
        "payload": "x".repeat(MAX_FRAME_BYTES + 256),
    });
    let mut framed = BytesMut::new();
    let err = encode_message_frame(&msg, &mut framed).expect_err("oversize payload must fail");
    assert!(
        matches!(
            err,
            IpcError::Frame(FrameError::Oversize {
                max: MAX_FRAME_BYTES,
                ..
            })
        ),
        "expected frame oversize, got {err:?}"
    );
}

#[test]
fn single_pass_classification_keeps_known_type_payload_malformed() {
    // Verify structural detection: type "ready" is known, so a
    // missing field classifies as MalformedJson (not UnknownMessageType),
    // regardless of serde's error message wording.
    let bytes = br#"{"type":"ready"}"#;
    let err = decode_typed_message::<ContainerToHost>(bytes, ContainerToHost::KNOWN_TYPES)
        .expect_err("should error");
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "known type with bad payload should be MalformedJson, got {err:?}"
    );
}
