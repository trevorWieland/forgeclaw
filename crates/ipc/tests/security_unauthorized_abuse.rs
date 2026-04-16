//! Unauthorized-command abuse controls: backoff and disconnect.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcServer, IpcServerOptions,
    ProtocolError, ReadyPayload, RegisterGroupPayload, UnauthorizedCommandLimitConfig,
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

fn unauthorized_register_group_command() -> ContainerToHost {
    ContainerToHost::Command(CommandPayload {
        body: CommandBody::RegisterGroup(RegisterGroupPayload {
            name: "new-group".parse().expect("valid name"),
            extensions: None,
        }),
    })
}

async fn run_unauthorized_abuse_sequence(path: &std::path::Path, split: bool) -> IpcError {
    let server = IpcServer::bind_with_options(
        path,
        IpcServerOptions {
            unauthorized_limit: UnauthorizedCommandLimitConfig {
                burst_capacity: 1,
                refill_per_second: 1,
                backoff: Duration::from_millis(5),
                disconnect_after_strikes: 2,
            },
            ..IpcServerOptions::insecure_capture_only()
        },
    )
    .expect("bind");
    let group = GroupInfo {
        id: GroupId::new("group-worker").expect("valid group id"),
        name: "Worker".parse().expect("valid name"),
        is_main: false,
        capabilities: GroupCapabilities::default(),
    };
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
            for _ in 0..2 {
                let err = reader.recv_command().await.expect_err("should reject");
                assert!(matches!(
                    err,
                    IpcError::Protocol(ProtocolError::Unauthorized {
                        command: "register_group",
                        ..
                    })
                ));
            }
            reader
                .recv_command()
                .await
                .expect_err("third attempt should disconnect")
        } else {
            for _ in 0..2 {
                let err = conn.recv_command().await.expect_err("should reject");
                assert!(matches!(
                    err,
                    IpcError::Protocol(ProtocolError::Unauthorized {
                        command: "register_group",
                        ..
                    })
                ));
            }
            conn.recv_command()
                .await
                .expect_err("third attempt should disconnect")
        }
    });

    let pending_client = IpcClient::connect(&path_clone).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    for _ in 0..3 {
        client
            .send(&unauthorized_register_group_command())
            .await
            .expect("send");
    }
    drop(client);

    accept_task.await.expect("join")
}

#[tokio::test]
async fn unauthorized_abuse_disconnects_unsplit_connection() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-abuse-unsplit.sock");
    let err = run_unauthorized_abuse_sequence(&path, false).await;
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse {
                command: "register_group",
                ..
            })
        ),
        "expected UnauthorizedCommandAbuse, got {err:?}"
    );
}

#[tokio::test]
async fn unauthorized_abuse_disconnects_split_connection() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "auth-abuse-split.sock");
    let err = run_unauthorized_abuse_sequence(&path, true).await;
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse {
                command: "register_group",
                ..
            })
        ),
        "expected UnauthorizedCommandAbuse, got {err:?}"
    );
}
