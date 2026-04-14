//! Socket bind and credential hardening tests.
//!
//! Verifies filesystem permission enforcement, parent-directory
//! safety, and peer credential capture on accepted connections.

use forgeclaw_core::GroupId;
use forgeclaw_ipc::{GroupCapabilities, GroupInfo, IpcClient, IpcServer};
use tempfile::tempdir;

#[cfg(unix)]
#[tokio::test]
async fn bind_creates_socket_with_restricted_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    // Use a subdirectory with known-safe permissions.
    let sub = dir.path().join("safe");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("set mode");
    let path = sub.join("ipc.sock");
    let _server = IpcServer::bind(&path).expect("bind");
    let meta = std::fs::metadata(&path).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket should be owner read/write only");
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_group_writable_parent_directory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o770))
        .expect("set group-writable mode");
    let path = sub.join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject unsafe dir");
    assert!(
        err.to_string().contains("unsafe permissions"),
        "expected unsafe permissions error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_creates_missing_parent_with_safe_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("created_by_ipc");
    let path = sub.join("ipc.sock");
    let _server = IpcServer::bind(&path).expect("bind");
    let meta = std::fs::metadata(&sub).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "created dir should be 0700");
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_intermediate_symlink_ancestor() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real");
    let nested = real.join("nested");
    std::fs::create_dir(&real).expect("create real");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).expect("chmod real");
    std::fs::create_dir(&nested).expect("create nested");
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700))
        .expect("chmod nested");

    let link = dir.path().join("link");
    symlink(&real, &link).expect("create symlink");

    let path = link.join("nested").join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject lexical symlink ancestor");
    assert!(
        err.to_string().contains("symlink in socket ancestor chain"),
        "expected symlink ancestor error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_connection_has_peer_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("cred");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("set mode");
    let path = sub.join("ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let group = GroupInfo {
        id: GroupId::from("group-cred"),
        name: "Cred".to_owned(),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let accept_task = tokio::spawn(async move { server.accept(group).await.expect("accept") });
    let _pending_client = IpcClient::connect(&path).await.expect("connect");
    let conn = accept_task.await.expect("join");
    let id = conn.identity().lock().expect("lock");
    let creds = id.peer_credentials().expect("credentials on unix");
    // The connecting process is us — verify the struct is populated.
    let _ = creds.uid;
    let _ = creds.gid;
}
