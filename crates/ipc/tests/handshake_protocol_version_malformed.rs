//! Handshake-boundary rejection tests for malformed `protocol_version`
//! strings.
//!
//! The `ProtocolVersionText` wire type validates at deserialization
//! time, so adversarial versions surface as `FrameError::MalformedJson`
//! on the server's handshake path. Versions that are well-formed but
//! incompatible (different major) still route through
//! `ProtocolError::UnsupportedVersion`.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    FrameError, GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig, InitContext,
    InitPayload, IpcError, IpcServer, ProtocolError,
};
use tempfile::tempdir;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

const HS_TIMEOUT: Duration = Duration::from_secs(5);

fn socket_path(dir: &tempfile::TempDir) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let socket_dir = dir.path().join("s");
    std::fs::create_dir_all(&socket_dir).expect("mkdir s");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod s");
    socket_dir.join("ipc.sock")
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
        job_id: JobId::new("job-1").expect("valid job id"),
        context: InitContext {
            messages: HistoricalMessages::default(),
            group: sample_group(),
            timezone: "UTC".parse().expect("valid timezone"),
        },
        config: InitConfig {
            provider_proxy_url: "http://proxy.local".parse().expect("valid proxy url"),
            provider_proxy_token: "token".parse().expect("valid proxy token"),
            model: "claude-sonnet-4-6".parse().expect("valid model"),
            max_tokens: 512,
            session_id: None,
            tools_enabled: true,
            timeout_seconds: 300,
        },
    }
}

async fn write_raw_frame(stream: &mut UnixStream, payload: &[u8]) {
    let len = u32::try_from(payload.len()).expect("frame len fits u32");
    stream
        .write_all(&len.to_be_bytes())
        .await
        .expect("write length prefix");
    stream.write_all(payload).await.expect("write payload");
    stream.flush().await.expect("flush");
}

async fn drive_handshake_with_ready_payload(raw_ready_json: &str) -> IpcError {
    let dir = tempdir().expect("tempdir");
    let path = socket_path(&dir);
    let server = IpcServer::bind(&path).expect("bind");
    let init = sample_init();
    let group = sample_group();

    let accept_task = tokio::spawn(async move {
        let pending = server.accept(group).await.expect("accept");
        pending.handshake(init, HS_TIMEOUT).await
    });

    let mut stream = UnixStream::connect(&path).await.expect("connect");
    write_raw_frame(&mut stream, raw_ready_json.as_bytes()).await;

    accept_task
        .await
        .expect("join accept task")
        .expect_err("handshake must reject malformed ready payload")
}

fn ready_json_with_version(version: &str) -> String {
    // Escape any embedded quote/backslash so we can interpolate
    // arbitrary attacker-controlled inputs.
    let escaped: String = version
        .chars()
        .flat_map(|c| match c {
            '"' => vec!['\\', '"'],
            '\\' => vec!['\\', '\\'],
            _ => vec![c],
        })
        .collect();
    format!(
        r#"{{"type":"ready","adapter":"t","adapter_version":"0","protocol_version":"{escaped}"}}"#
    )
}

#[tokio::test]
async fn rejects_empty_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "empty protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_single_number_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("1")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "missing-dot protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_trailing_dot_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("1.")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "empty-minor protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_non_numeric_minor_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("1.foo")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "non-numeric minor protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_extra_segment_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("1.0.3")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "extra-segment protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_prefix_alpha_protocol_version() {
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("v1.0")).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "alpha-prefixed protocol_version must be rejected as malformed, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_overlong_protocol_version() {
    let overlong = "1.".to_owned() + &"9".repeat(200);
    let err = drive_handshake_with_ready_payload(&ready_json_with_version(&overlong)).await;
    assert!(
        matches!(err, IpcError::Frame(FrameError::MalformedJson(_))),
        "overlong protocol_version must be rejected, got {err:?}"
    );
}

#[tokio::test]
async fn rejects_incompatible_major_via_unsupported_version() {
    // Well-formed but different major — this is the
    // `ProtocolError::UnsupportedVersion` path, distinct from malformed.
    let err = drive_handshake_with_ready_payload(&ready_json_with_version("2.0")).await;
    assert!(
        matches!(
            err,
            IpcError::Protocol(ProtocolError::UnsupportedVersion { .. })
        ),
        "incompatible major protocol_version must be rejected as UnsupportedVersion, got {err:?}"
    );
}
