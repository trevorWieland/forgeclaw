//! End-to-end lifecycle tests over real Unix sockets.

use std::path::PathBuf;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, FrameError, GroupInfo, HeartbeatPayload, HistoricalMessage, HostToContainer,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcServer, MessagesPayload,
    OutputCompletePayload, ProtocolError, ReadyPayload, ShutdownPayload, ShutdownReason,
    StopReason,
};
use tempfile::tempdir;

fn socket_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".to_owned(),
        adapter_version: "0.1.0".to_owned(),
        protocol_version: version.to_owned(),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::from("job-integration-1"),
        context: InitContext {
            messages: vec![],
            group: GroupInfo {
                id: GroupId::from("group-main"),
                name: "Main".to_owned(),
                is_main: true,
            },
            timezone: "UTC".to_owned(),
        },
        config: InitConfig {
            provider_proxy_url: "http://proxy.local".to_owned(),
            provider_proxy_token: "token".to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            max_tokens: 1000,
            session_id: None,
            tools_enabled: true,
            timeout_seconds: 600,
        },
    }
}

#[tokio::test]
async fn handshake_happy_path() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");
    assert_eq!(server.path(), path.as_path());

    let accept_task = {
        let server = server;
        tokio::spawn(async move {
            let mut conn = server.accept().await.expect("accept");
            let ready = conn
                .handshake(sample_init())
                .await
                .expect("server handshake");
            (conn, ready)
        })
    };

    let mut client = IpcClient::connect(&path).await.expect("connect");
    let init = client
        .handshake(sample_ready("1.0"))
        .await
        .expect("client handshake");

    assert_eq!(init, sample_init());

    let (_conn, ready) = accept_task.await.expect("join");
    assert_eq!(ready, sample_ready("1.0"));
}

#[tokio::test]
async fn handshake_rejects_mismatched_major_version() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        conn.handshake(sample_init()).await
    });

    let mut client = IpcClient::connect(&path).await.expect("connect");
    // Client sends Ready first; the server's handshake will fail
    // with UnsupportedVersion before it ever sends Init.
    client
        .send(&ContainerToHost::Ready(sample_ready("2.0")))
        .await
        .expect("client send ready");

    let server_result = accept_task.await.expect("join");
    let err = server_result.expect_err("handshake rejected");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnsupportedVersion { ref peer, .. })
                if peer == "2.0"
        ),
        "expected UnsupportedVersion, got {err:?}"
    );
}

#[tokio::test]
async fn full_duplex_exchange_after_handshake() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn
            .handshake(sample_init())
            .await
            .expect("server handshake");
        // Host sends a shutdown, then receives an output_complete.
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        }))
        .await
        .expect("send shutdown");
        conn.recv_container().await.expect("recv last msg")
    });

    let mut client = IpcClient::connect(&path).await.expect("connect");
    let _init = client
        .handshake(sample_ready("1.0"))
        .await
        .expect("client handshake");
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(
        shutdown,
        HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        })
    ));

    // Client reports completion and disconnects.
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-integration-1"),
            result: Some("done".to_owned()),
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");

    let last = accept_task.await.expect("join");
    assert!(matches!(
        last,
        ContainerToHost::OutputComplete(OutputCompletePayload { ref result, .. })
            if result.as_deref() == Some("done")
    ));
}

#[tokio::test]
async fn heartbeat_exchange_after_handshake() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn.handshake(sample_init()).await.expect("handshake");
        conn.recv_container().await.expect("recv heartbeat")
    });

    let mut client = IpcClient::connect(&path).await.expect("connect");
    let _init = client
        .handshake(sample_ready("1.0"))
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::Heartbeat(HeartbeatPayload {
            timestamp: "2026-04-11T00:00:00Z".to_owned(),
        }))
        .await
        .expect("send heartbeat");

    let received = accept_task.await.expect("join");
    assert!(matches!(received, ContainerToHost::Heartbeat(_)));
}

#[tokio::test]
async fn peer_disconnect_before_handshake_reports_closed() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        conn.recv_container().await
    });

    // Open and immediately drop the client socket.
    let client = IpcClient::connect(&path).await.expect("connect");
    drop(client);

    let result = accept_task.await.expect("join");
    assert!(
        matches!(result, Err(IpcError::Closed)),
        "expected IpcError::Closed, got {result:?}"
    );
}

#[tokio::test]
async fn drop_unlinks_socket_file() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    {
        let _server = IpcServer::bind(&path).expect("bind");
        assert!(path.exists(), "socket file should exist after bind");
    }
    assert!(
        !path.exists(),
        "socket file should be removed when server is dropped"
    );
}

