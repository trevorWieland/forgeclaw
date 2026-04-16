//! Adversarial tests for independent unknown-traffic hardening.

use std::fmt::Write as _;

use forgeclaw_ipc::{
    ContainerToHost, IpcError, IpcInboundEvent, IpcServer, IpcServerOptions, ProtocolError,
    UnknownTrafficLimitConfig,
};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "common/mod.rs"]
mod common;
use common::{HS_TIMEOUT, sample_group, sample_init, sample_ready, socket_path};

fn server_options(unknown: UnknownTrafficLimitConfig) -> IpcServerOptions {
    IpcServerOptions {
        unknown_traffic_limit: unknown,
        ..IpcServerOptions::insecure_capture_only()
    }
}

async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("frame len fits u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(payload).await.expect("write payload");
}

async fn read_init_frame(stream: &mut tokio::net::UnixStream) {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read init len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .expect("read init payload");
}

fn heartbeat_frame() -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::Heartbeat(
        forgeclaw_ipc::HeartbeatPayload {
            timestamp: "2026-04-03T10:30:00Z".parse().expect("valid timestamp"),
        },
    ))
    .expect("serialize heartbeat")
}

#[tokio::test]
async fn handshake_rejects_unknown_lifetime_cap_breach() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-handshake-limit.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 1,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 0,
            rate_limit_refill_per_second: 0,
            ignored_field_lifetime_keys: 0,
            ignored_field_lifetime_bytes: 0,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(&mut raw, br#"{"type":"experimental_a","x":1}"#).await;
    write_frame(&mut raw, br#"{"type":"experimental_b","x":2}"#).await;
    raw.flush().await.expect("flush");

    let err = accept_task
        .await
        .expect("join")
        .expect_err("handshake should fail");
    drop(raw);
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { count: 2, limit: 1 })
    ));
}

#[tokio::test]
async fn unsplit_rejects_alternating_unknown_and_heartbeat_via_lifetime_cap() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-unsplit-lifetime.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 3,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 0,
            rate_limit_refill_per_second: 0,
            ignored_field_lifetime_keys: 0,
            ignored_field_lifetime_bytes: 0,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        for _ in 0..3 {
            let event = conn.recv_event().await.expect("heartbeat event");
            assert!(matches!(
                event,
                IpcInboundEvent::Message(ContainerToHost::Heartbeat(_))
            ));
        }
        conn.recv_event()
            .await
            .expect_err("fourth unknown should fail")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush ready");
    read_init_frame(&mut raw).await;

    let heartbeat = heartbeat_frame();
    for i in 0..4 {
        let unknown = format!(r#"{{"type":"experimental_unsplit_{i}","x":{i}}}"#);
        write_frame(&mut raw, unknown.as_bytes()).await;
        write_frame(&mut raw, &heartbeat).await;
    }
    raw.flush().await.expect("flush mixed stream");
    drop(raw);

    let err = accept_task.await.expect("join");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { count: 4, limit: 3 })
    ));
}

#[tokio::test]
async fn split_reader_rejects_alternating_unknown_and_heartbeat_via_rate_limit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-split-rate.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 0,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 2,
            rate_limit_refill_per_second: 1,
            ignored_field_lifetime_keys: 0,
            ignored_field_lifetime_bytes: 0,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();

        for _ in 0..2 {
            let event = reader.recv_event().await.expect("heartbeat event");
            assert!(matches!(
                event,
                IpcInboundEvent::Message(ContainerToHost::Heartbeat(_))
            ));
        }
        reader
            .recv_event()
            .await
            .expect_err("third unknown should rate-limit")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush ready");
    read_init_frame(&mut raw).await;

    let heartbeat = heartbeat_frame();
    for i in 0..3 {
        let unknown = format!(r#"{{"type":"experimental_split_{i}","x":{i}}}"#);
        write_frame(&mut raw, unknown.as_bytes()).await;
        write_frame(&mut raw, &heartbeat).await;
    }
    raw.flush().await.expect("flush mixed stream");
    drop(raw);

    let err = accept_task.await.expect("join");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::UnknownMessageRateLimitExceeded {
            burst_capacity: 2,
            refill_per_second: 1
        })
    ));
}

// ─────────────────────────────────────────────────────────────────────
// Forward-compat abuse shaping (finding #1)
// ─────────────────────────────────────────────────────────────────────

