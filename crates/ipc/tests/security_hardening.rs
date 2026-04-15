//! Security hardening tests: bounded unknown-frame skipping and
//! the full IPC authorization matrix.

use std::path::PathBuf;

use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    AuthorizedCommand, CommandBody, CommandPayload, ContainerToHost, DispatchTanrenPayload,
    GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig, InitContext, InitPayload,
    IpcClient, IpcError, IpcServer, PrivilegedAuthorizedCommand, ProtocolError, ReadyPayload,
    RegisterGroupPayload, ScheduleTaskPayload, ScheduleType, ScopedAuthorizedCommand,
    SendMessagePayload,
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

#[tokio::test]
async fn too_many_unknown_frames_poisons_connection() {
    use tokio::io::AsyncWriteExt;

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };

    let init = sample_init(&group);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        let _ = tx.send(());
        // Handshake internally calls recv_container, hitting the unknown-frame limit.
        let err = pending.handshake(init, Duration::from_secs(5)).await;
        assert!(
            matches!(
                err,
                Err(IpcError::Protocol(
                    ProtocolError::TooManyUnknownMessages { .. }
                ))
            ),
            "expected TooManyUnknownMessages, got {err:?}"
        );
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    // Wait for accept to capture peer credentials.
    let _ = rx.await;
    // Send 33 unknown-type frames (cap is 32).
    for i in 0..33 {
        let payload = format!(r#"{{"type":"unknown_{i}","data":{i}}}"#);
        let payload_bytes = payload.as_bytes();
        let len = u32::try_from(payload_bytes.len()).expect("fits");
        raw.write_all(&len.to_be_bytes()).await.expect("w");
        raw.write_all(payload_bytes).await.expect("w");
    }
    raw.flush().await.expect("flush");
    drop(raw);

    accept_task.await.expect("join");
}

// ── Authorization test helpers ──────────────────────────────────

const HS_TIMEOUT: Duration = Duration::from_secs(5);

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: version.parse().expect("valid protocol version"),
    }
}

fn sample_init(group: &GroupInfo) -> InitPayload {
    InitPayload {
        job_id: JobId::new("job-auth-1").expect("valid job id"),
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

/// Set up a handshake'd server reader + client and run a command
/// through `recv_command`, returning the authorization result.
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
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client.send(&command).await.expect("send");
    drop(client);

    accept_task.await.expect("join")
}

// ── Privileged command tests ────────────────────────────────────

#[tokio::test]
async fn privileged_command_rejected_for_non_main_session() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-reject.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "new-group".parse().expect("valid name"),
            extensions: None,
        }),
    });

    let err = setup_auth_test(&path, group, command)
        .await
        .expect_err("should reject");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::Unauthorized {
                command: "register_group",
                ..
            })
        ),
        "expected Unauthorized(register_group), got {err:?}"
    );
}

#[tokio::test]
async fn privileged_command_accepted_for_main_session() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-accept.sock");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "new-group".parse().expect("valid name"),
            extensions: None,
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("should accept");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Privileged(PrivilegedAuthorizedCommand::RegisterGroup(_))
        ),
        "expected RegisterGroup, got {cmd:?}"
    );
}

// ── Group-scoping tests ─────────────────────────────────────────

#[tokio::test]
async fn send_message_own_group_accepted_for_non_main() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-msg-own.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::new("group-worker").expect("valid group id"),
            text: "hello".parse().expect("valid text"),
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("own-group should pass");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::SendMessage(_))
        ),
        "expected SendMessage, got {cmd:?}"
    );
}

#[tokio::test]
async fn send_message_cross_group_rejected_for_non_main() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-msg-cross.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::new("group-other").expect("valid group id"),
            text: "hello".parse().expect("valid text"),
        }),
    });

    let err = setup_auth_test(&path, group, command)
        .await
        .expect_err("cross-group should reject");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::Unauthorized {
                command: "send_message",
                reason: "cross-group target",
            })
        ),
        "expected Unauthorized(send_message, cross-group), got {err:?}"
    );
}

