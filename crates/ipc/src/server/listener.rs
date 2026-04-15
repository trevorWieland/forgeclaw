//! Unix socket listener and bind lifecycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;

use crate::error::IpcError;
use crate::message::shared::GroupInfo;
use crate::peer_cred::{self, SessionIdentity};

use super::PendingConnection;

/// Per-connection unauthorized-command abuse controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnauthorizedCommandLimitConfig {
    /// Maximum immediate unauthorized attempts before refill/backoff.
    pub burst_capacity: u32,
    /// Refill rate in tokens per second.
    pub refill_per_second: u32,
    /// Delay applied after each exhausted-budget strike.
    pub backoff: Duration,
    /// Disconnect after this many exhausted-budget strikes.
    pub disconnect_after_strikes: u32,
}

impl Default for UnauthorizedCommandLimitConfig {
    fn default() -> Self {
        Self {
            burst_capacity: 8,
            refill_per_second: 2,
            backoff: Duration::from_millis(200),
            disconnect_after_strikes: 5,
        }
    }
}

/// Host-side server options.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct IpcServerOptions {
    /// Unauthorized-command limiter config for accepted connections.
    pub unauthorized_limit: UnauthorizedCommandLimitConfig,
}

/// Unix-socket server for the host side of the IPC protocol.
///
/// An [`IpcServer`] owns a single [`tokio::net::UnixListener`] and the
/// filesystem path the listener is bound to. Accepted peers are
/// returned as [`PendingConnection`]s that must complete the
/// handshake before becoming full [`super::IpcConnection`]s.
#[derive(Debug)]
pub struct IpcServer {
    listener: UnixListener,
    socket_path: PathBuf,
    #[cfg(unix)]
    socket_fingerprint: (u64, u64),
    options: IpcServerOptions,
}

impl IpcServer {
    /// Bind a Unix socket at `path`.
    ///
    /// If a **stale Unix socket** already exists at the path, it is
    /// removed first so that a host restart succeeds without manual
    /// intervention. If the socket is actively served by another
    /// process, the call fails with [`std::io::ErrorKind::AddrInUse`].
    /// If a non-socket file (regular file, directory, symlink, etc.)
    /// exists at the path, the call fails with
    /// [`std::io::ErrorKind::AlreadyExists`] rather than silently
    /// deleting an unrelated file.
    ///
    /// On Unix, the parent directory is validated before binding:
    /// - Must be a real directory (not a symlink).
    /// - Must have mode 0o700.
    /// - Ancestors must not be symlinks except vetted system aliases
    ///   (platform-dependent).
    /// - If it does not exist, it is created with mode 0o700.
    ///
    /// The socket file itself is set to mode 0o600 after binding.
    ///
    /// Post-bind listener/path attestation is fail-closed: mismatch or
    /// inconclusive identity evidence is treated as a bind-race error.
    pub fn bind(path: impl AsRef<Path>) -> Result<Self, IpcError> {
        Self::bind_with_options(path, IpcServerOptions::default())
    }

