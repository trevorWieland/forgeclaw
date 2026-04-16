//! End-to-end lifecycle tests over real Unix sockets.

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HeartbeatPayload, HistoricalMessage,
    HostToContainer, IpcClient, IpcError, IpcInboundEvent, IpcServer, MessagesPayload,
    OutputCompletePayload, ProtocolError, ShutdownPayload, ShutdownReason, StopReason,
};
use tempfile::tempdir;

#[path = "common/mod.rs"]
mod common;
use common::{HS_TIMEOUT, sample_group, sample_init, sample_ready, socket_path};

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
async fn handshake_accepts_previous_minor_version() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-prev-minor.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (_conn, ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        ready
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    let ready = accept_task.await.expect("join");
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
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        }))
        .await
        .expect("send shutdown");
        recv_message(&mut conn).await.expect("recv last msg")
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

    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: Some("done".parse().expect("valid result")),
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
        recv_message(&mut conn).await.expect("recv heartbeat")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::Heartbeat(HeartbeatPayload {
            timestamp: "2026-04-11T00:00:00Z".parse().expect("valid timestamp"),
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
        let _ = tx.send(());
        pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .map(|(_conn, ready)| ready)
    });

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
    {
        let _old = std::os::unix::net::UnixListener::bind(&path).expect("bind old");
    }
    assert!(path.exists(), "stale socket file should exist");

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
        conn.send_host(&HostToContainer::Messages(MessagesPayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            messages: vec![HistoricalMessage {
                sender: "Alice".parse().expect("valid sender"),
                text: "Also check the logs".parse().expect("valid text"),
                timestamp: "2026-04-03T10:05:00Z".parse().expect("valid timestamp"),
            }]
            .try_into()
            .expect("messages within bound"),
        }))
        .await
        .expect("send messages");
        recv_message(&mut conn).await.expect("recv complete")
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
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: Some("done".parse().expect("valid result")),
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
        {
            let id = conn.identity();
            let group = id.group();
            assert!(group.is_main, "should be main group");
            assert!(id.is_main());
        }
        let (writer, reader) = conn.into_split();
        {
            let id = writer.identity();
            assert_eq!(id.group().id.as_ref(), "group-main");
        }
        {
            let id = reader.identity();
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

    let mut init = sample_init();
    init.context.group = GroupInfo {
        id: GroupId::new("group-other").expect("valid group id"),
        name: "Other".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let session_group = sample_group();

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(session_group).await.expect("accept");
        pending.handshake(init, HS_TIMEOUT).await
    });

    let client_pending = IpcClient::connect(&path).await.expect("connect");
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

    let session_group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };

    let mut init = sample_init();
    init.context.group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main (Updated)".parse().expect("valid name"),
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
