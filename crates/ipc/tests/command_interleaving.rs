//! Command receive behavior under legal message interleaving.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    AuthorizedCommand, CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo,
    HistoricalMessages, InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcInboundEvent,
    IpcServer, OutputDeltaPayload, ProtocolError, ReadyPayload, SendMessagePayload,
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

fn sample_ready() -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
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
        job_id: JobId::from("job-interleave-1"),
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

fn sample_command() -> ContainerToHost {
    ContainerToHost::Command(CommandPayload {
        body: CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::from("group-main"),
            text: "hello".parse().expect("valid text"),
        }),
    })
}

fn sample_delta() -> ContainerToHost {
    ContainerToHost::OutputDelta(OutputDeltaPayload {
        text: "chunk".parse().expect("valid text"),
        job_id: JobId::from("job-interleave-1"),
    })
}

#[tokio::test]
async fn recv_command_returns_not_command_then_accepts_followup_command() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-not-command.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");

        let first = conn
            .recv_command()
            .await
            .expect_err("output_delta should surface as NotCommand");
        assert!(
            matches!(
                first,
                IpcError::Protocol(ProtocolError::NotCommand {
                    got: "output_delta"
                })
            ),
            "expected NotCommand(output_delta), got {first:?}"
        );

        let second = conn
            .recv_command()
            .await
            .expect("follow-up command should still be readable");
        assert!(
            matches!(second, AuthorizedCommand::Scoped(_)),
            "expected scoped command, got {second:?}"
        );
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&sample_delta()).await.expect("send delta");
    client.send(&sample_command()).await.expect("send command");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn recv_event_yields_message_then_command() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-recv-event.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");

        let first = conn.recv_event().await.expect("first event");
        assert!(
            matches!(
                first,
                IpcInboundEvent::Message(ContainerToHost::OutputDelta(_))
            ),
            "expected output_delta message event, got {first:?}"
        );

        let second = conn.recv_event().await.expect("second event");
        assert!(
            matches!(
                second,
                IpcInboundEvent::Command(AuthorizedCommand::Scoped(_))
            ),
            "expected authorized command event, got {second:?}"
        );
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&sample_delta()).await.expect("send delta");
    client.send(&sample_command()).await.expect("send command");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn recv_command_strict_returns_unexpected_message_for_non_command() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-recv-command-strict.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.recv_command_strict().await
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&sample_delta()).await.expect("send delta");

    let err = accept_task
        .await
        .expect("join")
        .expect_err("strict command receive should reject non-command");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "command",
                got: "output_delta"
            })
        ),
        "expected UnexpectedMessage(command, output_delta), got {err:?}"
    );
}

#[tokio::test]
async fn split_reader_recv_event_yields_message_then_command() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-split-recv-event.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();

        let first = reader.recv_event().await.expect("first event");
        assert!(
            matches!(
                first,
                IpcInboundEvent::Message(ContainerToHost::OutputDelta(_))
            ),
            "expected output_delta message event, got {first:?}"
        );

        let second = reader.recv_event().await.expect("second event");
        assert!(
            matches!(
                second,
                IpcInboundEvent::Command(AuthorizedCommand::Scoped(_))
            ),
            "expected authorized command event, got {second:?}"
        );
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&sample_delta()).await.expect("send delta");
    client.send(&sample_command()).await.expect("send command");

    accept_task.await.expect("join");
}