    /// Bind a Unix socket at `path` with explicit server options.
    ///
    /// See [`Self::bind`] for bind and hardening semantics.
    pub fn bind_with_options(
        path: impl AsRef<Path>,
        options: IpcServerOptions,
    ) -> Result<Self, IpcError> {
        let socket_path = path.as_ref().to_path_buf();
        // `bind` is a startup-time operation so the short blocking
        // metadata check + unlink here is fine.
        #[cfg(unix)]
        peer_cred::validate_socket_dir(&socket_path)?;
        #[cfg(unix)]
        let bind_attestation = peer_cred::capture_bind_attestation(&socket_path)?;
        clean_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        #[cfg(unix)]
        let socket_fingerprint = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
            if let Err(e) = peer_cred::attest_post_bind(&socket_path, &listener, &bind_attestation)
            {
                cleanup_listener_owned_socket(
                    &socket_path,
                    &listener,
                    "bind_attestation_failure",
                    None,
                );
                return Err(e);
            }
            peer_cred::socket_path_fingerprint(&socket_path)?
        };
        tracing::debug!(
            target: "forgeclaw_ipc::server",
            path = %socket_path.display(),
            "bound IPC server"
        );
        Ok(Self {
            listener,
            socket_path,
            #[cfg(unix)]
            socket_fingerprint,
            options,
        })
    }

    /// Accept one container connection with a host-authoritative
    /// group identity.
    ///
    /// Returns a [`PendingConnection`] that must complete the
    /// handshake before send/recv operations become available.
    ///
    /// The `group` parameter is the identity the host assigns to this
    /// connection — it is bound at accept time so that no later code
    /// path can inject a different identity.
    ///
    /// On Unix, peer credential capture is **fail-closed**: if the OS
    /// refuses or fails to attest the peer, the accept is rejected.
    /// On non-Unix platforms, `None` credentials are expected and
    /// accepted.
    pub async fn accept(&self, group: GroupInfo) -> Result<PendingConnection, IpcError> {
        let (stream, _addr) = self.listener.accept().await?;
        let creds = peer_cred::peer_credentials(&stream).map_err(|e| {
            tracing::warn!(
                target: "forgeclaw_ipc::server",
                error = %e,
                "peer credential capture failed — rejecting connection"
            );
            IpcError::Io(e)
        })?;
        let identity = Arc::new(std::sync::Mutex::new(SessionIdentity::new(creds, group)));
        Ok(PendingConnection::from_stream(
            stream,
            identity,
            self.options.unauthorized_limit,
        ))
    }

    /// Returns the filesystem path this server is bound to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(unix)]
fn cleanup_listener_owned_socket(
    path: &Path,
    listener: &UnixListener,
    operation: &'static str,
    expected_fingerprint: Option<(u64, u64)>,
) {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;

    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_socket() => {
            tracing::warn!(
                target: "forgeclaw_ipc::server",
                path = %path.display(),
                operation,
                "socket path replaced with non-socket; skipping cleanup"
            );
            return;
        }
        Ok(meta) => {
            if let Some((expected_dev, expected_ino)) = expected_fingerprint {
                let current = (meta.dev(), meta.ino());
                if current != (expected_dev, expected_ino) {
                    tracing::warn!(
                        target: "forgeclaw_ipc::server",
                        path = %path.display(),
                        operation,
                        expected_dev,
                        expected_ino,
                        current_dev = current.0,
                        current_ino = current.1,
                        "socket path inode does not match bind-time listener socket; skipping cleanup"
                    );
                    return;
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(
                target: "forgeclaw_ipc::server",
                path = %path.display(),
                operation,
                error = %e,
                "failed to stat IPC socket file before cleanup"
            );
            return;
        }
    }

    if !peer_cred::listener_matches_socket_definitive(listener, path) {
        tracing::warn!(
            target: "forgeclaw_ipc::server",
            path = %path.display(),
            operation,
            "socket path inode does not match active listener; skipping cleanup"
        );
        return;
    }

    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                target: "forgeclaw_ipc::server",
                path = %path.display(),
                operation,
                error = %e,
                "failed to remove IPC socket file"
            );
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            cleanup_listener_owned_socket(
                &self.socket_path,
                &self.listener,
                "server_drop",
                Some(self.socket_fingerprint),
            );
        }
    }
}

/// Inspect the filesystem at `path` and remove a stale Unix socket
/// if present. Errors on non-socket entries; ignores missing paths.
#[cfg(unix)]
fn clean_stale_socket(path: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::FileTypeExt;

    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            // Liveness probe: try connecting to detect an active listener.
            // Only `ConnectionRefused` proves no listener exists (stale).
            // Any other probe failure (e.g. `PermissionDenied`) is
            // ambiguous — do NOT unlink, surface the error instead.
            match std::os::unix::net::UnixStream::connect(path) {
                Ok(_) => Err(IpcError::Io(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!(
                        "socket already in use by another process: {}",
                        path.display()
                    ),
                ))),
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path)?;
                    tracing::debug!(
                        target: "forgeclaw_ipc::server",
                        path = %path.display(),
                        "removed stale socket file"
                    );
                    Ok(())
                }
                Err(e) => Err(IpcError::Io(e)),
            }
        }
        Ok(_) => Err(IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("path exists and is not a Unix socket: {}", path.display()),
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IpcError::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use tempfile::tempdir;

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
        // connect probe gets PermissionDenied instead of
        // ConnectionRefused.
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
}
