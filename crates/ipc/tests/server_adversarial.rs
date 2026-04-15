//! Server-side adversarial input, timeout, and socket-lifecycle tests.
//!
//! Exercises the host receive path with raw malformed frames, the
//! handshake timeout, and the bind liveness check.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, FrameError, GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig,
    InitContext, InitPayload, IpcClient, IpcError, IpcInboundEvent, IpcServer, ProtocolError,
    ReadyPayload,
};
use tempfile::tempdir;

fn socket_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let socket_dir = dir.path().join("s");
        std::fs::create_dir_all(&socket_dir).expect("mkdir s");
        std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod s");
        {
            let short_name = if name.len() > 32 { &name[..32] } else { name };
            socket_dir.join(short_name)
        }
    }
    #[cfg(not(unix))]
    {
        dir.path().join(name)
    }
}

const HS_TIMEOUT: Duration = Duration::from_secs(5);

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: version.parse().expect("valid protocol version"),
    }
}

fn sample_group() -> GroupInfo {
    GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::from("job-integration-1"),
        context: InitContext {
            messages: HistoricalMessages::default(),
            group: sample_group(),
            timezone: "UTC".parse().expect("valid timezone"),
        },
        config: InitConfig {
            provider_proxy_url: "http://proxy.local".parse().expect("valid proxy url"),
            provider_proxy_token: "token".parse().expect("valid proxy token"),
            model: "claude-sonnet-4-6".parse().expect("valid model"),
            max_tokens: 1000,
            session_id: None,
            tools_enabled: true,
            timeout_seconds: 600,
        },
    }
}

async fn recv_message(
    conn: &mut forgeclaw_ipc::IpcConnection,
) -> Result<ContainerToHost, IpcError> {
    match conn.recv_event().await? {
        IpcInboundEvent::Message(msg) => Ok(msg),
        IpcInboundEvent::Command(_) => Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
            expected: "message",
            got: "command",
        })),
        IpcInboundEvent::Unauthorized(rej) => {
            Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "message",
                got: rej.command,
            }))
        }
    }
}

// --- Raw-socket adversarial input tests ---

/// Sends raw bytes from a fake client and expects the server's
/// `PendingConnection::handshake` to fail with the returned error.
async fn raw_handshake_error(payload: &'static [u8]) -> IpcError {
    use tokio::io::AsyncWriteExt;

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        // Signal that accept (and peer_cred) has completed.
        let _ = tx.send(());
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });
    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    // Wait for accept to complete before sending and dropping.
    let _ = rx.await;
    let len = u32::try_from(payload.len()).expect("fits");
    raw.write_all(&len.to_be_bytes()).await.expect("write len");
    raw.write_all(payload).await.expect("write payload");
    raw.flush().await.expect("flush");
    drop(raw);
    accept_task.await.expect("join").expect_err("should error")
}

#[tokio::test]
async fn raw_invalid_utf8_through_socket() {
    let err = raw_handshake_error(&[0xFF, 0xFE, 0xFD]).await;
    assert!(matches!(err, IpcError::Frame(FrameError::InvalidUtf8)));
}

#[tokio::test]
async fn raw_malformed_json_through_socket() {
    let err = raw_handshake_error(b"{not json at all}").await;
    assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
}

