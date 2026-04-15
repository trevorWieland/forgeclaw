//! Tests for the split read/write API: buffer preservation, concurrent
//! full-duplex, and peer-observable teardown in split mode.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    CommandBody, CommandPayload, ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages,
    HostToContainer, InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcInboundEvent,
    IpcServer, MessagesPayload, OutputCompletePayload, OutputDeltaPayload, ProtocolError,
    ReadyPayload, SendMessagePayload, ShutdownPayload, ShutdownReason, StopReason,
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

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: version.parse().expect("valid protocol version"),
    }
}

const HS_TIMEOUT: Duration = Duration::from_secs(5);

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

async fn recv_message_reader(
    reader: &mut forgeclaw_ipc::IpcConnectionReader,
) -> Result<ContainerToHost, IpcError> {
    match reader.recv_event().await? {
        IpcInboundEvent::Message(msg) => Ok(msg),
        IpcInboundEvent::Command(_) => Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
            expected: "message",
            got: "command",
        })),
        IpcInboundEvent::Unauthorized(rej) => {
            Err(IpcError::Protocol(ProtocolError::UnexpectedMessage {
                expected: "message",
                got: rej.command,
            }))
        }
    }
}

#[tokio::test]
async fn concurrent_send_recv_through_split() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (mut writer, mut reader) = conn.into_split();

        let write_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            writer
                .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                    reason: ShutdownReason::HostShutdown,
                    deadline_ms: 5_000,
                }))
                .await
                .expect("send shutdown");
        });

        let msg = recv_message_reader(&mut reader).await.expect("recv delta");
        assert!(matches!(msg, ContainerToHost::OutputDelta(_)));
        write_task.await.expect("write task");
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "partial output".parse().expect("valid text"),
            job_id: JobId::new("job-integration-1").expect("valid job id"),
        }))
        .await
        .expect("send delta");

    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));
    accept_task.await.expect("join");
}

#[tokio::test]
async fn split_preserves_pipelined_buffered_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        recv_message_reader(&mut reader)
            .await
            .expect("recv pipelined msg")
    });

    // Pipeline Ready + delta back-to-back via raw socket.
    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let ready =
        serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready");
    let delta = serde_json::to_vec(&ContainerToHost::OutputDelta(OutputDeltaPayload {
        text: "pipelined".parse().expect("valid text"),
        job_id: JobId::new("job-integration-1").expect("valid job id"),
    }))
    .expect("serialize delta");

    let ready_len = u32::try_from(ready.len()).expect("fits");
    raw.write_all(&ready_len.to_be_bytes()).await.expect("w");
    raw.write_all(&ready).await.expect("w");
    let delta_len = u32::try_from(delta.len()).expect("fits");
    raw.write_all(&delta_len.to_be_bytes()).await.expect("w");
    raw.write_all(&delta).await.expect("w");
    raw.flush().await.expect("flush");

    let mut len_buf = [0u8; 4];
    raw.read_exact(&mut len_buf).await.expect("read init len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload_buf = vec![0u8; len];
    raw.read_exact(&mut payload_buf).await.expect("read init");
    drop(raw);

    let msg = accept_task.await.expect("join");
    assert!(
        matches!(&msg, ContainerToHost::OutputDelta(d) if d.text == "pipelined"),
        "expected pipelined OutputDelta, got {msg:?}"
    );
}

#[tokio::test]
async fn split_reader_error_immediately_closes_transport() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    // The key assertion: the reader's fatal error shuts down the
    // write half *directly* — without the writer ever running again.
    // The writer is held but never called.
    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        // Reader gets invalid UTF-8 → triggers shutdown via handle.
        let err = recv_message_reader(&mut reader).await;
        assert!(err.is_err(), "invalid frame should error");
        // Keep _writer alive but never call send_host(). The peer
        // should still see EOF because the reader shut down the
        // shared write half directly.
    });

    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let ready =
        serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready");
    let ready_len = u32::try_from(ready.len()).expect("fits");
    raw.write_all(&ready_len.to_be_bytes()).await.expect("w");
    raw.write_all(&ready).await.expect("w");
    raw.flush().await.expect("flush");
    let mut len_buf = [0u8; 4];
    raw.read_exact(&mut len_buf).await.expect("read init len");
    let init_len = u32::from_be_bytes(len_buf) as usize;
    let mut init_buf = vec![0u8; init_len];
    raw.read_exact(&mut init_buf).await.expect("read init");
    // Send invalid UTF-8 frame to trigger fatal read error.
    let bad: &[u8] = &[0xFF, 0xFE, 0xFD];
    let bad_len = u32::try_from(bad.len()).expect("fits");
    raw.write_all(&bad_len.to_be_bytes()).await.expect("w");
    raw.write_all(bad).await.expect("w");
    raw.flush().await.expect("flush");
    // Peer sees EOF immediately — no waiting for writer.
    let mut buf = [0u8; 1];
    let n = raw.read(&mut buf).await.expect("read");
    assert_eq!(n, 0, "peer should see EOF after reader error");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn split_reader_rejects_output_delta_after_idle() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-split-lifecycle-idle.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        let first = recv_message_reader(&mut reader)
            .await
            .expect("first message");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");
    client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "late".parse().expect("valid text"),
            job_id: JobId::new("job-integration-1").expect("valid job id"),
        }))
        .await
        .expect_err("send late delta should fail client-side");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn split_reader_rejects_command_after_output_complete_until_messages() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-split-lifecycle-idle-command.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        let first = recv_message_reader(&mut reader)
            .await
            .expect("first message");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send complete");
    client
        .send(&ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::new("group-main").expect("valid group id"),
                text: "late command".parse().expect("valid text"),
            }),
        }))
        .await
        .expect_err("send command should fail client-side");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn split_writer_rejects_messages_after_shutdown() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-split-lifecycle-shutdown.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (mut writer, _reader) = conn.into_split();
        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 5_000,
            }))
            .await
            .expect("shutdown allowed");
        writer
            .send_host(&HostToContainer::Messages(MessagesPayload {
                job_id: JobId::new("job-integration-1").expect("valid job id"),
                messages: HistoricalMessages::default(),
            }))
            .await
            .expect_err("messages after shutdown should fail")
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let received = client.recv().await.expect("recv shutdown");
    assert!(matches!(received, HostToContainer::Shutdown(_)));

    let err = accept_task.await.expect("join");
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ),
        "expected lifecycle violation, got {err:?}"
    );
}

#[tokio::test]
async fn split_reader_rejects_repeated_output_complete_while_draining() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-split-lifecycle-draining-complete.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("server handshake");
        let (mut writer, mut reader) = conn.into_split();
        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 5_000,
            }))
            .await
            .expect("send shutdown");
        let first = recv_message_reader(&mut reader)
            .await
            .expect("first complete");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending_client = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending_client
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let received = client.recv().await.expect("recv shutdown");
    assert!(matches!(received, HostToContainer::Shutdown(_)));
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("first completion");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::new("job-integration-1").expect("valid job id"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect_err("second completion should fail client-side");

    accept_task.await.expect("join");
}