/// A heartbeat with *any* extra field must be rejected immediately —
/// payload policy is `Reject`, there is no per-frame budget to consume.
#[tokio::test]
async fn heartbeat_with_any_extra_field_is_rejected_immediately() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ignored-heartbeat-reject.sock");
    let server = IpcServer::bind_with_options(&path, IpcServerOptions::insecure_capture_only())
        .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.recv_event().await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush");
    read_init_frame(&mut raw).await;

    let payload = br#"{"type":"heartbeat","timestamp":"2026-04-16T00:00:00Z","junk":"x"}"#;
    write_frame(&mut raw, payload).await;
    raw.flush().await.expect("flush hb");

    let err = accept_task.await.expect("join").expect_err("reject");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::UnknownFieldsRejected {
            message_type: "heartbeat",
            ..
        })
    ));
    drop(raw);
}

/// An `output_delta` with *any* extra field must also be rejected
/// (same policy class as `heartbeat`).
#[tokio::test]
async fn output_delta_with_any_extra_field_is_rejected_immediately() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ignored-delta-reject.sock");
    let server = IpcServer::bind_with_options(&path, IpcServerOptions::insecure_capture_only())
        .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.recv_event().await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush");
    read_init_frame(&mut raw).await;

    let payload = br#"{"type":"output_delta","text":"x","job_id":"job-integration-1","junk":"z"}"#;
    write_frame(&mut raw, payload).await;
    raw.flush().await.expect("flush delta");

    let err = accept_task.await.expect("join").expect_err("reject");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::UnknownFieldsRejected {
            message_type: "output_delta",
            ..
        })
    ));
    drop(raw);
}

/// A ready with extra fields whose *bytes* exceed the per-frame budget
/// trips `IgnoredFieldBudgetExceeded` on the `bytes` dimension.
#[tokio::test]
async fn ready_with_large_extra_field_bytes_exceeds_budget() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ignored-ready-bytes.sock");
    let server = IpcServer::bind_with_options(&path, IpcServerOptions::insecure_capture_only())
        .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    let junk = "x".repeat(8192);
    let ready = format!(
        r#"{{"type":"ready","adapter":"a","adapter_version":"v","protocol_version":"1.0","junk":"{junk}"}}"#
    );
    write_frame(&mut raw, ready.as_bytes()).await;
    raw.flush().await.expect("flush");

    let err = accept_task.await.expect("join").expect_err("ready abuse");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::IgnoredFieldBudgetExceeded {
            message_type: "ready",
            scope: "per_frame",
            dimension: "bytes",
            ..
        })
    ));
    drop(raw);
}

/// Bounded-Budget payloads carrying many small extra fields trip the
/// `keys` dimension.
#[tokio::test]
async fn ready_with_many_extra_keys_exceeds_budget() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ignored-ready-keys.sock");
    let server = IpcServer::bind_with_options(&path, IpcServerOptions::insecure_capture_only())
        .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    let mut extras = String::new();
    for i in 0..17 {
        let _ = write!(extras, r#","ext_{i}":0"#);
    }
    let ready = format!(
        r#"{{"type":"ready","adapter":"a","adapter_version":"v","protocol_version":"1.0"{extras}}}"#
    );
    write_frame(&mut raw, ready.as_bytes()).await;
    raw.flush().await.expect("flush");

    let err = accept_task.await.expect("join").expect_err("ready abuse");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::IgnoredFieldBudgetExceeded {
            message_type: "ready",
            scope: "per_frame",
            dimension: "keys",
            ..
        })
    ));
    drop(raw);
}

/// Lifetime byte cap accumulates across multiple known-type frames
/// even when no single frame trips the per-frame limit.
#[tokio::test]
async fn ignored_field_lifetime_bytes_budget_enforced_across_messages() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ignored-lifetime-bytes.sock");
    // Tiny lifetime byte cap, per-frame wide open.
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 0,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 0,
            rate_limit_refill_per_second: 0,
            ignored_field_lifetime_keys: 0,
            ignored_field_lifetime_bytes: 128,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        // After handshake, read each progress frame — each carries a
        // small unknown field (~20 bytes). The 7th frame pushes total
        // past 128 bytes and trips the lifetime byte cap.
        for _ in 0..10 {
            conn.recv_event().await?;
        }
        Ok::<_, IpcError>(())
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush ready");
    read_init_frame(&mut raw).await;

    // Each progress frame carries a 48-byte unknown value (~50 bytes
    // measured by the roundtrip diff); after frame 3 the lifetime
    // byte total crosses the 128-byte cap.
    for i in 0..10 {
        let junk = "x".repeat(48);
        let payload = format!(
            r#"{{"type":"progress","job_id":"job-integration-1","stage":"s","ext_{i}":"{junk}"}}"#
        );
        write_frame(&mut raw, payload.as_bytes()).await;
    }
    raw.flush().await.expect("flush progress");

    let err = accept_task.await.expect("join").expect_err("lifetime");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::IgnoredFieldBudgetExceeded {
            scope: "lifetime",
            dimension: "bytes",
            ..
        })
    ));
    drop(raw);
}
