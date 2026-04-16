//! Platform-specific Unix socket security: peer credential extraction,
//! socket directory hardening, and bind-path attestation.
//!
//! On Linux, peer credentials use `SO_PEERCRED`; on macOS they use
//! `LOCAL_PEERCRED` (without PID). On unsupported platforms,
//! credentials are unavailable and return `None`.

use std::io;

use tokio::net::UnixStream;

use crate::error::IpcError;
use crate::message::shared::GroupInfo;

#[cfg(unix)]
mod bind_attestation;
#[cfg(unix)]
mod socket_dir;

#[cfg(unix)]
pub(crate) use bind_attestation::{
    attest_post_bind, capture_bind_attestation, listener_matches_socket_definitive,
    socket_path_fingerprint,
};
#[cfg(unix)]
pub use socket_dir::ParentOwnerPolicy;
#[cfg(unix)]
pub(crate) use socket_dir::validate_socket_dir;

/// Identity of the peer process on the other end of a Unix socket.
///
/// Available fields depend on the platform:
/// - Linux: `uid`, `gid`, and `pid`.
/// - macOS: `uid` and `gid` (`pid` is unavailable).
/// - Other platforms: unavailable.
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

/// Look up the effective UID of the calling process without any
/// `libc`/`unsafe` dependency.
///
/// Implementation detail: opens a transient tokio `UnixStream::pair()`
/// and asks the kernel for our own peer credentials; both halves are
/// dropped immediately so no fds or tasks leak. `peer_cred` on a tokio
/// `UnixStream` is synchronous and does not require the reactor to be
/// active — registration is deferred until the first poll.
#[cfg(unix)]
pub(crate) fn effective_uid() -> Result<u32, IpcError> {
    let (a, _b) = UnixStream::pair().map_err(IpcError::Io)?;
    let ucred = a.peer_cred().map_err(IpcError::Io)?;
    Ok(ucred.uid())
}
