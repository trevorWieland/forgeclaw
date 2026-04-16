//! Adversarial tests for independent unknown-traffic hardening.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig, InitContext,
    InitPayload, IpcError, IpcInboundEvent, IpcServer, IpcServerOptions, ProtocolError,
    ReadyPayload, UnknownTrafficLimitConfig,
};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn server_options(unknown: UnknownTrafficLimitConfig) -> IpcServerOptions {
    IpcServerOptions {
        unknown_traffic_limit: unknown,
        ..IpcServerOptions::insecure_capture_only()
    }
}

async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("frame len fits u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(payload).await.expect("write payload");
}

async fn read_init_frame(stream: &mut tokio::net::UnixStream) {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read init len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .expect("read init payload");
}

fn heartbeat_frame() -> Vec<u8> {
    serde_json::to_vec(&ContainerToHost::Heartbeat(
        forgeclaw_ipc::HeartbeatPayload {
            timestamp: "2026-04-03T10:30:00Z".parse().expect("valid timestamp"),
        },
    ))
    .expect("serialize heartbeat")
}

#[tokio::test]
async fn handshake_rejects_unknown_lifetime_cap_breach() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-handshake-limit.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 1,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 0,
            rate_limit_refill_per_second: 0,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        pending.handshake(sample_init(), HS_TIMEOUT).await
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(&mut raw, br#"{"type":"experimental_a","x":1}"#).await;
    write_frame(&mut raw, br#"{"type":"experimental_b","x":2}"#).await;
    raw.flush().await.expect("flush");

    let err = accept_task
        .await
        .expect("join")
        .expect_err("handshake should fail");
    drop(raw);
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { count: 2, limit: 1 })
    ));
}

#[tokio::test]
async fn unsplit_rejects_alternating_unknown_and_heartbeat_via_lifetime_cap() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-unsplit-lifetime.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 3,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 0,
            rate_limit_refill_per_second: 0,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        for _ in 0..3 {
            let event = conn.recv_event().await.expect("heartbeat event");
            assert!(matches!(
                event,
                IpcInboundEvent::Message(ContainerToHost::Heartbeat(_))
            ));
        }
        conn.recv_event()
            .await
            .expect_err("fourth unknown should fail")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush ready");
    read_init_frame(&mut raw).await;

    let heartbeat = heartbeat_frame();
    for i in 0..4 {
        let unknown = format!(r#"{{"type":"experimental_unsplit_{i}","x":{i}}}"#);
        write_frame(&mut raw, unknown.as_bytes()).await;
        write_frame(&mut raw, &heartbeat).await;
    }
    raw.flush().await.expect("flush mixed stream");
    drop(raw);

    let err = accept_task.await.expect("join");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { count: 4, limit: 3 })
    ));
}

#[tokio::test]
async fn split_reader_rejects_alternating_unknown_and_heartbeat_via_rate_limit() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "unknown-split-rate.sock");
    let server = IpcServer::bind_with_options(
        &path,
        server_options(UnknownTrafficLimitConfig {
            lifetime_message_limit: 0,
            lifetime_byte_limit: 0,
            rate_limit_burst_capacity: 2,
            rate_limit_refill_per_second: 1,
        }),
    )
    .expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();

        for _ in 0..2 {
            let event = reader.recv_event().await.expect("heartbeat event");
            assert!(matches!(
                event,
                IpcInboundEvent::Message(ContainerToHost::Heartbeat(_))
            ));
        }
        reader
            .recv_event()
            .await
            .expect_err("third unknown should rate-limit")
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    write_frame(
        &mut raw,
        &serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready"),
    )
    .await;
    raw.flush().await.expect("flush ready");
    read_init_frame(&mut raw).await;

    let heartbeat = heartbeat_frame();
    for i in 0..3 {
        let unknown = format!(r#"{{"type":"experimental_split_{i}","x":{i}}}"#);
        write_frame(&mut raw, unknown.as_bytes()).await;
        write_frame(&mut raw, &heartbeat).await;
    }
    raw.flush().await.expect("flush mixed stream");
    drop(raw);

    let err = accept_task.await.expect("join");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::UnknownMessageRateLimitExceeded {
            burst_capacity: 2,
            refill_per_second: 1
        })
    ));
}
