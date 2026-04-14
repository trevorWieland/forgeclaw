//! Lifecycle phase enforcement tests for post-handshake traffic.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessage, HostToContainer, InitConfig,
    InitContext, InitPayload, IpcClient, IpcError, IpcServer, MessagesPayload,
    OutputCompletePayload, ProtocolError, ReadyPayload, ShutdownPayload, ShutdownReason,
    StopReason,
};
use tempfile::tempdir;

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

#[tokio::test]
async fn lifecycle_rejects_output_delta_after_idle_without_messages() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-lifecycle-idle.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let first = conn.recv_container().await.expect("first message");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        let _ = tx.send(());
        conn.recv_container().await.expect_err("second should fail")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-integration-1"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");
    let _ = rx.await;
    client
        .send(&ContainerToHost::OutputDelta(
            forgeclaw_ipc::OutputDeltaPayload {
                text: "late delta".to_owned(),
                job_id: JobId::from("job-integration-1"),
            },
        ))
        .await
        .expect("send late delta");

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected lifecycle violation, got {err:?}"
    );
}

#[tokio::test]
async fn lifecycle_rejects_messages_after_shutdown() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-lifecycle-shutdown.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        }))
        .await
        .expect("send shutdown");
        conn.send_host(&HostToContainer::Messages(MessagesPayload {
            job_id: JobId::from("job-integration-1"),
            messages: vec![HistoricalMessage {
                sender: "Alice".to_owned(),
                text: "follow-up".to_owned(),
                timestamp: "2026-04-03T10:05:00Z".to_owned(),
            }],
        }))
        .await
        .expect_err("messages after shutdown should fail")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let msg = client.recv().await.expect("recv shutdown");
    assert!(matches!(msg, HostToContainer::Shutdown(_)));

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected lifecycle violation, got {err:?}"
    );
}

#[tokio::test]
async fn lifecycle_rejects_repeated_output_complete_while_draining() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-lifecycle-draining-complete.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        }))
        .await
        .expect("send shutdown");
        let first = conn.recv_container().await.expect("first complete");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        conn.recv_container()
            .await
            .expect_err("second complete should fail")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let msg = client.recv().await.expect("recv shutdown");
    assert!(matches!(msg, HostToContainer::Shutdown(_)));
    for _ in 0..2 {
        client
            .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
                job_id: JobId::from("job-integration-1"),
                result: None,
                session_id: None,
                token_usage: None,
                stop_reason: StopReason::EndTurn,
            }))
            .await
            .expect("send complete");
    }

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected lifecycle violation, got {err:?}"
    );
}
