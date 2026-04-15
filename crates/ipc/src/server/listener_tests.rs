use std::os::unix::net::UnixListener;

use tempfile::tempdir;

use super::*;

#[test]
fn stale_socket_is_unlinked() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("stale.sock");
    // Create a socket and immediately drop the listener (stale).
    let listener = UnixListener::bind(&path).expect("bind");
    drop(listener);
    assert!(path.exists(), "socket file should exist after drop");
    clean_stale_socket(&path).expect("should clean stale socket");
    assert!(!path.exists(), "stale socket should be removed");
}

#[test]
fn live_socket_returns_addr_in_use() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("live.sock");
    let _listener = UnixListener::bind(&path).expect("bind");
    let err = clean_stale_socket(&path).expect_err("should fail");
    assert!(
        matches!(&err, IpcError::Io(e) if e.kind() == std::io::ErrorKind::AddrInUse),
        "expected AddrInUse, got {err:?}"
    );
}

#[test]
fn non_connection_refused_probe_does_not_unlink() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("perm.sock");
    // Create a stale socket, then remove all permissions so the
    // connect probe gets PermissionDenied instead of ConnectionRefused.
    let listener = UnixListener::bind(&path).expect("bind");
    drop(listener);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let err = clean_stale_socket(&path).expect_err("should fail");
    assert!(
        matches!(&err, IpcError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "expected PermissionDenied, got {err:?}"
    );
    // File should NOT have been unlinked.
    assert!(path.exists(), "socket should still exist");
    // Restore permissions for cleanup.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("restore");
}

#[test]
fn missing_path_is_ok() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("nonexistent.sock");
    clean_stale_socket(&path).expect("missing path should succeed");
}
