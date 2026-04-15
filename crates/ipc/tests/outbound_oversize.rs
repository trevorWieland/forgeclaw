//! Outbound oversize serialization enforcement tests.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupCapabilities, GroupExtensions, GroupExtensionsError,
    GroupExtensionsVersion, GroupInfo, HistoricalMessage, HistoricalMessages, HostToContainer,
    InitConfig, InitContext, InitPayload, IpcClient, IpcError, IpcInboundEvent, IpcServer,
    MAX_FRAME_BYTES, MessagesPayload, OutputCompletePayload, OutputDeltaPayload, ProtocolError,
    ReadyPayload, ShutdownPayload, ShutdownReason, StopReason,
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
        job_id: JobId::new("job-oversize-1").expect("valid job id"),
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

fn oversize_extensions_error() -> GroupExtensionsError {
    let mut data = serde_json::Map::new();
    data.insert(
        "blob".to_owned(),
        serde_json::Value::String("x".repeat(MAX_FRAME_BYTES + 1_024)),
    );
    GroupExtensions::with_data(
        GroupExtensionsVersion::new("1").expect("valid extension version"),
        data,
    )
    .expect_err("oversize extensions should fail constructor")
}

fn oversize_host_message(job_id: &str) -> HostToContainer {
    let large_text = "🦀".repeat(32 * 1024);
    let large_text = large_text.parse().expect("valid max-char text");
    let template = HistoricalMessage {
        sender: "A".parse().expect("valid sender"),
        text: large_text,
        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
    };

    HostToContainer::Messages(MessagesPayload {
        job_id: JobId::new(job_id).expect("valid job id"),
        messages: std::iter::repeat_n(template, 90)
            .collect::<Vec<_>>()
            .try_into()
            .expect("messages within bound"),
    })
}

fn minimal_host_messages(job_id: &str) -> HostToContainer {
    HostToContainer::Messages(MessagesPayload {
        job_id: JobId::new(job_id).expect("valid job id"),
        messages: HistoricalMessages::default(),
    })
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

#[tokio::test]
async fn client_unsplit_extensions_oversize_is_rejected_and_connection_remains_usable() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-client-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let event = conn
            .recv_event()
            .await
            .expect("recv follow-up output_delta");
        assert!(matches!(
            event,
            IpcInboundEvent::Message(ContainerToHost::OutputDelta(_))
        ));
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    let err = oversize_extensions_error();
    assert!(
        matches!(err, GroupExtensionsError::EncodedBytesExceeded { .. }),
        "expected encoded-bytes bound error, got {err:?}"
    );

    client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "follow-up".parse().expect("valid output"),
            job_id: JobId::new("job-oversize-1").expect("valid job id"),
        }))
        .await
        .expect(
            "client should remain in processing state after outbound oversize preflight failure",
        );

    accept_task.await.expect("join");
}

#[tokio::test]
async fn client_split_writer_extensions_oversize_is_rejected_and_connection_remains_usable() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-client-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (_writer, mut reader) = conn.into_split();
        let event = reader
            .recv_event()
            .await
            .expect("recv follow-up output_delta");
        assert!(matches!(
            event,
            IpcInboundEvent::Message(ContainerToHost::OutputDelta(_))
        ));
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let (mut writer, _reader) = client.into_split();

    let err = oversize_extensions_error();
    assert!(matches!(
        err,
        GroupExtensionsError::EncodedBytesExceeded { .. }
    ));

    writer
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "follow-up".parse().expect("valid output"),
            job_id: JobId::new("job-oversize-1").expect("valid job id"),
        }))
        .await
        .expect("split writer should remain in processing state after outbound oversize preflight failure");

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_unsplit_send_oversize_is_rejected_but_connection_remains_usable() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");

        let err = conn
            .send_host(&oversize_host_message("job-oversize-1"))
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::OutboundValidation {
                message_type: "messages",
                field_path,
                reason,
                ..
            }) if field_path == "$frame_bytes" && reason.contains("encoded frame size")
        ));

        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1_000,
        }))
        .await
        .expect("connection should remain usable after outbound oversize preflight failure");
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_split_writer_send_oversize_is_rejected_but_connection_remains_usable() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, _reader) = conn.into_split();

        let err = writer
            .send_host(&oversize_host_message("job-oversize-1"))
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::OutboundValidation {
                message_type: "messages",
                field_path,
                reason,
                ..
            }) if field_path == "$frame_bytes" && reason.contains("encoded frame size")
        ));

        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }))
            .await
            .expect("writer should remain usable after outbound oversize preflight failure");
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_unsplit_idle_rebind_retry_after_oversize_is_consistent() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-idle-retry-unsplit.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (mut conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");

        let event = conn.recv_event().await.expect("recv output_complete");
        assert!(matches!(
            event,
            IpcInboundEvent::Message(ContainerToHost::OutputComplete(_))
        ));

        let err = conn
            .send_host(&oversize_host_message("job-oversize-2"))
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::OutboundValidation {
                message_type: "messages",
                field_path,
                reason,
                ..
            }) if field_path == "$frame_bytes" && reason.contains("encoded frame size")
        ));

        conn.send_host(&minimal_host_messages("job-oversize-2"))
            .await
            .expect("retry should succeed with same idle rebind job");
        conn.send_host(&HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 1_000,
        }))
        .await
        .expect("shutdown should remain sendable");
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    client
        .send(&output_complete("job-oversize-1"))
        .await
        .expect("send output_complete");
    let messages = client.recv().await.expect("recv retry messages");
    assert!(matches!(
        messages,
        HostToContainer::Messages(MessagesPayload { ref job_id, .. })
            if job_id.as_ref() == "job-oversize-2"
    ));
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    accept_task.await.expect("join");
}

#[tokio::test]
async fn server_split_idle_rebind_retry_after_oversize_is_consistent() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "oversize-server-idle-retry-split.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(sample_group()).await.expect("accept");
        let (conn, _ready) = pending
            .handshake(sample_init(), HS_TIMEOUT)
            .await
            .expect("handshake");
        let (mut writer, mut reader) = conn.into_split();

        let event = reader.recv_event().await.expect("recv output_complete");
        assert!(matches!(
            event,
            IpcInboundEvent::Message(ContainerToHost::OutputComplete(_))
        ));

        let err = writer
            .send_host(&oversize_host_message("job-oversize-2"))
            .await
            .expect_err("oversize send must fail");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::OutboundValidation {
                message_type: "messages",
                field_path,
                reason,
                ..
            }) if field_path == "$frame_bytes" && reason.contains("encoded frame size")
        ));

        writer
            .send_host(&minimal_host_messages("job-oversize-2"))
            .await
            .expect("retry should succeed with same idle rebind job");
        writer
            .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::HostShutdown,
                deadline_ms: 1_000,
            }))
            .await
            .expect("shutdown should remain sendable");
    });

    let pending = IpcClient::connect(&path).await.expect("connect");
    let (mut client, _init) = pending
        .handshake(sample_ready(), HS_TIMEOUT)
        .await
        .expect("handshake");

    client
        .send(&output_complete("job-oversize-1"))
        .await
        .expect("send output_complete");
    let messages = client.recv().await.expect("recv retry messages");
    assert!(matches!(
        messages,
        HostToContainer::Messages(MessagesPayload { ref job_id, .. })
            if job_id.as_ref() == "job-oversize-2"
    ));
    let shutdown = client.recv().await.expect("recv shutdown");
    assert!(matches!(shutdown, HostToContainer::Shutdown(_)));

    accept_task.await.expect("join");
}
