//! Fail-closed validation of client-side socket paths.
//!
//! Covers the lexical contract shared with the server (absolute
//! path, no `.`/`..`, no unsupported prefix) plus the advisory
//! target-type check (path must be a Unix socket or symlink if it
//! exists at all).

use std::path::Path;

use forgeclaw_ipc::{IpcClient, IpcError};
use tempfile::tempdir;

fn expect_invalid_input(result: Result<(), IpcError>, hint: &str) {
    let err = result.expect_err(hint);
    let kind = match &err {
        IpcError::Io(io_err) => Some(io_err.kind()),
        _ => None,
    };
    assert_eq!(
        kind,
        Some(std::io::ErrorKind::InvalidInput),
        "{hint}: expected IpcError::Io(InvalidInput), got {err:?}"
    );
}

async fn connect_err(path: impl AsRef<Path>) -> Result<(), IpcError> {
    IpcClient::connect(path).await.map(|_| ())
}

#[tokio::test]
async fn rejects_relative_path() {
    expect_invalid_input(connect_err("relative-ipc.sock").await, "relative path");
    expect_invalid_input(connect_err("./nested/ipc.sock").await, "dot-prefixed path");
}

#[tokio::test]
async fn rejects_curdir_traversal() {
    expect_invalid_input(connect_err("/tmp/./ipc.sock").await, "curdir segment");
}

#[tokio::test]
async fn rejects_parentdir_traversal() {
    expect_invalid_input(
        connect_err("/tmp/foo/../ipc.sock").await,
        "parentdir segment",
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_regular_file_at_target() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("not-a-socket.txt");
    std::fs::write(&path, "hello").expect("create regular file");
    expect_invalid_input(connect_err(&path).await, "regular file at path");
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_directory_at_target() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("a-directory");
    std::fs::create_dir(&path).expect("create directory");
    expect_invalid_input(connect_err(&path).await, "directory at path");
}

#[cfg(unix)]
#[tokio::test]
async fn missing_path_falls_through_to_connect_error() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("does-not-exist.sock");
    // The lexical check passes (absolute, no traversal) and the
    // target-type check skips when the path does not exist, so the
    // connect syscall surfaces a NotFound / ConnectionRefused rather
    // than our InvalidInput.
    let err = IpcClient::connect(&path)
        .await
        .expect_err("connect must fail when no listener exists");
    let kind = match &err {
        IpcError::Io(io_err) => Some(io_err.kind()),
        _ => None,
    };
    assert!(
        kind.is_some(),
        "missing path must surface as IpcError::Io, got {err:?}"
    );
    assert_ne!(
        kind,
        Some(std::io::ErrorKind::InvalidInput),
        "missing path should fall through to connect, not be rejected lexically: {err:?}"
    );
}
