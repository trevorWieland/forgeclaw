//! Tests for the split read/write API: buffer preservation, concurrent
//! full-duplex, and peer-observable teardown in split mode.

use std::path::PathBuf;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    ContainerToHost, GroupInfo, HostToContainer, InitConfig, InitContext, InitPayload, IpcClient,
    IpcServer, OutputDeltaPayload, ReadyPayload, ShutdownPayload, ShutdownReason,
};
use tempfile::tempdir;

fn socket_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".to_owned(),
        adapter_version: "0.1.0".to_owned(),
        protocol_version: version.to_owned(),
    }
}

fn sample_init() -> InitPayload {
    InitPayload {
        job_id: JobId::from("job-integration-1"),
        context: InitContext {
            messages: vec![],
            group: GroupInfo {
                id: GroupId::from("group-main"),
                name: "Main".to_owned(),
                is_main: true,
            },
            timezone: "UTC".to_owned(),
        },
        config: InitConfig {
            provider_proxy_url: "http://proxy.local".to_owned(),
            provider_proxy_token: "token".to_owned(),
            model: "claude-sonnet-4-6".to_owned(),
            max_tokens: 1000,
            session_id: None,
            tools_enabled: true,
            timeout_seconds: 600,
        },
    }
}

#[tokio::test]
async fn concurrent_send_recv_through_split() {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir, "ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");

    let accept_task = tokio::spawn(async move {
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn
            .handshake(sample_init())
            .await
            .expect("server handshake");
        let (mut writer, mut reader) = conn.into_split();

        let write_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            writer
                .send_host(&HostToContainer::Shutdown(ShutdownPayload {
                    reason: ShutdownReason::HostShutdown,
                    deadline_ms: 5_000,
                }))
                .await
                .expect("send shutdown");
        });

        let msg = reader.recv_container().await.expect("recv delta");
        assert!(matches!(msg, ContainerToHost::OutputDelta(_)));
        write_task.await.expect("write task");
    });

    let mut client = IpcClient::connect(&path).await.expect("connect");
    let _init = client
        .handshake(sample_ready("1.0"))
        .await
        .expect("client handshake");

    client
        .send(&ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "partial output".to_owned(),
            job_id: JobId::from("job-integration-1"),
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
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn
            .handshake(sample_init())
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        reader.recv_container().await.expect("recv pipelined msg")
    });

    // Pipeline Ready + delta back-to-back via raw socket.
    let mut raw = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let ready =
        serde_json::to_vec(&ContainerToHost::Ready(sample_ready("1.0"))).expect("serialize ready");
    let delta = serde_json::to_vec(&ContainerToHost::OutputDelta(OutputDeltaPayload {
        text: "pipelined".to_owned(),
        job_id: JobId::from("job-integration-1"),
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
        let mut conn = server.accept().await.expect("accept");
        let _ready = conn
            .handshake(sample_init())
            .await
            .expect("server handshake");
        let (_writer, mut reader) = conn.into_split();
        // Reader gets invalid UTF-8 → triggers shutdown via handle.
        let err = reader.recv_container().await;
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
