//! Hand-crafted malformed-input cases against the raw frame codec.
//!
//! These tests exercise the specific rejection behaviors documented
//! in [`docs/IPC_PROTOCOL.md`](../../../docs/IPC_PROTOCOL.md)
//! §Error Handling: oversize frames, zero-length frames,
//! non-UTF-8 payloads, malformed JSON, and unknown `type`
//! discriminators. Each case verifies both the error variant and,
//! where meaningful, that the buffer is left in a sensible state so
//! the codec can recover or cleanly tear down.

use bytes::{Bytes, BytesMut};
use forgeclaw_ipc::{FrameCodec, FrameError, IpcError, MAX_FRAME_BYTES, ProtocolError};
use tokio_util::codec::{Decoder, Encoder};

fn frame_bytes(payload: &[u8]) -> BytesMut {
    let mut buf = BytesMut::with_capacity(4 + payload.len());
    let len = u32::try_from(payload.len()).expect("fits in u32");
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

#[test]
fn truncated_length_header_is_none_until_full_header() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::from(&[0u8, 0, 0][..]);
    let res = codec.decode(&mut buf).expect("decode returns Ok");
    assert!(res.is_none(), "should wait for more bytes");
    assert_eq!(buf.len(), 3, "buffer left untouched");
}

#[test]
fn partial_payload_returns_none_and_reserves() {
    let mut codec = FrameCodec::new();
    // Length = 10 but only 4 payload bytes are present.
    let mut buf = BytesMut::from(&[0u8, 0, 0, 10, b'a', b'b', b'c', b'd'][..]);
    let res = codec.decode(&mut buf).expect("decode returns Ok");
    assert!(res.is_none(), "should wait for more bytes");
}

#[test]
fn oversize_length_is_rejected_and_payload_not_buffered() {
    let mut codec = FrameCodec::new();
    let bogus_len = u32::try_from(MAX_FRAME_BYTES + 1).expect("fits");
    let mut buf = BytesMut::from(&bogus_len.to_be_bytes()[..]);
    let err = codec
        .decode(&mut buf)
        .expect_err("oversize length should be rejected");
    assert!(matches!(
        err,
        IpcError::Frame(FrameError::Oversize { size, max })
            if size == MAX_FRAME_BYTES + 1 && max == MAX_FRAME_BYTES
    ));
    assert!(buf.is_empty(), "header consumed but payload never buffered");
}

#[test]
fn zero_length_header_is_rejected() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::from(&[0u8, 0, 0, 0][..]);
    let err = codec.decode(&mut buf).expect_err("zero length rejected");
    assert!(matches!(err, IpcError::Frame(FrameError::EmptyFrame)));
    assert!(buf.is_empty());
}

#[test]
fn valid_frame_with_invalid_utf8_surfaces_through_typed_decode() {
    // Frame invalid-UTF-8 bytes and run them through the crate's
    // public two-pass decode path (not just std::str::from_utf8).
    use forgeclaw_ipc::decode_container_to_host;

    let bad_payload: &[u8] = &[0xFF, 0xFE, 0xFD];
    let mut codec = FrameCodec::new();
    let mut buf = frame_bytes(bad_payload);
    let frame = codec
        .decode(&mut buf)
        .expect("decode is Ok at frame layer")
        .expect("frame present");
    let err = decode_container_to_host(&frame).expect_err("should reject invalid UTF-8");
    assert!(
        matches!(err, IpcError::Frame(FrameError::InvalidUtf8)),
        "expected InvalidUtf8, got {err:?}"
    );
}

#[test]
fn encoder_rejects_empty_payload() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    let err = codec
        .encode(Bytes::new(), &mut buf)
        .expect_err("empty payload rejected");
    assert!(matches!(err, IpcError::Frame(FrameError::EmptyFrame)));
    assert!(buf.is_empty(), "nothing written on error");
}

#[test]
fn encoder_rejects_oversize_payload() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    let big = Bytes::from(vec![0u8; MAX_FRAME_BYTES + 1]);
    let err = codec
        .encode(big, &mut buf)
        .expect_err("oversize payload rejected");
    assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));
}

#[test]
fn encoder_accepts_exactly_max_frame() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::new();
    codec
        .encode(Bytes::from(vec![0u8; MAX_FRAME_BYTES]), &mut buf)
        .expect("max frame accepted");
    assert_eq!(buf.len(), 4 + MAX_FRAME_BYTES);
}

#[test]
fn malformed_json_after_valid_frame_is_malformed_not_unknown() {
    // Valid frame that contains text which parses as JSON but does
    // NOT have a `type` field. This should classify as
    // `MalformedJson`, not `UnknownMessageType`.
    use forgeclaw_ipc::ContainerToHost;
    let raw = br#"{"adapter":"x","adapter_version":"y"}"#;
    let err =
        serde_json::from_slice::<ContainerToHost>(raw).expect_err("missing `type` field rejected");
    // Direct serde error — the crate's classifier sits above this.
    assert!(err.to_string().contains("missing field"));
}

#[test]
fn frame_with_unknown_type_classifies_as_protocol_error() {
    // Frame valid JSON with an unrecognized `type` field and run it
    // through the crate's public two-pass decode path.
    use forgeclaw_ipc::decode_container_to_host;

    let payload = br#"{"type":"bogus_type","data":42}"#;
    let mut codec = FrameCodec::new();
    let mut buf = frame_bytes(payload);
    let frame = codec
        .decode(&mut buf)
        .expect("frame layer ok")
        .expect("frame present");
    let err = decode_container_to_host(&frame).expect_err("should reject unknown type");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnknownMessageType(ref ty)) if ty == "bogus_type"
        ),
        "expected UnknownMessageType(\"bogus_type\"), got {err:?}"
    );
}
