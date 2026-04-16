//! Idle-phase heartbeat rejection tests.
//!
//! Heartbeats are documented as a `Processing`/`Draining` liveness
//! signal in `docs/IPC_PROTOCOL.md` § Lifecycle. The host (and the
//! typed client, symmetrically) rejects them in `Idle` so a buggy or
//! malicious container cannot use heartbeat traffic to pin idle session
//! resources indefinitely.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HeartbeatPayload, HistoricalMessages,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcInboundEvent, IpcServer,
    OutputCompletePayload, ProtocolError, ReadyPayload, StopReason,
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

fn output_complete() -> ContainerToHost {
    ContainerToHost::OutputComplete(OutputCompletePayload {
        job_id: JobId::new("job-integration-1").expect("valid job id"),
        result: Some("done".parse().expect("valid result")),
        session_id: None,
        token_usage: None,
        stop_reason: StopReason::EndTurn,
    })
}

fn heartbeat_message() -> ContainerToHost {
    ContainerToHost::Heartbeat(HeartbeatPayload {
        timestamp: "2026-04-16T00:00:00Z".parse().expect("valid timestamp"),
    })
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
async fn idle_heartbeat_is_rejected_immediately_unsplit() {
    // Simulates a buggy/malicious container that bypasses its own
    // outbound lifecycle gate (the typed client refuses to send a
    // heartbeat from Idle, so we drive the socket directly).
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-hb-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let first = conn.recv_event().await.expect("recv output_complete");
        assert!(matches!(
            first,
            IpcInboundEvent::Message(ContainerToHost::OutputComplete(_))
        ));
        conn.recv_event()
            .await
            .expect_err("idle heartbeat must be rejected")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready())).await;
    raw_send_message(&mut raw, &output_complete()).await;
    raw_send_message(&mut raw, &heartbeat_message()).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                phase: "idle",
                direction: "container_to_host",
                message_type: "heartbeat",
                ..
            })
        ),
        "expected LifecycleViolation for heartbeat in idle, got {err:?}"
    );
}

#[tokio::test]
async fn idle_heartbeat_is_rejected_immediately_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-hb-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();
        let first = reader.recv_event().await.expect("recv output_complete");
        assert!(matches!(
            first,
            IpcInboundEvent::Message(ContainerToHost::OutputComplete(_))
        ));
        reader
            .recv_event()
            .await
            .expect_err("idle heartbeat must be rejected")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready())).await;
    raw_send_message(&mut raw, &output_complete()).await;
    raw_send_message(&mut raw, &heartbeat_message()).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                phase: "idle",
                direction: "container_to_host",
                message_type: "heartbeat",
                ..
            })
        ),
        "expected LifecycleViolation for heartbeat in idle, got {err:?}"
    );
}

#[tokio::test]
async fn typed_client_refuses_to_send_heartbeat_from_idle() {
    // The IpcClient enforces the same outbound gate the host enforces
    // inbound, so well-behaved adapters cannot accidentally emit an
    // idle heartbeat. This test pins the symmetric enforcement.
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-hb-typed.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let _ = conn.recv_event().await.expect("recv output_complete");
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&output_complete())
        .await
        .expect("send output_complete");
    let err = client
        .send(&heartbeat_message())
        .await
        .expect_err("client must refuse idle heartbeat");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                phase: "idle",
                direction: "container_to_host",
                message_type: "heartbeat",
                ..
            })
        ),
        "expected client-side LifecycleViolation, got {err:?}"
    );
    accept_task.await.expect("join");
}

#[tokio::test]
async fn idle_heartbeat_spam_does_not_extend_idle_deadline() {
    // Sustained-burst variant: even repeated heartbeats during Idle
    // must terminate on the very first one. Resource pinning is
    // structurally impossible because the lifecycle violation closes
    // the connection before the read loop ever recomputes the idle
    // deadline.
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-hb-spam.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let first = conn.recv_event().await.expect("recv output_complete");
        assert!(matches!(
            first,
            IpcInboundEvent::Message(ContainerToHost::OutputComplete(_))
        ));
        conn.recv_event()
            .await
            .expect_err("idle heartbeat spam must be rejected")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready())).await;
    raw_send_message(&mut raw, &output_complete()).await;
    for _ in 0..16 {
        raw_send_message(&mut raw, &heartbeat_message()).await;
    }

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                phase: "idle",
                direction: "container_to_host",
                message_type: "heartbeat",
                ..
            })
        ),
        "expected LifecycleViolation for heartbeat-spam in idle, got {err:?}"
    );
}
