//! Timeout and liveness enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages, HostToContainer, InitConfig,
    InitContext, InitPayload, IpcClient, IpcError, IpcServer, MessagesPayload, ProtocolError,
    ReadyPayload, ShutdownPayload, ShutdownReason,
};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;

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

fn oversized_init() -> InitPayload {
    use forgeclaw_ipc::HistoricalMessage;

    let mut init = sample_init();
    // Maximal semantically valid payload to force backpressure if peer
    // stops reading during handshake send.
    let msg = HistoricalMessage {
        sender: "A".parse().expect("valid sender"),
        text: "x"
            .repeat(32 * 1024)
            .parse()
            .expect("valid historical message"),
        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
    };
    init.context.messages = std::iter::repeat_n(msg, 256)
        .collect::<Vec<_>>()
        .try_into()
        .expect("messages within bound");
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
        let err = conn.recv_event().await.expect_err("should timeout");
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
        let err = reader.recv_event().await.expect_err("should timeout");
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

#[tokio::test(start_paused = true)]
async fn outbound_messages_near_deadline_do_not_extend_heartbeat_timeout_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-heartbeat-unsplit-no-extend.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        tokio::time::sleep(Duration::from_secs(59)).await;
        conn.send_host(&HostToContainer::Messages(MessagesPayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            messages: HistoricalMessages::default(),
        }))
        .await
        .expect("follow-up messages should be allowed");
        conn.recv_event()
            .await
            .expect_err("should timeout at original deadline")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;

    assert!(
        accept_task.is_finished(),
        "server must timeout at 60s even if host sends follow-up messages at t=59s"
    );
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
}

#[tokio::test(start_paused = true)]
async fn outbound_messages_near_deadline_do_not_extend_heartbeat_timeout_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-heartbeat-split-no-extend.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, mut reader) = conn.into_split();
        tokio::time::sleep(Duration::from_secs(59)).await;
        writer
            .send_host(&HostToContainer::Messages(MessagesPayload {
                job_id: JobId::new("job-integration-1").expect("valid job id"),
                messages: HistoricalMessages::default(),
            }))
            .await
            .expect("follow-up messages should be allowed");
        reader
            .recv_event()
            .await
            .expect_err("should timeout at original deadline")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::task::yield_now().await;

    assert!(
        accept_task.is_finished(),
        "split reader must timeout at 60s even if writer sends follow-up messages at t=59s"
    );
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
}

#[tokio::test(start_paused = true)]
async fn shutdown_deadline_timeout_while_draining_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-draining-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1_500,
        }))
        .await
        .expect("send shutdown");
        conn.recv_event()
            .await
            .expect_err("drain timeout should fire")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1_600)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded {
                phase: "draining",
                deadline_ms: 1_500
            })
        ),
        "expected ShutdownDeadlineExceeded, got {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_deadline_timeout_while_draining_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-draining-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, mut reader) = conn.into_split();
        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_250,
            }))
            .await
            .expect("send shutdown");
        reader
            .recv_event()
            .await
            .expect_err("drain timeout should fire")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1_400)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded {
                phase: "draining",
                deadline_ms: 1_250
            })
        ),
        "expected ShutdownDeadlineExceeded, got {err:?}"
    );
}
