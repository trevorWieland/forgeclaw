//! Platform-specific Unix socket security: peer credential
//! extraction and socket directory hardening.
//!
//! On Linux, uses `SO_PEERCRED` to obtain the peer's uid, gid, and
//! pid. On macOS, uses `LOCAL_PEERCRED` for uid and gid (pid is not
//! available via this mechanism). On unsupported platforms, returns
//! `None`.
//!
//! The extraction is performed via
//! [`tokio::net::UnixStream::peer_cred`], which handles the
//! platform-specific syscall internally — no unsafe code or
//! additional dependencies are needed.

use std::io;
use std::path::Path;

use tokio::net::UnixStream;

use crate::error::IpcError;
use crate::message::shared::GroupInfo;

/// Identity of the peer process on the other end of a Unix socket.
///
/// Available fields depend on the platform:
/// - **Linux**: all three fields populated via `SO_PEERCRED`.
/// - **macOS**: `uid` and `gid` populated via `LOCAL_PEERCRED`;
///   `pid` is `None`.
/// - **Other platforms**: not available (the constructor returns
///   `None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Unix user ID of the peer process.
    pub uid: u32,
    /// Unix group ID of the peer process.
    pub gid: u32,
    /// Process ID of the peer, if available on this platform.
    pub pid: Option<i32>,
}

/// Authenticated session identity for an accepted IPC connection.
///
/// Created at accept time with OS-level peer credentials and the
/// host-authoritative group identity for the session. The handshake
/// normalizes outbound `Init` group metadata to this identity. This
/// type is wrapped in `Arc` so it survives
/// [`into_split`](crate::server::IpcConnection::into_split) and
/// is available on both reader and writer halves.
///
/// On Unix platforms, construction fails if peer credentials
/// cannot be captured (fail-closed). On non-Unix platforms,
/// `peer_credentials` is `None`.
#[derive(Debug, Clone)]
pub struct SessionIdentity {
    /// OS-level peer credentials captured at accept time.
    peer_credentials: Option<PeerCredentials>,
    /// Host-authoritative group identity, bound at accept time.
    group: GroupInfo,
}

impl SessionIdentity {
    /// Create a new session identity from peer credentials and the
    /// host-authoritative group assignment.
    pub(crate) fn new(peer_credentials: Option<PeerCredentials>, group: GroupInfo) -> Self {
        Self {
            peer_credentials,
            group,
        }
    }

    /// Returns the OS-level peer credentials, if available.
    #[must_use]
    pub fn peer_credentials(&self) -> Option<&PeerCredentials> {
        self.peer_credentials.as_ref()
    }

    /// Returns the host-authoritative group identity for this session.
    #[must_use]
    pub fn group(&self) -> &GroupInfo {
        &self.group
    }

    /// Returns `true` if this session belongs to the main group.
    #[must_use]
    pub fn is_main(&self) -> bool {
        self.group.is_main
    }
}

/// Extract peer credentials from an accepted Unix socket.
///
/// Returns `Ok(Some(creds))` on supported platforms, `Ok(None)` on
/// unsupported platforms, and `Err` on I/O failures.
pub(crate) fn peer_credentials(stream: &UnixStream) -> io::Result<Option<PeerCredentials>> {
    peer_credentials_impl(stream)
}

#[cfg(unix)]
fn peer_credentials_impl(stream: &UnixStream) -> io::Result<Option<PeerCredentials>> {
    let ucred = stream.peer_cred()?;
    Ok(Some(PeerCredentials {
        uid: ucred.uid(),
        gid: ucred.gid(),
        pid: ucred.pid(),
    }))
}

#[cfg(not(unix))]
fn peer_credentials_impl(_stream: &UnixStream) -> io::Result<Option<PeerCredentials>> {
    Ok(None)
}

/// Validate that the socket directory is safe for binding.
///
/// Ensures the immediate parent exists (creating it with mode 0o700
/// if missing), then validates every ancestor from the parent up to
/// the filesystem root: no symlinks, no group/other write bits.
///
/// # Residual TOCTOU
///
/// There is a microsecond-scale check-then-bind window between this
/// validation and the subsequent `UnixListener::bind`. Exploiting it
/// requires a same-uid attacker who can replace a validated ancestor
/// with a symlink during that window. Tokio's `UnixListener::bind`
/// accepts only path-based arguments, so an `openat`-style approach
/// that would close this window is not available without unsafe code.
/// The ancestor chain walk narrows the attack surface significantly
/// compared to checking only the immediate parent.
#[cfg(unix)]
pub(crate) fn validate_socket_dir(socket_path: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::PermissionsExt;

    let parent = socket_path.parent().ok_or_else(|| {
        IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has no parent directory",
        ))
    })?;

    // Ensure the immediate parent exists.
    match std::fs::symlink_metadata(parent) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("socket parent is a symlink: {}", parent.display()),
                )));
            }
            if !meta.is_dir() {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("socket parent is not a directory: {}", parent.display()),
                )));
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Parent doesn't exist — safe to create because we own it.
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            tracing::debug!(
                target: "forgeclaw_ipc::server",
                path = %parent.display(),
                "created socket directory with mode 0700"
            );
        }
        Err(e) => return Err(IpcError::Io(e)),
    }

    // First validate the caller-provided lexical chain before any
    // canonicalization so intermediate symlinks cannot be hidden by
    // path resolution.
    validate_lexical_ancestor_chain(parent)?;

    // Then resolve to the real physical directory chain for
    // permission checks. On macOS `/var` and `/tmp` are system
    // symlinks to `/private/*`.
    let canonical = std::fs::canonicalize(parent).map_err(IpcError::Io)?;
    validate_resolved_ancestor_chain(&canonical)
}

/// Walk the original lexical ancestor chain and reject symlinks.
#[cfg(unix)]
fn validate_lexical_ancestor_chain(start: &Path) -> Result<(), IpcError> {
    for ancestor in start.ancestors() {
        // Stop at the filesystem root.
        if ancestor == Path::new("/") || ancestor == Path::new("") {
            break;
        }
        let meta = std::fs::symlink_metadata(ancestor).map_err(IpcError::Io)?;
        if meta.file_type().is_symlink() {
            if allowed_system_symlink_ancestor(ancestor) {
                continue;
            }
            return Err(IpcError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("symlink in socket ancestor chain: {}", ancestor.display()),
            )));
        }
    }
    Ok(())
}

/// Walk the resolved ancestor chain and reject unsafe permissions.
#[cfg(unix)]
fn validate_resolved_ancestor_chain(start: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::PermissionsExt;

    for ancestor in start.ancestors() {
        // Stop at the filesystem root.
        if ancestor == Path::new("/") || ancestor == Path::new("") {
            break;
        }
        let meta = std::fs::symlink_metadata(ancestor).map_err(IpcError::Io)?;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o022 != 0 {
            return Err(IpcError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "socket ancestor {path} has unsafe permissions \
                     {mode:o} (group/other write); use a dedicated \
                     directory with mode 0700",
                    path = ancestor.display(),
                ),
            )));
        }
    }
    Ok(())
}

#[cfg(all(unix, target_os = "macos"))]
fn allowed_system_symlink_ancestor(path: &Path) -> bool {
    path == Path::new("/var") || path == Path::new("/tmp")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn allowed_system_symlink_ancestor(_path: &Path) -> bool {
    false
}
