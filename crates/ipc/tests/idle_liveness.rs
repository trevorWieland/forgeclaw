//! Idle-phase read timeout enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig, InitContext,
    InitPayload, IpcClient, IpcError, IpcInboundEvent, IpcServer, OutputCompletePayload,
    ProtocolError, ReadyPayload, StopReason,
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

fn output_complete(job_id: &str) -> ContainerToHost {
    ContainerToHost::OutputComplete(OutputCompletePayload {
        job_id: JobId::new(job_id).expect("valid job id"),
        result: Some("done".parse().expect("valid result")),
        session_id: None,
        token_usage: None,
        stop_reason: StopReason::EndTurn,
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

async fn raw_send_partial_frame(
    stream: &mut tokio::net::UnixStream,
    payload: &[u8],
    partial_len: usize,
) {
    let len = u32::try_from(payload.len()).expect("fits");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream
        .write_all(&payload[..partial_len.min(payload.len())])
        .await
        .expect("write partial payload");
    stream.flush().await.expect("flush partial payload");
}

#[tokio::test(start_paused = true)]
async fn idle_read_timeout_after_output_complete_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-timeout-unsplit.sock");
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
            .expect_err("idle timeout should fire")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&output_complete("job-integration-1"))
        .await
        .expect("send output_complete");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: "idle",
                timeout_secs: 60
            })
        ),
        "expected IdleReadTimeout, got {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_read_timeout_after_output_complete_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-timeout-split.sock");
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
            .expect_err("idle timeout should fire")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&output_complete("job-integration-1"))
        .await
        .expect("send output_complete");

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: "idle",
                timeout_secs: 60
            })
        ),
        "expected IdleReadTimeout, got {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_partial_frame_stall_times_out_unsplit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-partial-unsplit.sock");
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
            .expect_err("idle partial-frame stall should timeout")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready("1.0"))).await;
    raw_send_message(&mut raw, &output_complete("job-integration-1")).await;
    let heartbeat = br#"{"type":"heartbeat","timestamp":"2026-04-03T10:00:00Z"}"#;
    raw_send_partial_frame(&mut raw, heartbeat, 8).await;

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: "idle",
                timeout_secs: 60
            })
        ),
        "expected IdleReadTimeout, got {err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn idle_partial_frame_stall_times_out_split_reader() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-idle-partial-split.sock");
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
            .expect_err("idle partial-frame stall should timeout")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_send_message(&mut raw, &ContainerToHost::Ready(sample_ready("1.0"))).await;
    raw_send_message(&mut raw, &output_complete("job-integration-1")).await;
    let heartbeat = br#"{"type":"heartbeat","timestamp":"2026-04-03T10:00:00Z"}"#;
    raw_send_partial_frame(&mut raw, heartbeat, 8).await;

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(61)).await;

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::IdleReadTimeout {
                phase: "idle",
                timeout_secs: 60
            })
        ),
        "expected IdleReadTimeout, got {err:?}"
    );
}
