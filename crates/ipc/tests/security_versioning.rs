//! Security regression tests for safe-default receive APIs and
//! negotiated minor-version behavior.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    AuthorizedCommand, CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo,
    HistoricalMessages, InitConfig, InitContext, InitPayload, IpcClient, IpcInboundEvent,
    IpcServer, ReadyPayload, RegisterGroupPayload, ScopedAuthorizedCommand, SendMessagePayload,
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

fn sample_init(group: &GroupInfo) -> InitPayload {
    InitPayload {
        job_id: JobId::from("job-auth-1"),
        context: InitContext {
            messages: HistoricalMessages::default(),
            group: group.clone(),
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

async fn setup_unauthorized_then_authorized_event_test(
    path: &std::path::Path,
    group: GroupInfo,
    split: bool,
) -> (IpcInboundEvent, IpcInboundEvent) {
    let server = IpcServer::bind(path).expect("bind");
    let init = sample_init(&group);
    let path_clone = path.to_path_buf();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(init, HS_TIMEOUT)
            .await
            .expect("handshake");
        if split {
            let (_writer, mut reader) = conn.into_split();
            let first = reader.recv_event().await.expect("first event");
            let second = reader.recv_event().await.expect("second event");
            (first, second)
        } else {
            let first = conn.recv_event().await.expect("first event");
            let second = conn.recv_event().await.expect("second event");
            (first, second)
        }
    });

    let pending_client = IpcClient::connect(&path_clone).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::Command(CommandPayload {
            body: CommandBody::RegisterGroup(RegisterGroupPayload {
                name: "new-group".parse().expect("valid name"),
                extensions: None,
            }),
        }))
        .await
        .expect("send unauthorized");
    client
        .send(&ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::from("group-worker"),
                text: "allowed".parse().expect("valid message"),
            }),
        }))
        .await
        .expect("send authorized");
    drop(client);

    accept_task.await.expect("join")
}

#[tokio::test]
async fn default_recv_event_surfaces_unauthorized_then_continues_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-default-unsplit.sock");
    let group = GroupInfo {
        id: GroupId::from("group-worker"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let (first, second) = setup_unauthorized_then_authorized_event_test(&path, group, false).await;
    assert!(
        matches!(
            first,
            IpcInboundEvent::Unauthorized(rej) if rej.command == "register_group"
        ),
        "expected unauthorized register_group event, got {first:?}"
    );
    assert!(
        matches!(
            second,
            IpcInboundEvent::Command(AuthorizedCommand::Scoped(
                ScopedAuthorizedCommand::SendMessage(_)
            ))
        ),
        "expected follow-up authorized command event, got {second:?}"
    );
}

#[tokio::test]
async fn default_recv_event_surfaces_unauthorized_then_continues_split() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-default-split.sock");
    let group = GroupInfo {
        id: GroupId::from("group-worker"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let (first, second) = setup_unauthorized_then_authorized_event_test(&path, group, true).await;
    assert!(
        matches!(
            first,
            IpcInboundEvent::Unauthorized(rej) if rej.command == "register_group"
        ),
        "expected unauthorized register_group event, got {first:?}"
    );
    assert!(
        matches!(
            second,
            IpcInboundEvent::Command(AuthorizedCommand::Scoped(
                ScopedAuthorizedCommand::SendMessage(_)
            ))
        ),
        "expected follow-up authorized command event, got {second:?}"
    );
}

#[tokio::test]
async fn negotiated_protocol_version_persisted_unsplit_for_peer_v1_0() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-legacy-v1-0.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let group = GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let init = sample_init(&group);

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(init, HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.negotiated_protocol_version()
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    drop(client);

    let negotiated = accept_task.await.expect("join");
    assert_eq!(negotiated.major(), 1);
    assert_eq!(negotiated.minor(), 0);
}

#[tokio::test]
async fn negotiated_protocol_version_persisted_for_split_halves() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-versioned-v1-0-split.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let group = GroupInfo {
        id: GroupId::from("group-main"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let init = sample_init(&group);

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(init, HS_TIMEOUT)
            .await
            .expect("handshake");
        let (writer, reader) = conn.into_split();
        (
            writer
                .negotiated_protocol_version()
                .expect("writer version"),
            reader
                .negotiated_protocol_version()
                .expect("reader version"),
        )
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    drop(client);

    let (writer_version, reader_version) = accept_task.await.expect("join");
    assert_eq!(writer_version.major(), 1);
    assert_eq!(writer_version.minor(), 0);
    assert_eq!(reader_version, writer_version);
}
