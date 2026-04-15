//! Tests for the `OwnershipPending` wrapper: verify that task
//! ownership checks are enforced correctly for non-main sessions
//! and that main sessions always pass.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::GroupId;
use forgeclaw_ipc::{
    AuthorizedCommand, CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo,
    HistoricalMessages, InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcServer,
    ProtocolError, ReadyPayload, ScopedAuthorizedCommand,
};
use tempfile::tempdir;

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

const HS_TIMEOUT: Duration = Duration::from_secs(5);

fn sample_ready() -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
    }
}

fn sample_init(group: &GroupInfo) -> InitPayload {
    InitPayload {
        job_id: forgeclaw_core::JobId::new("job-own-1").expect("valid job id"),
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

async fn setup_auth_test(
    path: &std::path::Path,
    group: GroupInfo,
    command: ContainerToHost,
) -> Result<AuthorizedCommand, IpcError> {
    let server = IpcServer::bind(path).expect("bind");
    let init = sample_init(&group);

    let path_clone = path.to_path_buf();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(init, HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();
        reader.recv_command().await
    });

    let pending_client = IpcClient::connect(&path_clone).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&command).await.expect("send");
    drop(client);

    accept_task.await.expect("join")
}

#[tokio::test]
async fn pause_task_returns_ownership_pending() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-pause.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::PauseTask(forgeclaw_ipc::PauseTaskPayload {
            task_id: forgeclaw_core::TaskId::new("task-1").expect("valid task id"),
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("pause_task should pass IPC auth");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(_))
        ),
        "expected PauseTask, got {cmd:?}"
    );
    let AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(pending)) = cmd else {
        unreachable!();
    };
    assert_eq!(
        pending.session_group(),
        &GroupId::new("group-worker").expect("valid group id")
    );
    assert!(!pending.is_main());
    let payload = pending
        .verify(&GroupId::new("group-worker").expect("valid group id"))
        .expect("same-group should pass");
    assert_eq!(
        payload.task_id,
        forgeclaw_core::TaskId::new("task-1").expect("valid task id")
    );
}

#[tokio::test]
async fn ownership_pending_verify_rejects_cross_group() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-pause-cross.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::PauseTask(forgeclaw_ipc::PauseTaskPayload {
            task_id: forgeclaw_core::TaskId::new("task-1").expect("valid task id"),
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("pause_task should pass IPC auth");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(_))
        ),
        "expected PauseTask, got {cmd:?}"
    );
    let AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(pending)) = cmd else {
        unreachable!();
    };
    let err = pending
        .verify(&GroupId::new("group-other").expect("valid group id"))
        .expect_err("cross-group should fail");
    assert!(
        matches!(err, IpcError::Protocol(ProtocolError::Unauthorized { .. })),
        "expected Unauthorized, got {err:?}"
    );
}

#[tokio::test]
async fn ownership_pending_main_always_passes() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-pause-main.sock");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::PauseTask(forgeclaw_ipc::PauseTaskPayload {
            task_id: forgeclaw_core::TaskId::new("task-1").expect("valid task id"),
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("pause_task should pass for main");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(_))
        ),
        "expected PauseTask, got {cmd:?}"
    );
    let AuthorizedCommand::Scoped(ScopedAuthorizedCommand::PauseTask(pending)) = cmd else {
        unreachable!();
    };
    assert!(pending.is_main());
    let payload = pending
        .verify(&GroupId::new("group-totally-different").expect("valid group id"))
        .expect("main always passes");
    assert_eq!(
        payload.task_id,
        forgeclaw_core::TaskId::new("task-1").expect("valid task id")
    );
}
