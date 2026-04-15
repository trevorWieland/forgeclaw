//! Adversarial tests targeting the **client** receive path.
//!
//! These mirror the server-side adversarial coverage in
//! `handshake_lifecycle.rs` and `malformed_frames.rs`, exercising
//! the client's `recv`, `recv_with_policy(Strict)`, and split-mode teardown
//! against raw malformed frames sent by a fake host.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, FrameError, GroupCapabilities, GroupInfo, HistoricalMessages, HostToContainer,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, MAX_FRAME_BYTES, ProtocolError,
    ReadyPayload, ShutdownPayload, ShutdownReason, UnknownTypePolicy,
};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const HS_TIMEOUT: Duration = Duration::from_secs(5);

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

fn sample_ready() -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::new("job-adv-1").expect("valid job id"),
        context: InitContext {
            messages: HistoricalMessages::default(),
            group: GroupInfo {
                id: GroupId::new("group-main").expect("valid group id"),
                name: "Main".parse().expect("valid name"),
                is_main: true,
                capabilities: GroupCapabilities::default(),
            },
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

// --- Raw frame helpers ---

async fn raw_write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("payload fits in u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

async fn raw_read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.expect("read payload");
    buf
}

/// Accept a raw connection as a fake host: read the Ready frame,
/// send back an Init frame, then return the raw stream for further
/// adversarial use.
async fn fake_host_handshake(listener: &tokio::net::UnixListener) -> tokio::net::UnixStream {
    let (mut stream, _) = listener.accept().await.expect("accept");
    // Read the client's Ready frame (we don't need to parse it).
    let _ready_bytes = raw_read_frame(&mut stream).await;
    // Send the Init frame.
    let init_json =
        serde_json::to_vec(&HostToContainer::Init(sample_init())).expect("serialize init");
    raw_write_frame(&mut stream, &init_json).await;
    stream
}

// --- Tests ---

#[tokio::test]
async fn client_recv_invalid_utf8() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        raw_write_frame(&mut stream, &[0xFF, 0xFE, 0xFD]).await;
        // Keep stream alive so client doesn't see EOF first.
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should error");
    assert!(
        matches!(err, IpcError::Frame(FrameError::InvalidUtf8)),
        "expected InvalidUtf8, got {err:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_recv_malformed_json() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        raw_write_frame(&mut stream, b"{not json at all}").await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should error");
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "expected MalformedJson, got {err:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_recv_unknown_type_skip_limit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // Send 33 unknown-type frames (limit is 32).
        for i in 0..33 {
            let payload = format!(r#"{{"type":"unknown_adv_{i}","data":null}}"#);
            raw_write_frame(&mut stream, payload.as_bytes()).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client.recv().await.expect_err("should poison");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::TooManyUnknownMessages {
                count: 33,
                limit: 32
            })
        ),
        "expected TooManyUnknownMessages, got {err:?}"
    );
    // Connection should be poisoned — subsequent recv returns Closed.
    let err2 = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should be closed");
    assert!(
        matches!(err2, IpcError::Closed),
        "expected Closed, got {err2:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_recv_unknown_type_byte_budget_limit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-byte-budget.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // Exceed the unknown-byte budget across many parseable unknown
        // frames so classification remains UnknownMessageType.
        for i in 0..20 {
            let payload = format!(
                r#"{{"type":"unknown_adv_large_{i}","data":"{}"}}"#,
                "x".repeat(60_000)
            );
            raw_write_frame(&mut stream, payload.as_bytes()).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client.recv().await.expect_err("should poison");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::TooManyUnknownBytes { .. })
        ),
        "expected TooManyUnknownBytes, got {err:?}"
    );
    let err2 = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should be closed");
    assert!(matches!(err2, IpcError::Closed));

    host_task.abort();
}

#[tokio::test]
async fn client_recv_unknown_then_valid() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // One unknown frame, then a valid Shutdown.
        let unknown = br#"{"type":"experimental_v99","data":"ignored"}"#;
        raw_write_frame(&mut stream, unknown).await;
        let shutdown = serde_json::to_vec(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        }))
        .expect("serialize");
        raw_write_frame(&mut stream, &shutdown).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let msg = client.recv().await.expect("should skip unknown");
    assert!(
        matches!(
            msg,
            HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                ..
            })
        ),
        "expected Shutdown, got {msg:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_split_reader_error_shuts_down_writer() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // Send invalid UTF-8 to trigger a fatal error on the reader.
        raw_write_frame(&mut stream, &[0xFF, 0xFE]).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (mut writer, mut reader) = client.into_split();

    // Reader gets fatal error.
    let err = reader
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should error");
    assert!(
        matches!(err, IpcError::Frame(FrameError::InvalidUtf8)),
        "expected InvalidUtf8, got {err:?}"
    );

    // Writer should be poisoned — send returns Closed.
    let send_result = writer
        .send(&ContainerToHost::Heartbeat(
            forgeclaw_ipc::HeartbeatPayload {
                timestamp: "2026-04-13T00:00:00Z".parse().expect("valid timestamp"),
            },
        ))
        .await;
    assert!(
        matches!(send_result, Err(IpcError::Closed)),
        "expected Closed on poisoned writer, got {send_result:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_recv_oversize_frame() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // Write a length prefix exceeding MAX_FRAME_BYTES.
        let declared_size = u32::try_from(MAX_FRAME_BYTES + 1).expect("fits");
        stream
            .write_all(&declared_size.to_be_bytes())
            .await
            .expect("write len");
        // Don't need to send the full payload — codec rejects on header.
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should error");
    assert!(
        matches!(err, IpcError::Frame(FrameError::Oversize { .. })),
        "expected Oversize, got {err:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_recv_truncated_frame() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        // Declare 100 bytes, send only 10, then close.
        let declared: u32 = 100;
        stream
            .write_all(&declared.to_be_bytes())
            .await
            .expect("write len");
        stream.write_all(&[b'x'; 10]).await.expect("write partial");
        drop(stream);
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let err = client
        .recv_with_policy(UnknownTypePolicy::Strict)
        .await
        .expect_err("should error");
    assert!(
        matches!(
            err,
            IpcError::Frame(FrameError::Truncated {
                expected: 100,
                got: 10
            })
        ),
        "expected Truncated(100, 10), got {err:?}"
    );

    host_task.await.expect("join");
}