#[tokio::test]
async fn rebind_over_stale_socket_file_succeeds() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    // Create a real Unix socket to simulate a previous process crash.
    {
        let _old = std::os::unix::net::UnixListener::bind(&path).expect("bind old");
        // Drop the listener but leave the socket file behind.
    }
    assert!(path.exists(), "stale socket file should exist");

    // Bind should unlink the stale socket and proceed.
    let server = IpcServer::bind(&path).expect("bind over stale socket");
    assert_eq!(server.path(), path.as_path());
}

#[tokio::test]
async fn bind_refuses_regular_file_at_path() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    std::fs::write(&path, b"not a socket").expect("write stub");
    assert!(path.exists());

    let err = IpcServer::bind(&path).expect_err("bind should reject regular file");
    assert!(
        matches!(&err, IpcError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists),
        "expected AlreadyExists error, got {err:?}"
    );
}

// --- Raw-socket adversarial input tests ---

/// Write a length-prefixed frame to a raw socket.
fn raw_send(stream: &mut std::os::unix::net::UnixStream, payload: &[u8]) {
    use std::io::Write;
    let len = u32::try_from(payload.len()).expect("payload fits in u32");
    stream.write_all(&len.to_be_bytes()).expect("write len");
    stream.write_all(payload).expect("write payload");
    stream.flush().expect("flush");
}

async fn raw_recv_error(payload: &'static [u8]) -> IpcError {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        conn.recv_container().await
    });
    let mut raw = std::os::unix::net::UnixStream::connect(&path).expect("raw connect");
    raw_send(&mut raw, payload);
    drop(raw);
    accept_task.await.expect("join").expect_err("should error")
}

#[tokio::test]
async fn raw_invalid_utf8_through_socket() {
    let err = raw_recv_error(&[0xFF, 0xFE, 0xFD]).await;
    assert!(matches!(err, IpcError::Frame(FrameError::InvalidUtf8)));
}

#[tokio::test]
async fn raw_malformed_json_through_socket() {
    let err = raw_recv_error(b"{not json at all}").await;
    assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
}

#[tokio::test]
async fn raw_missing_required_fields_through_socket() {
    let err = raw_recv_error(br#"{"type":"ready"}"#).await;
    assert!(matches!(err, IpcError::Frame(FrameError::MalformedJson(_))));
}

#[tokio::test]
async fn unknown_type_skipped_during_handshake() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        conn.handshake(sample_init()).await
    });

    // Use async tokio UnixStream so we don't block the executor.
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

    // Server handshake should have succeeded (skipping the unknown message).
    let ready = accept_task.await.expect("join").expect("handshake ok");
    assert_eq!(ready.adapter, "test-adapter");
}

#[tokio::test]
async fn messages_exchange_after_init() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn
            .handshake(sample_init())
            .await
            .expect("server handshake");
        // Host sends a follow-up Messages payload.
        conn.send_host(&HostToContainer::Messages(MessagesPayload {
            job_id: JobId::from("job-integration-1"),
            messages: vec![HistoricalMessage {
                sender: "Alice".to_owned(),
                text: "Also check the logs".to_owned(),
                timestamp: "2026-04-03T10:05:00Z".to_owned(),
            }],
        }))
        .await
        .expect("send messages");
        conn.recv_container().await.expect("recv complete")
    });

    let mut client = IpcClient::connect(&path).await.expect("connect");
    let _init = client
        .handshake(sample_ready("1.0"))
        .await
        .expect("client handshake");
    let msg = client.recv().await.expect("recv messages");
    assert!(matches!(msg, HostToContainer::Messages(_)));

    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-integration-1"),
            result: Some("done".to_owned()),
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");

    let last = accept_task.await.expect("join");
    assert!(matches!(last, ContainerToHost::OutputComplete(_)));
}

#[tokio::test]
async fn post_error_connection_is_poisoned() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        // First recv gets a malformed frame → connection poisoned.
        let err1 = conn.recv_container().await;
        assert!(err1.is_err(), "malformed frame should error");
        // Second recv should immediately return Closed (not garbage).
        let err2 = conn.recv_container().await;
        assert!(
            matches!(err2, Err(IpcError::Closed)),
            "poisoned connection should return Closed, got {err2:?}"
        );
    });

    let mut raw = std::os::unix::net::UnixStream::connect(&path).expect("raw connect");
    raw_send(&mut raw, &[0xFF, 0xFE, 0xFD]); // invalid UTF-8
    // Write a valid-looking frame that should never be processed.
    let valid = br#"{"type":"ready","adapter":"x","adapter_version":"v","protocol_version":"1.0"}"#;
    raw_send(&mut raw, valid);
    drop(raw);

    accept_task.await.expect("join");
}