#[tokio::test]
async fn send_message_cross_group_accepted_for_main() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-msg-main.sock");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::new("group-other").expect("valid group id"),
            text: "hello from main".parse().expect("valid text"),
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("main can cross-group");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::SendMessage(_))
        ),
        "expected SendMessage, got {cmd:?}"
    );
}

#[tokio::test]
async fn schedule_task_cross_group_rejected_for_non_main() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-sched-cross.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::ScheduleTask(ScheduleTaskPayload {
            group: GroupId::new("group-other").expect("valid group id"),
            schedule_type: ScheduleType::Once,
            schedule_value: "2026-04-12T00:00:00Z".parse().expect("valid schedule"),
            prompt: "p".parse().expect("valid prompt"),
            context_mode: None,
        }),
    });

    let err = setup_auth_test(&path, group, command)
        .await
        .expect_err("cross-group schedule should reject");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::Unauthorized {
                command: "schedule_task",
                reason: "cross-group target",
            })
        ),
        "expected Unauthorized(schedule_task, cross-group), got {err:?}"
    );
}

// ── Capability-gated tests ──────────────────────────────────────

#[tokio::test]
async fn dispatch_tanren_rejected_without_capability() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-tanren-no.sock");

    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities { tanren: false },
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "forgeclaw".parse().expect("valid project"),
            branch: "main".parse().expect("valid branch"),
            phase: forgeclaw_ipc::TanrenPhase::DoTask,
            prompt: "do thing".parse().expect("valid prompt"),
            environment_profile: None,
        }),
    });

    let err = setup_auth_test(&path, group, command)
        .await
        .expect_err("should reject without tanren capability");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::Unauthorized {
                command: "dispatch_tanren",
                reason: "missing tanren capability",
            })
        ),
        "expected Unauthorized(dispatch_tanren, capability), got {err:?}"
    );
}

#[tokio::test]
async fn dispatch_tanren_accepted_with_capability() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-tanren-yes.sock");

    let group = GroupInfo {
        id: GroupId::new("group-tanren").expect("valid group id"),
        name: "Tanren".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities { tanren: true },
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "forgeclaw".parse().expect("valid project"),
            branch: "main".parse().expect("valid branch"),
            phase: forgeclaw_ipc::TanrenPhase::DoTask,
            prompt: "do thing".parse().expect("valid prompt"),
            environment_profile: None,
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("tanren capability should pass");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::DispatchTanren(_))
        ),
        "expected DispatchTanren, got {cmd:?}"
    );
}

#[tokio::test]
async fn dispatch_tanren_rejected_for_main_without_capability() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-tanren-main-no.sock");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities { tanren: false },
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "forgeclaw".parse().expect("valid project"),
            branch: "main".parse().expect("valid branch"),
            phase: forgeclaw_ipc::TanrenPhase::DoTask,
            prompt: "do thing".parse().expect("valid prompt"),
            environment_profile: None,
        }),
    });

    let err = setup_auth_test(&path, group, command)
        .await
        .expect_err("main without tanren capability should be rejected");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::Unauthorized {
                command: "dispatch_tanren",
                reason: "missing tanren capability",
            })
        ),
        "expected Unauthorized(dispatch_tanren, capability), got {err:?}"
    );
}

#[tokio::test]
async fn dispatch_tanren_accepted_for_main_with_capability() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-tanren-main-yes.sock");

    let group = GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities { tanren: true },
    };
    let command = ContainerToHost::Command(CommandPayload {
        body: CommandBody::DispatchTanren(DispatchTanrenPayload {
            project: "forgeclaw".parse().expect("valid project"),
            branch: "main".parse().expect("valid branch"),
            phase: forgeclaw_ipc::TanrenPhase::DoTask,
            prompt: "do thing".parse().expect("valid prompt"),
            environment_profile: None,
        }),
    });

    let cmd = setup_auth_test(&path, group, command)
        .await
        .expect("main with tanren capability should pass");
    assert!(
        matches!(
            cmd,
            AuthorizedCommand::Scoped(ScopedAuthorizedCommand::DispatchTanren(_))
        ),
        "expected DispatchTanren, got {cmd:?}"
    );
}
