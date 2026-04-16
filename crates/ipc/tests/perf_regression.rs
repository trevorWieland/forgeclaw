//! Performance regression gates.
//!
//! Finding #3 from the Lane 0.4 audit: the criterion benches in
//! `crates/ipc/benches/` are only manually invoked, so decode
//! regressions can land without CI signal. This file adds a
//! deterministic gate that runs under `cargo nextest` — included in
//! `just ci`'s `test` step — and combines **functional invariants**
//! (buffer capacity bounds, allocation-free reject paths) with
//! CI-noise-aware **wall-clock ceilings**.
//!
//! The wall-clock ceilings are generous: the default multiplier is 5×
//! the observed local best-of-5; under CI (`$CI=true`) it tightens to
//! 3× (laptops don't churn, CI doesn't flake).

use std::time::{Duration, Instant};

use bytes::BytesMut;
use forgeclaw_core::JobId;
use forgeclaw_ipc::{
    ContainerToHost, FrameCodec, HeartbeatPayload, MAX_FRAME_BYTES, OutputCompletePayload,
    OutputDeltaPayload, ReadyPayload, StopReason, decode_container_to_host,
};
use tokio_util::codec::{Decoder, Encoder};

fn ceiling(per_iter_nanos: u64) -> Duration {
    let multiplier: u64 = if std::env::var("CI").is_ok() { 3 } else { 5 };
    Duration::from_nanos(per_iter_nanos.saturating_mul(multiplier))
}

fn time_per_iter<F: FnMut()>(iterations: u32, mut body: F) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        body();
    }
    let elapsed = start.elapsed();
    elapsed / iterations.max(1)
}

fn encode_frame(payload: &[u8]) -> BytesMut {
    let mut codec = FrameCodec::new();
    let mut out = BytesMut::new();
    codec
        .encode(bytes::Bytes::copy_from_slice(payload), &mut out)
        .expect("encode frame");
    out
}

// ─────────────────────────────────────────────────────────────────────
// Functional invariants — hardware-independent assertions.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn burst_idle_reclaims_retained_capacity() {
    // Mirror `benches/buffer_retention.rs`: push a 1 MiB frame,
    // drain, then push 16 small frames. The reclaim pass must not
    // retain more than 512 KiB of capacity.
    const BIG: usize = 1 << 20;
    const SMALL: usize = 32;

    let big_payload = vec![0xA5u8; BIG];
    let small_payload = vec![0xA5u8; SMALL];
    let mut codec = FrameCodec::new();
    let mut reader_buf = BytesMut::new();

    reader_buf.extend_from_slice(&encode_frame(&big_payload));
    let big = codec.decode(&mut reader_buf).expect("decode big");
    assert!(big.is_some());
    for _ in 0..16 {
        reader_buf.extend_from_slice(&encode_frame(&small_payload));
        let frame = codec.decode(&mut reader_buf).expect("decode small");
        assert!(frame.is_some());
    }

    let retained = reader_buf.capacity();
    assert!(
        retained <= 512 * 1024,
        "buffer must reclaim oversized capacity after idle — retained {retained} bytes"
    );
}

#[test]
fn oversize_length_rejects_without_allocation() {
    // Length header declares 16 MiB (> MAX_FRAME_BYTES); the codec
    // must reject without allocating for the payload.
    const OVERSIZE: usize = 0x00FF_0000;
    const _: () = assert!(OVERSIZE > MAX_FRAME_BYTES);
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::from(&[0x00u8, 0xFF, 0x00, 0x00][..]);
    let err = codec
        .decode(&mut buf)
        .expect_err("oversize length must reject");
    // On reject, only the 4-byte length header is consumed. Buffer
    // capacity must stay below 64 bytes (leaving headroom for
    // whatever BytesMut's internal rounding does).
    assert!(buf.capacity() <= 64, "capacity: {}", buf.capacity());
    drop(err);
}

#[test]
fn zero_length_frame_rejects_without_allocation() {
    let mut codec = FrameCodec::new();
    let mut buf = BytesMut::from(&[0x00u8, 0x00, 0x00, 0x00][..]);
    let err = codec
        .decode(&mut buf)
        .expect_err("zero-length frame must reject");
    drop(err);
    assert!(buf.capacity() <= 64);
}

// ─────────────────────────────────────────────────────────────────────
// Wall-clock ceilings — generous, CI-aware.
// ─────────────────────────────────────────────────────────────────────

const ITERS: u32 = 2048;

