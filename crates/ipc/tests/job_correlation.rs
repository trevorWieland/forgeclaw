//! Job ID correlation and transition-coherence tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, ErrorCode, ErrorPayload, GroupCapabilities, GroupInfo, HistoricalMessage,
    HistoricalMessages, HostToContainer, InitConfig, InitContext, InitPayload, IpcClient, IpcError,
    IpcInboundEvent, IpcServer, MessagesPayload, OutputCompletePayload, OutputDeltaPayload,
    ProgressPayload, ProtocolError, ReadyPayload, StopReason,
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
        job_id: JobId::from("job-integration-1"),
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

async fn recv_message(
    conn: &mut forgeclaw_ipc::IpcConnection,
) -> Result<ContainerToHost, IpcError> {
    match conn.recv_event().await? {
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

async fn run_unsplit_correlation_case(name: &str, msg: ContainerToHost, expect_mismatch: bool) {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, name);
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        recv_message(&mut conn).await
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let send_result = client.send(&msg).await;

    if expect_mismatch {
        if let Err(e) = send_result {
            assert!(
                matches!(e, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
                "unexpected send error: {e:?}"
            );
            drop(client);
            let _ = accept_task.await.expect("join");
        } else {
            let err = accept_task
                .await
                .expect("join")
                .expect_err("should reject mismatched job");
            assert!(
                matches!(err, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
                "expected JobIdMismatch, got {err:?}"
            );
        }
    } else {
        send_result.expect("send message");
        let received = accept_task.await.expect("join").expect("message accepted");
        assert!(
            matches!(
                received,
                ContainerToHost::Error(ErrorPayload { job_id: None, .. })
            ),
            "expected error(job_id=None), got {received:?}"
        );
    }
}

#[tokio::test]
async fn unsplit_rejects_mismatched_job_scoped_messages() {
    run_unsplit_correlation_case(
        "ipc-job-unsplit-delta.sock",
        ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
            job_id: JobId::from("job-other"),
        }),
        true,
    )
    .await;

    run_unsplit_correlation_case(
        "ipc-job-unsplit-progress.sock",
        ContainerToHost::Progress(ProgressPayload {
            job_id: JobId::from("job-other"),
            stage: "tool_execution".parse().expect("valid stage"),
            detail: None,
            percent: None,
        }),
        true,
    )
    .await;

    run_unsplit_correlation_case(
        "ipc-job-unsplit-complete.sock",
        ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-other"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }),
        true,
    )
    .await;

    run_unsplit_correlation_case(
        "ipc-job-unsplit-error-some.sock",
        ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "boom".parse().expect("valid error message"),
            fatal: false,
            job_id: Some(JobId::from("job-other")),
        }),
        true,
    )
    .await;

    run_unsplit_correlation_case(
        "ipc-job-unsplit-error-none.sock",
        ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "non-job error".parse().expect("valid error message"),
            fatal: false,
            job_id: None,
        }),
        false,
    )
    .await;
}

async fn run_split_correlation_case(name: &str, msg: ContainerToHost, expect_mismatch: bool) {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, name);
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();
        recv_message_reader(&mut reader).await
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    let send_result = client.send(&msg).await;

    if expect_mismatch {
        if let Err(e) = send_result {
            assert!(
                matches!(e, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
                "unexpected send error: {e:?}"
            );
            drop(client);
            let _ = accept_task.await.expect("join");
        } else {
            let err = accept_task
                .await
                .expect("join")
                .expect_err("should reject mismatched job");
            assert!(
                matches!(err, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
                "expected JobIdMismatch, got {err:?}"
            );
        }
    } else {
        send_result.expect("send message");
        let received = accept_task.await.expect("join").expect("message accepted");
        assert!(
            matches!(
                received,
                ContainerToHost::Error(ErrorPayload { job_id: None, .. })
            ),
            "expected error(job_id=None), got {received:?}"
        );
    }
}

#[tokio::test]
async fn split_rejects_mismatched_job_scoped_messages() {
    run_split_correlation_case(
        "ipc-job-split-delta.sock",
        ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
            job_id: JobId::from("job-other"),
        }),
        true,
    )
    .await;

    run_split_correlation_case(
        "ipc-job-split-progress.sock",
        ContainerToHost::Progress(ProgressPayload {
            job_id: JobId::from("job-other"),
            stage: "tool_execution".parse().expect("valid stage"),
            detail: None,
            percent: None,
        }),
        true,
    )
    .await;

    run_split_correlation_case(
        "ipc-job-split-complete.sock",
        ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-other"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }),
        true,
    )
    .await;

    run_split_correlation_case(
        "ipc-job-split-error-some.sock",
        ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "boom".parse().expect("valid error message"),
            fatal: false,
            job_id: Some(JobId::from("job-other")),
        }),
        true,
    )
    .await;

    run_split_correlation_case(
        "ipc-job-split-error-none.sock",
        ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "non-job error".parse().expect("valid error message"),
            fatal: false,
            job_id: None,
        }),
        false,
    )
    .await;
}

#[tokio::test]
async fn host_messages_job_mismatch_rejected_while_processing() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-host-messages-processing-mismatch.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        conn.send_host(&HostToContainer::Messages(MessagesPayload {
            job_id: JobId::from("job-other"),
            messages: vec![HistoricalMessage {
                sender: "Alice".parse().expect("valid sender"),
                text: "follow-up".parse().expect("valid text"),
                timestamp: "2026-04-03T10:05:00Z".parse().expect("valid timestamp"),
            }]
            .try_into()
            .expect("messages within bound"),
        }))
        .await
        .expect_err("mismatched job_id should fail")
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (_client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");

    let err = accept_task.await.expect("join");
    assert!(
        matches!(err, IpcError::Protocol(ProtocolError::JobIdMismatch { .. })),
        "expected JobIdMismatch, got {err:?}"
    );
}

#[tokio::test]
async fn host_messages_rebinds_in_idle_and_enforces_new_job_split() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc-host-messages-idle-rebind-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, mut reader) = conn.into_split();

        let first = recv_message_reader(&mut reader)
            .await
            .expect("initial completion");
        assert!(matches!(first, ContainerToHost::OutputComplete(_)));

        writer
            .send_host(&HostToContainer::Messages(MessagesPayload {
                job_id: JobId::from("job-integration-2"),
                messages: vec![HistoricalMessage {
                    sender: "Alice".parse().expect("valid sender"),
                    text: "new job".parse().expect("valid text"),
                    timestamp: "2026-04-03T10:06:00Z".parse().expect("valid timestamp"),
                }]
                .try_into()
                .expect("messages within bound"),
            }))
            .await
            .expect("messages rebind should succeed in idle");
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready("1.0"), HS_TIMEOUT)
        .await
        .expect("client handshake");
    client
        .send(&ContainerToHost::OutputComplete(OutputCompletePayload {
            job_id: JobId::from("job-integration-1"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::EndTurn,
        }))
        .await
        .expect("send first completion");

    let rebinding_msg = client.recv().await.expect("recv messages");
    assert!(matches!(rebinding_msg, HostToContainer::Messages(_)));

    client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "old job delta".parse().expect("valid text"),
            job_id: JobId::from("job-integration-1"),
        }))
        .await
        .expect_err("old-job delta should fail client-side");

    accept_task.await.expect("join");
}