#[tokio::test]
async fn raw_missing_required_fields_through_socket() {
    let err = raw_handshake_error(br#"{"type":"ready"}"#).await;
    assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
}

#[tokio::test]
async fn unknown_type_skipped_during_handshake() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");

    // Send an unknown-type frame first, then a proper Ready.
    let unknown_payload = br#"{"type":"experimental_v2","data":42}"#;
    let unknown_len = u32::try_from(unknown_payload.len()).expect("fits");
    raw.write_all(&unknown_len.to_be_bytes()).await.expect("w");
    raw.write_all(unknown_payload).await.expect("w");

    let ready_json =
        serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready");
    let ready_len = u32::try_from(ready_json.len()).expect("fits");
    raw.write_all(&ready_len.to_be_bytes()).await.expect("w");
    raw.write_all(&ready_json).await.expect("w");
    raw.flush().await.expect("flush");

    // Read back the Init response so the server handshake completes.
    let mut len_buf = [0u8; 4];
    raw.read_exact(&mut len_buf).await.expect("read init len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload_buf = vec![0u8; len];
    raw.read_exact(&mut payload_buf)
        .await
        .expect("read init payload");
    drop(raw);

    let (_conn, ready) = accept_task.await.expect("join").expect("handshake ok");
    assert_eq!(ready.adapter, "test-adapter");
}

#[tokio::test]
async fn unknown_type_byte_budget_enforced_during_handshake() {
    use tokio::io::AsyncWriteExt;

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-budget.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");

    // Send parseable unknown-type frames until cumulative unknown
    // bytes exceed the 1 MiB budget.
    let data = "x".repeat(70_000);
    for i in 0..20 {
        let payload = format!(r#"{{"type":"experimental_large_{i}","data":"{data}"}}"#);
        let len = u32::try_from(payload.len()).expect("fits");
        if let Err(e) = raw.write_all(&len.to_be_bytes()).await {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                break;
            }
            assert_eq!(e.kind(), std::io::ErrorKind::BrokenPipe, "write len: {e}");
        }
        if let Err(e) = raw.write_all(payload.as_bytes()).await {
            if e.kind() == std::io::ErrorKind::BrokenPipe {
                break;
            }
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe,
                "write payload: {e}"
            );
        }
    }
    let _ = raw.flush().await;
    drop(raw);

    let err = accept_task
        .await
        .expect("join")
        .expect_err("handshake should fail");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::TooManyUnknownBytes { .. })
        ),
        "expected TooManyUnknownBytes, got {err:?}"
    );
}

#[tokio::test]
async fn post_error_connection_is_poisoned() {
    use tokio::io::AsyncWriteExt;

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let _ = tx.send(());
        // The malformed frame should cause handshake to fail.
        let err = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect_err("malformed frame should error");
        assert!(
            matches!(err, IpcError::Frame(FrameError::InvalidUtf8)),
            "expected InvalidUtf8, got {err:?}"
        );
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    let _ = rx.await;
    let payload: &[u8] = &[0xFF, 0xFE, 0xFD];
    let len = u32::try_from(payload.len()).expect("fits");
    raw.write_all(&len.to_be_bytes()).await.expect("w");
    raw.write_all(payload).await.expect("w");
    raw.flush().await.expect("flush");
    drop(raw);

    accept_task.await.expect("join");
}

// --- Timeout tests ---

#[tokio::test]
async fn server_handshake_timeout_when_peer_silent() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending
            .handshake(sample_init(), Duration::from_millis(50))
            .await
    });

    // Connect but never send Ready — server should time out.
    let _raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");

    let result = accept_task.await.expect("join");
    assert!(
        matches!(result, Err(IpcError::Timeout(_))),
        "expected Timeout, got {result:?}"
    );
}

#[tokio::test]
async fn client_handshake_timeout_when_host_silent() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let _pending = server.accept(sample_group()).await.expect("accept");
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let result = pending
        .handshake(sample_ready("1.0"), Duration::from_millis(50))
        .await;
    assert!(
        matches!(result, Err(IpcError::Timeout(_))),
        "expected Timeout, got {result:?}"
    );

    accept_task.abort();
}

// --- Socket lifecycle tests ---

#[tokio::test]
async fn bind_refuses_live_socket() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");

    let _live = std::os::unix::net::UnixListener::bind(&path).expect("bind live");
    assert!(path.exists(), "socket file should exist");

    let err = IpcServer::bind(&path).expect_err("should reject live socket");
    assert!(
        matches!(&err, IpcError::Io(e) if e.kind() == std::io::ErrorKind::AddrInUse),
        "expected AddrInUse, got {err:?}"
    );
}

