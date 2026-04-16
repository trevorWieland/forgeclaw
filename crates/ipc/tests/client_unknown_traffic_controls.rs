//! Client-side unknown-traffic abuse controls (lifetime + rate).

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    GroupCapabilities, GroupInfo, HistoricalMessages, HostToContainer, InitConfig, InitContext,
    InitPayload, IpcClient, IpcClientOptions, IpcError, MessagesPayload, ProtocolError,
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

fn sample_ready() -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: "1.0".parse().expect("valid protocol version"),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::new("job-adv-1").expect("valid job id"),
        context: InitContext {
            messages: HistoricalMessages::default(),
            group: GroupInfo {
                id: GroupId::new("group-main").expect("valid group id"),
                name: "Main".parse().expect("valid name"),
                is_main: true,
                capabilities: GroupCapabilities::default(),
            },
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

async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("payload fits in u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

async fn read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.expect("read payload");
    buf
}

async fn fake_host_handshake(listener: &tokio::net::UnixListener) -> tokio::net::UnixStream {
    let (mut stream, _) = listener.accept().await.expect("accept");
    let _ready_bytes = read_frame(&mut stream).await;
    let init_json = serde_json::to_vec(&HostToContainer::Init(sample_init())).expect("serialize");
    write_frame(&mut stream, &init_json).await;
    stream
}

fn valid_messages_frame() -> Vec<u8> {
    serde_json::to_vec(&HostToContainer::Messages(MessagesPayload {
        job_id: JobId::new("job-adv-1").expect("valid job id"),
        messages: HistoricalMessages::default(),
    }))
    .expect("serialize messages")
}

#[tokio::test]
async fn client_recv_enforces_unknown_lifetime_cap_with_interleaved_known_frames() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-client-lifetime-cap.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        let known = valid_messages_frame();
        for i in 0..4 {
            let payload = format!(r#"{{"type":"unknown_lifetime_{i}","data":null}}"#);
            write_frame(&mut stream, payload.as_bytes()).await;
            if i < 3 {
                write_frame(&mut stream, &known).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect_with_options(
        &path,
        IpcClientOptions {
            write_timeout: Duration::from_secs(5),
            unknown_traffic_limit: UnknownTrafficLimitConfig {
                lifetime_message_limit: 3,
                lifetime_byte_limit: 0,
                rate_limit_burst_capacity: 0,
                rate_limit_refill_per_second: 0,
                ignored_field_lifetime_keys: 0,
                ignored_field_lifetime_bytes: 0,
            },
        },
    )
    .await
    .expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    for _ in 0..3 {
        let msg = client.recv().await.expect("known messages frame");
        assert!(matches!(msg, HostToContainer::Messages(_)));
    }
    let err = client.recv().await.expect_err("fourth unknown should fail");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { count: 4, limit: 3 })
    ));
    host_task.abort();
}

#[tokio::test]
async fn client_split_recv_enforces_unknown_rate_limit_with_interleaved_known_frames() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-client-rate-cap.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        let known = valid_messages_frame();
        for i in 0..3 {
            let payload = format!(r#"{{"type":"unknown_rate_{i}","data":null}}"#);
            write_frame(&mut stream, payload.as_bytes()).await;
            write_frame(&mut stream, &known).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let pending_client = IpcClient::connect_with_options(
        &path,
        IpcClientOptions {
            write_timeout: Duration::from_secs(5),
            unknown_traffic_limit: UnknownTrafficLimitConfig {
                lifetime_message_limit: 0,
                lifetime_byte_limit: 0,
                rate_limit_burst_capacity: 2,
                rate_limit_refill_per_second: 1,
                ignored_field_lifetime_keys: 0,
                ignored_field_lifetime_bytes: 0,
            },
        },
    )
    .await
    .expect("connect");
    let (client, _init) = pending_client
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (_writer, mut reader) = client.into_split();
    for _ in 0..2 {
        let msg = reader.recv().await.expect("known messages frame");
        assert!(matches!(msg, HostToContainer::Messages(_)));
    }
    let err = reader
        .recv()
        .await
        .expect_err("third unknown should rate-limit");
    assert!(matches!(
        err,
        IpcError::Protocol(ProtocolError::UnknownMessageRateLimitExceeded {
            burst_capacity: 2,
            refill_per_second: 1
        })
    ));
    host_task.abort();
}
