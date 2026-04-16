//! Peer-credential authorization policy for accepted IPC connections.
//!
//! `IpcServer::bind` defaults to
//! [`PeerCredentialPolicy::MatchCapturedProcess`] via
//! [`IpcServerOptions::hardened_current_process`]. This module owns
//! the policy enum itself and the bind-time UID/GID snapshot so the
//! listener module can stay focused on the socket lifecycle.

use std::sync::Arc;

use crate::error::{IpcError, ProtocolError};
use crate::message::shared::GroupInfo;
use crate::peer_cred::PeerCredentials;

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
///
/// **Policy selection is required.** There is intentionally no
/// `Default` impl: operators must pick one of the explicit variants
/// (typically via [`crate::server::IpcServerOptions::hardened_current_process`]
/// or [`crate::server::IpcServerOptions::hardened`]) so policy drift
/// cannot silently leave a deployment permissive.
pub enum PeerCredentialPolicy {
    /// Capture credentials for audit context but do not gate accept.
    /// Use only when host-side socket hardening is not viable;
    /// preferably gated behind a named, audit-visible constructor.
    CaptureOnly,
    /// Require exact UID/GID match when provided.
    RequireExact {
        /// Expected peer UID, if configured.
        uid: Option<u32>,
        /// Expected peer GID, if configured.
        gid: Option<u32>,
    },
    /// Require the peer's UID and GID to match a snapshot captured at
    /// bind time. This is the fail-closed default selected by
    /// [`crate::server::IpcServer::bind`] so that typical deployments
    /// enforce same-process-identity without requiring explicit
    /// opt-in.
    MatchCapturedProcess {
        /// UID snapped at bind-time (usually the host's own UID).
        captured_uid: u32,
        /// GID snapped at bind-time (usually the host's own GID).
        captured_gid: u32,
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
            Self::MatchCapturedProcess {
                captured_uid,
                captured_gid,
            } => f
                .debug_struct("MatchCapturedProcess")
                .field("captured_uid", captured_uid)
                .field("captured_gid", captured_gid)
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
            Self::MatchCapturedProcess {
                captured_uid,
                captured_gid,
            } => Self::MatchCapturedProcess {
                captured_uid: *captured_uid,
                captured_gid: *captured_gid,
            },
            Self::Custom(callback) => Self::Custom(Arc::clone(callback)),
        }
    }
}

impl PeerCredentialPolicy {
    /// Construct a hardened policy that snaps the current process's
    /// UID and GID as the expected peer identity. Call this inside the
    /// same privilege context as [`crate::server::IpcServer::bind`];
    /// the snapshot happens at call time, not at accept time.
    ///
    /// Fails if the platform cannot report our own credentials (e.g.
    /// sandbox restrictions) — in that case the caller must pick an
    /// explicit policy rather than silently falling back to
    /// `CaptureOnly`.
    #[cfg(unix)]
    pub fn match_current_process() -> Result<Self, IpcError> {
        // Snap our own UID/GID without `libc` FFI (forbidden by the
        // workspace) by creating a transient tokio socketpair and
        // asking the OS for our peer credentials. `peer_cred` on a
        // tokio `UnixStream` is synchronous and does not require the
        // reactor to be active — registration is deferred until the
        // first poll. Both halves are dropped immediately so no fds,
        // registrations, or tasks leak.
        let (a, _b) = tokio::net::UnixStream::pair().map_err(IpcError::Io)?;
        let ucred = a.peer_cred().map_err(IpcError::Io)?;
        Ok(Self::MatchCapturedProcess {
            captured_uid: ucred.uid(),
            captured_gid: ucred.gid(),
        })
    }

    pub(super) fn enforce(
        &self,
        creds: Option<PeerCredentials>,
        group: &GroupInfo,
    ) -> Result<(), IpcError> {
        match self {
            Self::CaptureOnly => Ok(()),
            Self::RequireExact { uid, gid } => enforce_require_exact(creds, *uid, *gid),
            Self::MatchCapturedProcess {
                captured_uid,
                captured_gid,
            } => enforce_match_captured(creds, (*captured_uid, *captured_gid)),
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

fn enforce_require_exact(
    creds: Option<PeerCredentials>,
    uid: Option<u32>,
    gid: Option<u32>,
) -> Result<(), IpcError> {
    let observed = creds.map(|cred| (cred.uid, cred.gid));
    if uid.is_some() && uid != observed.map(|(value, _)| value) {
        return Err(IpcError::Protocol(ProtocolError::PeerCredentialRejected {
            reason: "peer uid mismatch".to_owned(),
            expected_uid: uid,
            expected_gid: gid,
            actual_uid: observed.map(|(value, _)| value),
            actual_gid: observed.map(|(_, value)| value),
        }));
    }
    if gid.is_some() && gid != observed.map(|(_, value)| value) {
        return Err(IpcError::Protocol(ProtocolError::PeerCredentialRejected {
            reason: "peer gid mismatch".to_owned(),
            expected_uid: uid,
            expected_gid: gid,
            actual_uid: observed.map(|(value, _)| value),
            actual_gid: observed.map(|(_, value)| value),
        }));
    }
    Ok(())
}

fn enforce_match_captured(
    creds: Option<PeerCredentials>,
    captured: (u32, u32),
) -> Result<(), IpcError> {
    let observed = creds.map(|cred| (cred.uid, cred.gid));
    if observed == Some(captured) {
        return Ok(());
    }
    Err(IpcError::Protocol(ProtocolError::PeerCredentialRejected {
        reason: "peer uid/gid does not match bind-time snapshot".to_owned(),
        expected_uid: Some(captured.0),
        expected_gid: Some(captured.1),
        actual_uid: observed.map(|(value, _)| value),
        actual_gid: observed.map(|(_, value)| value),
    }))
}
