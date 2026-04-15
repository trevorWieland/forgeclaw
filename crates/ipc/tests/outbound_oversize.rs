//! Outbound oversize serialization enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    CommandBody, CommandPayload, ContainerToHost, FrameError, GroupCapabilities, GroupExtensions,
    GroupExtensionsVersion, GroupInfo, HistoricalMessage, HistoricalMessages, HostToContainer,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcServer, MAX_FRAME_BYTES,
    MessagesPayload, ReadyPayload, RegisterGroupPayload, ShutdownPayload, ShutdownReason,
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
        let short_name = if name.len() > 32 { &name[..32] } else { name };
        socket_dir.join(short_name)
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
        job_id: JobId::from("job-oversize-1"),
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

fn oversize_container_message() -> ContainerToHost {
    let mut ext =
        GroupExtensions::new(GroupExtensionsVersion::new("1").expect("valid extension version"));
    ext.insert(
        "blob",
        serde_json::Value::String("x".repeat(MAX_FRAME_BYTES + 1_024)),
    )
    .expect("non-reserved extension key");
    ContainerToHost::Command(CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "oversize-group".parse().expect("valid name"),
            extensions: Some(ext),
        }),
    })
}

fn oversize_host_message() -> HostToContainer {
    let large_text = "🦀".repeat(32 * 1024);
    let large_text = large_text.parse().expect("valid max-char text");
    let template = HistoricalMessage {
        sender: "A".parse().expect("valid sender"),
        text: large_text,
        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
    };

    HostToContainer::Messages(MessagesPayload {
        job_id: JobId::from("job-oversize-1"),
        messages: std::iter::repeat_n(template, 90)
            .collect::<Vec<_>>()
            .try_into()
            .expect("messages within bound"),
    })
}

#[tokio::test]
async fn client_unsplit_send_oversize_is_rejected_and_poisoned() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-client-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (_conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    let err = client
        .send(&oversize_container_message())
        .await
        .expect_err("oversize send must fail");
    assert!(
        matches!(
            err,
            IpcError::Frame(FrameError::Oversize {
                max: MAX_FRAME_BYTES,
                ..
            })
        ),
        "expected Oversize, got {err:?}"
    );

    let err = client
        .send(&ContainerToHost::Heartbeat(
            forgeclaw_ipc::HeartbeatPayload {
                timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
            },
        ))
        .await
        .expect_err("poisoned client should be closed");
    assert!(matches!(err, IpcError::Closed));

    accept_task.await.expect("join");
}

#[tokio::test]
async fn client_split_writer_send_oversize_is_rejected_and_poisoned() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-client-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (_conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (mut writer, _reader) = client.into_split();

    let err = writer
        .send(&oversize_container_message())
        .await
        .expect_err("oversize send must fail");
    assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));

    let err = writer
        .send(&ContainerToHost::Heartbeat(
            forgeclaw_ipc::HeartbeatPayload {
                timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
            },
        ))
        .await
        .expect_err("poisoned writer should be closed");
    assert!(matches!(err, IpcError::Closed));

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_unsplit_send_oversize_is_rejected_and_poisoned() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");

        let err = conn
            .send_host(&oversize_host_message())
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));

        let err = conn
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }))
            .await
            .expect_err("poisoned connection should be closed");
        assert!(matches!(err, IpcError::Closed));
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_split_writer_send_oversize_is_rejected_and_poisoned() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, _reader) = conn.into_split();

        let err = writer
            .send_host(&oversize_host_message())
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(err, IpcError::Frame(FrameError::Oversize { .. })));

        let err = writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }))
            .await
            .expect_err("poisoned writer should be closed");
        assert!(matches!(err, IpcError::Closed));
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    accept_task.await.expect("join");
}
