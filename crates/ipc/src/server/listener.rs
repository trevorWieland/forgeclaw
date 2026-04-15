//! Unix socket listener and bind lifecycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixListener;

use crate::error::{IpcError, ProtocolError};
use crate::message::shared::GroupInfo;
use crate::peer_cred::{self, PeerCredentials, SessionIdentity};
use crate::policy::UnknownTrafficLimitConfig;

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

/// Error returned when peer-credential policy rejects an accepted peer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct PeerCredentialPolicyError {
    /// Human-readable reason for policy rejection.
    pub reason: String,
}

impl PeerCredentialPolicyError {
    /// Create a new policy rejection reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Peer-credential authorization policy for accepted connections.
type PeerCredentialPolicyCallback = dyn Fn(Option<PeerCredentials>, &GroupInfo) -> Result<(), PeerCredentialPolicyError>
    + Send
    + Sync;

/// Peer-credential authorization policy for accepted connections.
#[derive(Default)]
pub enum PeerCredentialPolicy {
    /// Capture credentials for audit context but do not gate accept.
    #[default]
    CaptureOnly,
    /// Require exact UID/GID match when provided.
    RequireExact {
        /// Expected peer UID, if configured.
        uid: Option<u32>,
        /// Expected peer GID, if configured.
        gid: Option<u32>,
    },
    /// Custom caller-provided authorization callback.
    Custom(Arc<PeerCredentialPolicyCallback>),
}

impl std::fmt::Debug for PeerCredentialPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CaptureOnly => f.write_str("CaptureOnly"),
            Self::RequireExact { uid, gid } => f
                .debug_struct("RequireExact")
                .field("uid", uid)
                .field("gid", gid)
                .finish(),
            Self::Custom(_) => f.write_str("Custom(..)"),
        }
    }
}

impl Clone for PeerCredentialPolicy {
    fn clone(&self) -> Self {
        match self {
            Self::CaptureOnly => Self::CaptureOnly,
            Self::RequireExact { uid, gid } => Self::RequireExact {
                uid: *uid,
                gid: *gid,
            },
            Self::Custom(callback) => Self::Custom(Arc::clone(callback)),
        }
    }
}

impl PeerCredentialPolicy {
    fn enforce(&self, creds: Option<PeerCredentials>, group: &GroupInfo) -> Result<(), IpcError> {
        match self {
            Self::CaptureOnly => Ok(()),
            Self::RequireExact { uid, gid } => {
                let observed = creds.map(|cred| (cred.uid, cred.gid));
                if uid.is_some() && *uid != observed.map(|(value, _)| value) {
                    return Err(IpcError::Protocol(ProtocolError::PeerCredentialRejected {
                        reason: "peer uid mismatch".to_owned(),
                        expected_uid: *uid,
                        expected_gid: *gid,
                        actual_uid: observed.map(|(value, _)| value),
                        actual_gid: observed.map(|(_, value)| value),
                    }));
                }
                if gid.is_some() && *gid != observed.map(|(_, value)| value) {
                    return Err(IpcError::Protocol(ProtocolError::PeerCredentialRejected {
                        reason: "peer gid mismatch".to_owned(),
                        expected_uid: *uid,
                        expected_gid: *gid,
                        actual_uid: observed.map(|(value, _)| value),
                        actual_gid: observed.map(|(_, value)| value),
                    }));
                }
                Ok(())
            }
            Self::Custom(callback) => callback(creds, group).map_err(|e| {
                IpcError::Protocol(ProtocolError::PeerCredentialRejected {
                    reason: e.reason,
                    expected_uid: None,
                    expected_gid: None,
                    actual_uid: creds.map(|c| c.uid),
                    actual_gid: creds.map(|c| c.gid),
                })
            }),
        }
    }
}

/// Host-side server options.
#[derive(Debug, Clone)]
pub struct IpcServerOptions {
    /// Unauthorized-command limiter config for accepted connections.
    pub unauthorized_limit: UnauthorizedCommandLimitConfig,
    /// Unknown-message abuse controls for accepted connections.
    ///
    /// Defaults are enabled. Set individual fields to `0` to disable
    /// specific controls explicitly.
    pub unknown_traffic_limit: UnknownTrafficLimitConfig,
    /// Peer-credential authorization policy at accept time.
    pub peer_credential_policy: PeerCredentialPolicy,
    /// Post-handshake write timeout for outbound sends.
    pub write_timeout: Duration,
}

impl IpcServerOptions {
    /// Hardened option preset with strict peer credential matching.
    #[must_use]
    pub fn hardened(uid: Option<u32>, gid: Option<u32>) -> Self {
        Self {
            peer_credential_policy: PeerCredentialPolicy::RequireExact { uid, gid },
            ..Self::default()
        }
    }
}

impl Default for IpcServerOptions {
    fn default() -> Self {
        Self {
            unauthorized_limit: UnauthorizedCommandLimitConfig::default(),
            unknown_traffic_limit: UnknownTrafficLimitConfig::default(),
            peer_credential_policy: PeerCredentialPolicy::default(),
            write_timeout: Duration::from_secs(5),
        }
    }
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
        if let Err(e) = self.options.peer_credential_policy.enforce(creds, &group) {
            tracing::warn!(
                target: "forgeclaw_ipc::server",
                peer_uid = creds.map(|c| c.uid),
                peer_gid = creds.map(|c| c.gid),
                group_id = %group.id.as_ref(),
                group_name = %group.name.as_ref(),
                error = %e,
                "peer-credential policy rejected connection"
            );
            return Err(e);
        }
        let identity = Arc::new(SessionIdentity::new(creds, group));
        Ok(PendingConnection::from_stream(
            stream,
            identity,
            self.options.unauthorized_limit,
            self.options.unknown_traffic_limit,
            self.options.write_timeout,
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
#[path = "listener_tests.rs"]
mod tests;
