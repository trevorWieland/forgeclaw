//! Lifecycle phase enforcement tests for post-handshake traffic.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessage,
    HistoricalMessages, HostToContainer, InitConfig, InitContext, InitPayload, IpcClient, IpcError,
    IpcInboundEvent, IpcServer, MessagesPayload, OutputCompletePayload, ProtocolError,
    ReadyPayload, SendMessagePayload, ShutdownPayload, ShutdownReason, StopReason,
};
use tempfile::tempdir;

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

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: version.parse().expect("valid protocol version"),
    }
}

fn sample_group() -> GroupInfo {
    GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::new("job-integration-1").expect("valid job id"),
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
        let first = recv_message(&mut conn).await.expect("first message");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        let _ = tx.send(());
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
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
                text: "late delta".parse().expect("valid text"),
                job_id: JobId::new("job-integration-1").expect("valid job id"),
            },
        ))
        .await
        .expect_err("send late delta should fail client-side");

    accept_task.await.expect("join");
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
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            messages: vec![HistoricalMessage {
                sender: "Alice".parse().expect("valid sender"),
                text: "follow-up".parse().expect("valid text"),
                timestamp: "2026-04-03T10:05:00Z".parse().expect("valid timestamp"),
            }]
            .try_into()
            .expect("messages within bound"),
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
async fn lifecycle_rejects_command_after_output_complete_until_messages() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-lifecycle-idle-command.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let first = recv_message(&mut conn).await.expect("first message");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");
    client
        .send(&ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::new("group-main").expect("valid group id"),
                text: "late command".parse().expect("valid text"),
            }),
        }))
        .await
        .expect_err("send command should fail client-side");

    accept_task.await.expect("join");
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
        let first = recv_message(&mut conn).await.expect("first complete");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let msg = client.recv().await.expect("recv shutdown");
    assert!(matches!(msg, HostToContainer::Shutdown(_)));
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("first completion");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect_err("second completion should fail client-side");

    accept_task.await.expect("join");
}