fn ready_frame() -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::Ready(ReadyPayload {
        adapter: "bench-adapter".parse().expect("valid adapter"),
        adapter_version: "1.0.0".parse().expect("valid adapter version"),
        protocol_version: "1.1".parse().expect("valid protocol version"),
    }))
    .expect("serialize ready")
}

fn output_delta_frame(size: usize) -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::OutputDelta(OutputDeltaPayload {
        text: "x".repeat(size).parse().expect("valid text"),
        job_id: JobId::new("job-bench").expect("valid job id"),
    }))
    .expect("serialize delta")
}

fn output_complete_frame() -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::OutputComplete(OutputCompletePayload {
        job_id: JobId::new("job-bench").expect("valid job id"),
        result: Some("done".parse().expect("valid result")),
        session_id: None,
        token_usage: None,
        stop_reason: StopReason::EndTurn,
    }))
    .expect("serialize complete")
}

fn heartbeat_frame() -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::Heartbeat(HeartbeatPayload {
        timestamp: "2026-04-16T00:00:00Z".parse().expect("valid timestamp"),
    }))
    .expect("serialize heartbeat")
}

#[test]
fn decode_ready_under_ceiling() {
    let bytes = ready_frame();
    // Local best-of-5 is ~2µs on a reference laptop; ceiling 20µs (5×
    // × 2µs). CI tightens to 6µs but we use the coarse 20µs anyway
    // since the multiplier is applied by `ceiling()`.
    let per = time_per_iter(ITERS, || {
        let msg = decode_container_to_host(&bytes).expect("decode ready");
        std::hint::black_box(msg);
    });
    let limit = ceiling(20_000);
    assert!(per <= limit, "decode_ready took {per:?}, ceiling {limit:?}");
}

#[test]
fn decode_output_delta_1kib_under_ceiling() {
    let bytes = output_delta_frame(1024);
    let per = time_per_iter(ITERS, || {
        let msg = decode_container_to_host(&bytes).expect("decode delta 1k");
        std::hint::black_box(msg);
    });
    let limit = ceiling(30_000);
    assert!(
        per <= limit,
        "decode_output_delta_1kib took {per:?}, ceiling {limit:?}"
    );
}

#[test]
fn decode_output_delta_64kib_under_ceiling() {
    let bytes = output_delta_frame(65_536);
    let per = time_per_iter(ITERS, || {
        let msg = decode_container_to_host(&bytes).expect("decode delta 64k");
        std::hint::black_box(msg);
    });
    let limit = ceiling(400_000);
    assert!(
        per <= limit,
        "decode_output_delta_64kib took {per:?}, ceiling {limit:?}"
    );
}

#[test]
fn decode_output_complete_under_ceiling() {
    let bytes = output_complete_frame();
    let per = time_per_iter(ITERS, || {
        let msg = decode_container_to_host(&bytes).expect("decode complete");
        std::hint::black_box(msg);
    });
    let limit = ceiling(15_000);
    assert!(
        per <= limit,
        "decode_output_complete took {per:?}, ceiling {limit:?}"
    );
}

#[test]
fn decode_heartbeat_under_ceiling() {
    let bytes = heartbeat_frame();
    let per = time_per_iter(ITERS, || {
        let msg = decode_container_to_host(&bytes).expect("decode heartbeat");
        std::hint::black_box(msg);
    });
    let limit = ceiling(10_000);
    assert!(
        per <= limit,
        "decode_heartbeat took {per:?}, ceiling {limit:?}"
    );
}

#[test]
fn decode_unknown_type_256kib_under_ceiling() {
    let mut payload = String::from(r#"{"type":"future_message","junk":""#);
    payload.extend(std::iter::repeat_n('x', 256 * 1024));
    payload.push_str(r#""}"#);
    let bytes = payload.into_bytes();
    let per = time_per_iter(256, || {
        let err = decode_container_to_host(&bytes).expect_err("unknown large");
        std::hint::black_box(err);
    });
    let limit = ceiling(2_000_000);
    assert!(
        per <= limit,
        "decode_unknown_type_256kib took {per:?}, ceiling {limit:?}"
    );
}

#[test]
fn decode_malformed_known_256kib_under_ceiling() {
    let mut payload = String::from(r#"{"type":"output_delta","text":["not a string "#);
    payload.push_str(&"x".repeat(256 * 1024));
    payload.push_str(r#""]}"#);
    let bytes = payload.into_bytes();
    let per = time_per_iter(256, || {
        let err = decode_container_to_host(&bytes).expect_err("malformed large");
        std::hint::black_box(err);
    });
    let limit = ceiling(3_000_000);
    assert!(
        per <= limit,
        "decode_malformed_known_256kib took {per:?}, ceiling {limit:?}"
    );
}
