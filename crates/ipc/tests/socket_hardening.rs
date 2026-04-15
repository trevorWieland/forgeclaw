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
async fn bind_smoke_succeeds_for_valid_socket_path() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("smoke");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    let server = IpcServer::bind(&path).expect("bind should succeed for valid path");
    assert_eq!(server.path(), path.as_path());
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
        err.to_string().contains("must have mode 0700"),
        "expected strict parent-mode error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_parent_directory_with_mode_0755() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("set mode");
    let path = sub.join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject non-0700 parent");
    assert!(
        err.to_string().contains("must have mode 0700"),
        "expected strict parent-mode error, got: {err}"
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
async fn bind_rejects_missing_parent_through_symlink_without_side_effects() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("create real");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).expect("chmod real");

    let link = dir.path().join("link");
    symlink(&real, &link).expect("create symlink");

    let path = link.join("missing").join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject lexical symlink ancestor");
    assert!(
        err.to_string().contains("symlink in socket ancestor chain"),
        "expected symlink ancestor error, got: {err}"
    );
    assert!(
        !real.join("missing").exists(),
        "bind must fail-closed and not create dirs through symlinked paths"
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
        name: "Cred".parse().expect("valid name"),
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

#[cfg(unix)]
#[tokio::test]
async fn drop_skips_cleanup_when_path_inode_no_longer_matches_listener() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("drop-race");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");
    let parked_path = sub.join("ipc-parked.sock");

    let server = IpcServer::bind(&path).expect("bind");
    assert!(path.exists(), "socket should exist after bind");

    std::fs::rename(&path, &parked_path).expect("rename server socket");
    let replacement = std::os::unix::net::UnixListener::bind(&path).expect("bind replacement");

    drop(server);

    assert!(
        path.exists(),
        "drop must not unlink a socket inode not owned by the listener"
    );

    drop(replacement);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&parked_path);
}
