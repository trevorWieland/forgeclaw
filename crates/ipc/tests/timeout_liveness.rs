//! Timeout and liveness enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HostToContainer, InitConfig, InitContext,
    InitPayload, IpcClient, IpcError, IpcServer, ProtocolError, ReadyPayload, ShutdownPayload,
    ShutdownReason,
};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;

const HS_TIMEOUT: Duration = Duration::from_secs(5);

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
            group: sample_group(),
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

fn oversized_init() -> InitPayload {
    let mut init = sample_init();
    // Large enough to force backpressure if peer stops reading.
    init.config.provider_proxy_token = "x".repeat(9_000_000);
    init
}

async fn raw_send_message(stream: &mut tokio::net::UnixStream, msg: &ContainerToHost) {
    let payload = serde_json::to_vec(msg).expect("serialize");
    let len = u32::try_from(payload.len()).expect("fits");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(&payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

#[tokio::test]
async fn server_handshake_timeout_covers_init_send() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-handshake-send-timeout.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending
            .handshake(oversized_init(), Duration::from_millis(50))
            .await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready("1.0"))).await;
    // Peer does not read Init, forcing host send-side backpressure.
    tokio::time::sleep(Duration::from_millis(150)).await;
    drop(raw);

    let result = accept_task.await.expect("join");
    assert!(
        matches!(result, Err(IpcError::Timeout(_))),
        "expected Timeout, got {result:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn heartbeat_timeout_during_processing_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-heartbeat-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let err = conn.recv_container().await.expect_err("should timeout");
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1_000,
        }))
        .await
        .expect("shutdown should still be sendable");
        err
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::HeartbeatTimeout {
                phase: "processing",
                timeout_secs: 60
            })
        ),
        "expected HeartbeatTimeout, got {err:?}"
    );
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));
}

#[tokio::test(start_paused = true)]
async fn heartbeat_timeout_during_processing_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-heartbeat-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, mut reader) = conn.into_split();
        let err = reader.recv_container().await.expect_err("should timeout");
        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }))
            .await
            .expect("shutdown should still be sendable");
        err
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::HeartbeatTimeout {
                phase: "processing",
                timeout_secs: 60
            })
        ),
        "expected HeartbeatTimeout, got {err:?}"
    );
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));
}
