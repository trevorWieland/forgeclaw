use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessage, HistoricalMessages,
    HostToContainer, InitConfig, InitContext, InitPayload, IpcClient, IpcClientOptions, IpcError,
    IpcServer, IpcServerOptions, MessagesPayload, OutputDeltaPayload, ReadyPayload,
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
        job_id: JobId::new("job-timeout-1").expect("valid job id"),
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

fn large_messages() -> HostToContainer {
    let msg = HistoricalMessage {
        sender: "A".parse().expect("valid sender"),
        text: "x".repeat(32 * 1024).parse().expect("valid text"),
        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
    };
    let messages = std::iter::repeat_n(msg, 180)
        .collect::<Vec<_>>()
        .try_into()
        .expect("messages within bound");
    HostToContainer::Messages(MessagesPayload {
        job_id: JobId::new("job-timeout-1").expect("valid job id"),
        messages,
    })
}

fn large_delta() -> ContainerToHost {
    ContainerToHost::OutputDelta(OutputDeltaPayload {
        text: "x".repeat(65_536).parse().expect("valid output delta"),
        job_id: JobId::new("job-timeout-1").expect("valid job id"),
    })
}

async fn raw_write_container(stream: &mut tokio::net::UnixStream, msg: &ContainerToHost) {
    let payload = serde_json::to_vec(msg).expect("serialize");
    let len = u32::try_from(payload.len()).expect("fits");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(&payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

async fn raw_read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.expect("read len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.expect("read payload");
    payload
}

#[tokio::test]
async fn server_unsplit_send_times_out_and_poisons() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-write-timeout-server-u.sock");
    let write_timeout = Duration::from_millis(25);
    let server = IpcServer::bind_with_options(
        &path,
        IpcServerOptions {
            write_timeout,
            ..IpcServerOptions::default()
        },
    )
    .expect("bind");

    let send_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let msg = large_messages();
        let mut timed_out = false;
        let mut unexpected = None;
        for _ in 0..512 {
            match conn.send_host(&msg).await {
                Ok(()) => {}
                Err(IpcError::Timeout(d)) => {
                    assert_eq!(d, write_timeout);
                    timed_out = true;
                    break;
                }
                Err(e) => {
                    unexpected = Some(e);
                    break;
                }
            }
        }
        assert!(
            unexpected.is_none(),
            "unexpected send error: {unexpected:?}"
        );
        assert!(timed_out, "expected send timeout");
        let err = conn.send_host(&msg).await.expect_err("poisoned connection");
        assert!(
            matches!(err, IpcError::Closed),
            "expected Closed, got {err:?}"
        );
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_write_container(&mut raw, &ContainerToHost::Ready(sample_ready("1.0"))).await;
    let _init = raw_read_frame(&mut raw).await;
    tokio::time::timeout(Duration::from_secs(8), send_task)
        .await
        .expect("join timeout")
        .expect("join");
}

#[tokio::test]
async fn server_split_writer_send_times_out_and_poisons() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-write-timeout-server-s.sock");
    let write_timeout = Duration::from_millis(25);
    let server = IpcServer::bind_with_options(
        &path,
        IpcServerOptions {
            write_timeout,
            ..IpcServerOptions::default()
        },
    )
    .expect("bind");

    let send_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, _reader) = conn.into_split();
        let msg = large_messages();
        let mut timed_out = false;
        let mut unexpected = None;
        for _ in 0..512 {
            match writer.send_host(&msg).await {
                Ok(()) => {}
                Err(IpcError::Timeout(d)) => {
                    assert_eq!(d, write_timeout);
                    timed_out = true;
                    break;
                }
                Err(e) => {
                    unexpected = Some(e);
                    break;
                }
            }
        }
        assert!(
            unexpected.is_none(),
            "unexpected send error: {unexpected:?}"
        );
        assert!(timed_out, "expected send timeout");
        let err = writer.send_host(&msg).await.expect_err("poisoned writer");
        assert!(
            matches!(err, IpcError::Closed),
            "expected Closed, got {err:?}"
        );
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("raw connect");
    raw_write_container(&mut raw, &ContainerToHost::Ready(sample_ready("1.0"))).await;
    let _init = raw_read_frame(&mut raw).await;
    tokio::time::timeout(Duration::from_secs(8), send_task)
        .await
        .expect("join timeout")
        .expect("join");
}

#[tokio::test]
async fn client_unsplit_send_times_out_and_poisons() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-write-timeout-client-u.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let _ready = raw_read_frame(&mut stream).await;
        let init = serde_json::to_vec(&HostToContainer::Init(sample_init())).expect("serialize");
        let len = u32::try_from(init.len()).expect("fits");
        stream
            .write_all(&len.to_be_bytes())
            .await
            .expect("write len");
        stream.write_all(&init).await.expect("write init");
        stream.flush().await.expect("flush");
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let write_timeout = Duration::from_millis(25);
    let pending = IpcClient::connect_with_options(&path, IpcClientOptions { write_timeout })
        .await
        .expect("connect");
    let (mut client, _) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("handshake");
    let msg = large_delta();
    let mut timed_out = false;
    let mut unexpected = None;
    for _ in 0..512 {
        match client.send(&msg).await {
            Ok(()) => {}
            Err(IpcError::Timeout(d)) => {
                assert_eq!(d, write_timeout);
                timed_out = true;
                break;
            }
            Err(e) => {
                unexpected = Some(e);
                break;
            }
        }
    }
    assert!(
        unexpected.is_none(),
        "unexpected send error: {unexpected:?}"
    );
    assert!(timed_out, "expected send timeout");
    let err = client.send(&msg).await.expect_err("poisoned client");
    assert!(
        matches!(err, IpcError::Closed),
        "expected Closed, got {err:?}"
    );

    host_task.abort();
}

#[tokio::test]
async fn client_split_writer_send_times_out_and_poisons() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-write-timeout-client-s.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let _ready = raw_read_frame(&mut stream).await;
        let init = serde_json::to_vec(&HostToContainer::Init(sample_init())).expect("serialize");
        let len = u32::try_from(init.len()).expect("fits");
        stream
            .write_all(&len.to_be_bytes())
            .await
            .expect("write len");
        stream.write_all(&init).await.expect("write init");
        stream.flush().await.expect("flush");
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let write_timeout = Duration::from_millis(25);
    let pending = IpcClient::connect_with_options(&path, IpcClientOptions { write_timeout })
        .await
        .expect("connect");
    let (client, _) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (mut writer, _reader) = client.into_split();
    let msg = large_delta();
    let mut timed_out = false;
    let mut unexpected = None;
    for _ in 0..512 {
        match writer.send(&msg).await {
            Ok(()) => {}
            Err(IpcError::Timeout(d)) => {
                assert_eq!(d, write_timeout);
                timed_out = true;
                break;
            }
            Err(e) => {
                unexpected = Some(e);
                break;
            }
        }
    }
    assert!(
        unexpected.is_none(),
        "unexpected send error: {unexpected:?}"
    );
    assert!(timed_out, "expected send timeout");
    let err = writer.send(&msg).await.expect_err("poisoned writer");
    assert!(
        matches!(err, IpcError::Closed),
        "expected Closed, got {err:?}"
    );

    host_task.abort();
}
