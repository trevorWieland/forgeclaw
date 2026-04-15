//! Client-side post-handshake lifecycle enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages, HostToContainer, InitConfig,
    InitContext, InitPayload, IpcClient, IpcError, MessagesPayload, OutputCompletePayload,
    OutputDeltaPayload, ProtocolError, ReadyPayload, ShutdownPayload, ShutdownReason, StopReason,
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
        id: GroupId::from("group-main"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::from("job-client-1"),
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

async fn raw_write_host_message(stream: &mut tokio::net::UnixStream, msg: &HostToContainer) {
    let payload = serde_json::to_vec(msg).expect("serialize host msg");
    let len = u32::try_from(payload.len()).expect("fits");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write len");
    stream.write_all(&payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

async fn fake_host_handshake(listener: &tokio::net::UnixListener) -> tokio::net::UnixStream {
    use tokio::io::AsyncReadExt;

    let (mut stream, _) = listener.accept().await.expect("accept");
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read ready len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .expect("read ready payload");
    raw_write_host_message(&mut stream, &HostToContainer::Init(sample_init())).await;
    stream
}

#[tokio::test]
async fn client_recv_rejects_post_handshake_init() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-post-init.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        raw_write_host_message(&mut stream, &HostToContainer::Init(sample_init())).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    let err = client
        .recv()
        .await
        .expect_err("post-handshake init must fail");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected LifecycleViolation, got {err:?}"
    );

    host_task.await.expect("join");
}

#[tokio::test]
async fn client_recv_rejects_messages_rebind_during_processing() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-bad-rebind.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        raw_write_host_message(
            &mut stream,
            &HostToContainer::Messages(MessagesPayload {
                job_id: JobId::from("job-other"),
                messages: HistoricalMessages::default(),
            }),
        )
        .await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    let err = client
        .recv()
        .await
        .expect_err("processing rebind must fail");
    assert!(
        matches!(err, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
        "expected JobIdMismatch, got {err:?}"
    );

    host_task.await.expect("join");
}

#[tokio::test]
async fn client_split_recv_rejects_duplicate_shutdown() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-dup-shutdown.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let mut stream = fake_host_handshake(&listener).await;
        raw_write_host_message(
            &mut stream,
            &HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }),
        )
        .await;
        raw_write_host_message(
            &mut stream,
            &HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }),
        )
        .await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (_writer, mut reader) = client.into_split();

    let first = reader.recv().await.expect("first shutdown");
    assert!(matches!(first, HostToContainer::Shutdown(_)));
    let err = reader
        .recv()
        .await
        .expect_err("duplicate shutdown should fail");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected LifecycleViolation, got {err:?}"
    );

    host_task.await.expect("join");
}

#[tokio::test]
async fn client_send_rejects_mismatched_job_while_processing() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-send-mismatch.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let _stream = fake_host_handshake(&listener).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    let err = client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "wrong job".parse().expect("valid text"),
            job_id: JobId::from("job-other"),
        }))
        .await
        .expect_err("mismatched job send should fail");
    assert!(
        matches!(err, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
        "expected JobIdMismatch, got {err:?}"
    );

    host_task.await.expect("join");
}

#[tokio::test]
async fn client_send_rejects_command_after_idle_transition() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-send-idle-command.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let _stream = fake_host_handshake(&listener).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-client-1"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("first completion transitions to idle");

    let err = client
        .send(&ContainerToHost::Command(forgeclaw_ipc::CommandPayload {
            body: forgeclaw_ipc::CommandBody::SendMessage(forgeclaw_ipc::SendMessagePayload {
                target_group: GroupId::from("group-main"),
                text: "late command".parse().expect("valid text"),
            }),
        }))
        .await
        .expect_err("command in idle must fail");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected LifecycleViolation, got {err:?}"
    );

    host_task.await.expect("join");
}

#[tokio::test]
async fn client_split_send_rejects_output_delta_after_idle() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "client-split-send-idle-delta.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");

    let host_task = tokio::spawn(async move {
        let _stream = fake_host_handshake(&listener).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (mut writer, _reader) = client.into_split();

    writer
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-client-1"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("completion transitions to idle");

    let err = writer
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "late delta".parse().expect("valid text"),
            job_id: JobId::from("job-client-1"),
        }))
        .await
        .expect_err("delta in idle must fail");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected LifecycleViolation, got {err:?}"
    );

    host_task.await.expect("join");
}
