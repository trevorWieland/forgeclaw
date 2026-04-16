//! Shared test fixtures for IPC integration tests.
//!
//! Extracted out of `handshake_lifecycle.rs` so that new integration
//! files (`versioning_e2e.rs`, `perf_regression.rs`, …) can reuse the
//! setup without pushing any single test file over the 500-line cap.
//!
//! Included per-test-binary via `#[path = "common/mod.rs"] mod common;`.

use std::path::PathBuf;
use std::time::Duration;

use forgeclaw_core::{GroupId, JobId};
use forgeclaw_ipc::{
    GroupCapabilities, GroupInfo, HistoricalMessages, InitConfig, InitContext, InitPayload,
    ReadyPayload,
};

/// Create a socket path inside `dir` under a 0o700 subdirectory named
/// `s` — matches the hardening defaults.
#[cfg(unix)]
pub(crate) fn socket_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let socket_dir = dir.path().join("s");
    std::fs::create_dir_all(&socket_dir).expect("mkdir s");
    std::fs::set_permissions(&socket_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod s");
    let short_name = if name.len() > 32 { &name[..32] } else { name };
    socket_dir.join(short_name)
}

#[cfg(not(unix))]
pub(crate) fn socket_path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

/// A container-side `ready` payload advertising `protocol_version`.
pub(crate) fn sample_ready(version: &str) -> ReadyPayload {
    ReadyPayload {
        adapter: "test-adapter".parse().expect("valid adapter"),
        adapter_version: "0.1.0".parse().expect("valid adapter version"),
        protocol_version: version.parse().expect("valid protocol version"),
    }
}

/// Default handshake deadline used across integration tests.
pub(crate) const HS_TIMEOUT: Duration = Duration::from_secs(5);

/// A sample group identity used as accept-time authoritative identity.
pub(crate) fn sample_group() -> GroupInfo {
    GroupInfo {
        id: GroupId::new("group-main").expect("valid group id"),
        name: "Main".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    }
}

/// A sample `init` payload to reply to the `ready` handshake.
pub(crate) fn sample_init() -> InitPayload {
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