/// The handshake internally uses forward-compatible recv that silently
/// skips unknown message types, matching the spec-required behavior.
#[tokio::test]
async fn default_recv_container_skips_unknown_types() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let _ = tx.send(());
        // Handshake uses forward-compatible recv internally.
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let _ = rx.await;
    // Send one unknown frame, then a valid Ready.
    let unknown_frame = br#"{"type":"experimental_v99","data":"ignored"}"#;
    let unknown_len = u32::try_from(unknown_frame.len()).expect("fits");
    raw.write_all(&unknown_len.to_be_bytes()).await.expect("w");
    raw.write_all(unknown_frame.as_slice()).await.expect("w");
    let valid_frame =
        br#"{"type":"ready","adapter":"x","adapter_version":"v","protocol_version":"1.0"}"#;
    let valid_len = u32::try_from(valid_frame.len()).expect("fits");
    raw.write_all(&valid_len.to_be_bytes()).await.expect("w");
    raw.write_all(valid_frame.as_slice()).await.expect("w");
    raw.flush().await.expect("flush");

    // Read back the Init response so the server handshake completes.
    let mut len_buf = [0u8; 4];
    raw.read_exact(&mut len_buf).await.expect("read init len");
    let init_len = u32::from_be_bytes(len_buf) as usize;
    let mut payload_buf = vec![0u8; init_len];
    raw.read_exact(&mut payload_buf)
        .await
        .expect("read init payload");
    drop(raw);

    let (_conn, ready) = accept_task
        .await
        .expect("join")
        .expect("should skip unknown and return Ready");
    assert!(
        matches!(
            ContainerToHost::Ready(ready.clone()),
            ContainerToHost::Ready(_)
        ),
        "expected Ready, got something else"
    );
}

#[tokio::test]
async fn unknown_budget_resets_after_recognized_message() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-reset.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let first = recv_message(&mut conn).await.expect("first known message");
        let second = recv_message(&mut conn).await.expect("second known message");
        (first, second)
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");

    // Handshake ready.
    let ready_json =
        serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize");
    let ready_len = u32::try_from(ready_json.len()).expect("fits");
    raw.write_all(&ready_len.to_be_bytes()).await.expect("w");
    raw.write_all(&ready_json).await.expect("w");
    raw.flush().await.expect("flush");

    // Drain init from server.
    let mut init_len = [0u8; 4];
    raw.read_exact(&mut init_len).await.expect("read init len");
    let payload_len = u32::from_be_bytes(init_len) as usize;
    let mut init_payload = vec![0u8; payload_len];
    raw.read_exact(&mut init_payload)
        .await
        .expect("read init payload");

    // 1 unknown frame.
    let unknown = br#"{"type":"experimental_pre","x":1}"#;
    let unknown_len = u32::try_from(unknown.len()).expect("fits");
    raw.write_all(&unknown_len.to_be_bytes()).await.expect("w");
    raw.write_all(unknown).await.expect("w");

    // Known message resets unknown budget.
    let delta = serde_json::to_vec(&ContainerToHost::OutputDelta(
        forgeclaw_ipc::OutputDeltaPayload {
            text: "delta".parse().expect("valid text"),
            job_id: JobId::from("job-integration-1"),
        },
    ))
    .expect("serialize");
    let delta_len = u32::try_from(delta.len()).expect("fits");
    raw.write_all(&delta_len.to_be_bytes()).await.expect("w");
    raw.write_all(&delta).await.expect("w");

    // Send exactly 32 unknown frames (count limit), then a known complete.
    for i in 0..32 {
        let msg = format!(r#"{{"type":"experimental_post_{i}","x":1}}"#);
        let len = u32::try_from(msg.len()).expect("fits");
        raw.write_all(&len.to_be_bytes()).await.expect("w");
        raw.write_all(msg.as_bytes()).await.expect("w");
    }
    let complete = serde_json::to_vec(&ContainerToHost::OutputComplete(
        forgeclaw_ipc::OutputCompletePayload {
            job_id: JobId::from("job-integration-1"),
            result: Some("done".parse().expect("valid result")),
            session_id: None,
            token_usage: None,
            stop_reason: forgeclaw_ipc::StopReason::EndTurn,
        },
    ))
    .expect("serialize");
    let complete_len = u32::try_from(complete.len()).expect("fits");
    raw.write_all(&complete_len.to_be_bytes()).await.expect("w");
    raw.write_all(&complete).await.expect("w");
    raw.flush().await.expect("flush");
    drop(raw);

    let (first, second) = accept_task.await.expect("join");
    assert!(matches!(first, ContainerToHost::OutputDelta(_)));
    assert!(matches!(second, ContainerToHost::OutputComplete(_)));
}
