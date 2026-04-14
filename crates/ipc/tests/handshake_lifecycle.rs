//! End-to-end lifecycle tests over real Unix sockets.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HeartbeatPayload, HistoricalMessage,
    HostToContainer, InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcServer,
    MessagesPayload, OutputCompletePayload, ProtocolError, ReadyPayload, ShutdownPayload,
    ShutdownReason, StopReason,
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

const HS_TIMEOUT: Duration = Duration::from_secs(5);

fn sample_group() -> GroupInfo {
    GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main".to_owned(),
        is_main: true,
        capabilities: GroupCapabilities::default(),
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
                capabilities: GroupCapabilities::default(),
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
            let pending = server.accept(sample_group()).await.expect("accept");
            let (conn, ready) = pending
                .handshake(sample_init(), HS_TIMEOUT)
                .await
                .expect("server handshake");
            (conn, ready)
        })
    };

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
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
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let client_pending = IpcClient::connect(&path).await.expect("connect");
    // Client sends Ready with unsupported version "2.0"; the server's
    // handshake will fail with UnsupportedVersion.
    let _ = client_pending
        .handshake(sample_ready("2.0"), HS_TIMEOUT)
        .await;

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
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
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

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
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
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.recv_container().await.expect("recv heartbeat")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
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

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        // Signal that accept (and peer_cred) has completed.
        let _ = tx.send(());
        pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .map(|(_conn, ready)| ready)
    });

    // Connect, wait for accept to capture credentials, then drop.
    let client = IpcClient::connect(&path).await.expect("connect");
    let _ = rx.await;
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

#[tokio::test]
async fn messages_exchange_after_init() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
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

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
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
async fn session_identity_survives_split_with_group_binding() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        // Group should be bound after handshake.
        {
            let id = conn.identity().lock().expect("lock");
            let group = id.group();
            assert!(group.is_main, "should be main group");
            assert!(id.is_main());
        }
        // Split and verify identity survives on both halves.
        let (writer, reader) = conn.into_split();
        {
            let id = writer.identity().lock().expect("lock");
            assert_eq!(id.group().id.as_ref(), "group-main");
        }
        {
            let id = reader.identity().lock().expect("lock");
            assert_eq!(id.group().id.as_ref(), "group-main");
        }
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn handshake_rejects_group_mismatch() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    // init.context.group has a different ID than the group parameter.
    let mut init = sample_init();
    init.context.group = GroupInfo {
        id: GroupId::from("group-other"),
        name: "Other".to_owned(),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let session_group = sample_group(); // group-main

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(session_group).await.expect("accept");
        pending.handshake(init, HS_TIMEOUT).await
    });

    let client_pending = IpcClient::connect(&path).await.expect("connect");
    // Client handshake will fail because the server rejects after
    // detecting the mismatch, poisoning the connection.
    let _ = client_pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await;

    let err = accept_task
        .await
        .expect("join")
        .expect_err("should reject mismatch");
    assert!(
        matches!(err, IpcError::Protocol(ProtocolError::GroupMismatch { .. })),
        "expected GroupMismatch, got {err:?}"
    );
}

#[tokio::test]
async fn handshake_overwrites_same_id_different_metadata_from_session_identity() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-meta-drift.sock");
    let server = IpcServer::bind(&path).expect("bind");

    // Session group has one name and capabilities...
    let session_group = GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main".to_owned(),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };

    // ...but init carries the same ID with different metadata.
    let mut init = sample_init();
    init.context.group = GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main (Updated)".to_owned(),
        is_main: false,
        capabilities: GroupCapabilities { tanren: true },
    };

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(session_group).await.expect("accept");
        pending.handshake(init, HS_TIMEOUT).await
    });

    let client_pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, received_init) = client_pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    let result = accept_task.await.expect("join");
    assert!(
        result.is_ok(),
        "same group ID with different metadata should succeed, got {result:?}"
    );
    assert_eq!(received_init.context.group.name, "Main");
    assert!(received_init.context.group.is_main);
    assert_eq!(
        received_init.context.group.capabilities,
        GroupCapabilities::default(),
    );
}
