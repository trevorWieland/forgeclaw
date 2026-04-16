//! Shared lexical policy for Unix socket paths.
//!
//! This module owns the "does this look like a plausible host-owned
//! socket path" validator so the server and client enforce the same
//! lexical contract. Filesystem-metadata checks (mode, symlink chain,
//! ownership) are **not** in scope here — those are server invariants
//! and live in [`crate::peer_cred::validate_socket_dir`].
//!
//! The server runs this validator as part of
//! [`crate::peer_cred::validate_socket_dir`] before its stricter
//! parent-directory checks. The client runs it standalone inside
//! [`crate::client::IpcClient::connect_with_options`] so that
//! misconfigured adapter sources cannot silently connect through a
//! traversal or non-absolute path.
//!
//! Lexical-only means: this function will **not** touch the
//! filesystem, will **not** resolve symlinks, and will **not** check
//! permissions. It only guarantees the path, as written, is
//! syntactically a valid absolute socket path with no traversal
//! segments and no unsupported prefix.

use std::io;
use std::path::{Component, Path};

use crate::error::IpcError;

/// Validate that `path` is a plausible Unix socket path:
///
/// - Must be absolute.
/// - Must not contain any `.` or `..` traversal segments.
/// - Must not carry an unsupported path prefix (e.g. a Windows drive
///   letter when running on Windows; irrelevant on Unix but rejected
///   for determinism).
///
/// Returns `Ok(())` when the path passes every lexical check. Returns
/// [`IpcError::Io`] with [`io::ErrorKind::InvalidInput`] otherwise so
/// callers can surface the exact reason in their own error-channel.
pub(crate) fn validate_socket_path_shape(path: &Path) -> Result<(), IpcError> {
    if !path.is_absolute() {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("socket path must be absolute: {}", path.display()),
        )));
    }
    if path_contains_explicit_traversal_segment(path) {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path must not contain traversal component `.` or `..`: {}",
                path.display()
            ),
        )));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "socket path must not contain traversal component `{}`: {}",
                        component.as_os_str().to_string_lossy(),
                        path.display()
                    ),
                )));
            }
            Component::Prefix(_) => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported socket path prefix: {}", path.display()),
                )));
            }
            Component::RootDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

/// Detect `.` / `..` segments using a byte-level scan on Unix so path
/// shapes like `foo//..//bar` are still caught even when `Path`
/// normalization swallows the empty segment. Fallback scans the lossy
/// string form on non-Unix where we do not promise portability.
fn path_contains_explicit_traversal_segment(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .any(|segment| segment == b"." || segment == b"..")
    }

    #[cfg(not(unix))]
    {
        path.to_string_lossy()
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    }
}

#[cfg(test)]
mod tests {
    use super::validate_socket_path_shape;
    use std::path::Path;

    #[test]
    fn rejects_relative_path() {
        assert!(validate_socket_path_shape(Path::new("ipc.sock")).is_err());
        assert!(validate_socket_path_shape(Path::new("./ipc.sock")).is_err());
        assert!(validate_socket_path_shape(Path::new("foo/ipc.sock")).is_err());
    }

    #[test]
    fn rejects_traversal_segments() {
        assert!(validate_socket_path_shape(Path::new("/tmp/./ipc.sock")).is_err());
        assert!(validate_socket_path_shape(Path::new("/tmp/../tmp/ipc.sock")).is_err());
        assert!(validate_socket_path_shape(Path::new("/tmp/foo/../ipc.sock")).is_err());
    }

    #[test]
    fn accepts_plain_absolute_path() {
        assert!(validate_socket_path_shape(Path::new("/tmp/ipc.sock")).is_ok());
        assert!(validate_socket_path_shape(Path::new("/var/run/forgeclaw/ipc.sock")).is_ok());
    }
}
