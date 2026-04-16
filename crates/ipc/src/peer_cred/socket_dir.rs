use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::IpcError;
use crate::path_policy::validate_socket_path_shape;

/// Policy governing which UID is allowed to own the socket's parent
/// directory at bind and post-bind attestation time.
///
/// `IpcServer::bind` defaults to [`Self::MatchEffectiveUid`], so a
/// path whose parent is owned by a different UID fails closed before
/// `UnixListener::bind` is ever called. Operators who need to bind
/// under a dedicated service account can name it explicitly via
/// [`Self::RequireUid`]. [`Self::Disabled`] is audit-visible (logs a
/// warn on every bind) and intended only for constrained
/// deployments that cannot guarantee ownership — the matching
/// [`crate::server::IpcServerOptions::insecure_capture_only`]
/// constructor selects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentOwnerPolicy {
    /// Parent UID must match the effective UID of the running process,
    /// sampled at bind time via a crate-internal `peer_cred` helper.
    MatchEffectiveUid,
    /// Parent UID must equal the explicitly supplied value.
    RequireUid(u32),
    /// Ownership check is disabled. Emits a `tracing::warn` on first
    /// bind so operators can audit the posture.
    Disabled,
}

impl ParentOwnerPolicy {
    /// Returns the UID this policy expects the parent to have, if the
    /// policy enforces a specific UID.
    ///
    /// For `MatchEffectiveUid`, this samples the running process's
    /// effective UID via `peer_cred::effective_uid` (a crate-internal
    /// helper that opens a transient Unix socketpair to read
    /// `peer_cred` without `libc` FFI).
    fn expected_uid(self) -> Result<Option<u32>, IpcError> {
        match self {
            Self::MatchEffectiveUid => Ok(Some(super::effective_uid()?)),
            Self::RequireUid(uid) => Ok(Some(uid)),
            Self::Disabled => Ok(None),
        }
    }
}

static DISABLED_WARN_LOGGED: AtomicBool = AtomicBool::new(false);

fn log_disabled_warn_once() {
    if DISABLED_WARN_LOGGED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        tracing::warn!(
            target: "forgeclaw_ipc::server",
            "socket parent ownership check is DISABLED — bind posture is permissive"
        );
    }
}

/// Validate that the socket directory is safe for binding.
pub(crate) fn validate_socket_dir(
    socket_path: &Path,
    owner_policy: ParentOwnerPolicy,
) -> Result<(), IpcError> {
    validate_socket_path_shape(socket_path)?;
    let parent = socket_path.parent().ok_or_else(|| {
        IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has no parent directory",
        ))
    })?;

    let (nearest_existing_lexical, nearest_existing_canonical) = nearest_existing_ancestor(parent)?;
    validate_lexical_ancestor_chain(&nearest_existing_lexical)?;
    let relative = normalized_relative_components(parent, &nearest_existing_lexical)?;
    let canonical_parent = canonical_parent_path(&nearest_existing_canonical, &relative);
    create_missing_parent_segments(&nearest_existing_canonical, &relative)?;
    validate_parent_directory_shape(&canonical_parent)?;
    validate_parent_directory_mode(&canonical_parent)?;
    validate_parent_canonical_identity(parent, &canonical_parent)?;
    validate_parent_directory_owner(&canonical_parent, owner_policy)
}

pub(crate) fn validate_parent_directory_owner(
    parent: &Path,
    policy: ParentOwnerPolicy,
) -> Result<(), IpcError> {
    use std::os::unix::fs::MetadataExt;

    match policy.expected_uid()? {
        None => {
            log_disabled_warn_once();
            Ok(())
        }
        Some(expected_uid) => {
            let meta = std::fs::symlink_metadata(parent).map_err(IpcError::Io)?;
            let actual_uid = meta.uid();
            if actual_uid == expected_uid {
                Ok(())
            } else {
                Err(IpcError::SocketParentOwnership {
                    path: parent.display().to_string(),
                    expected_uid,
                    actual_uid,
                })
            }
        }
    }
}

fn nearest_existing_ancestor(path: &Path) -> Result<(PathBuf, PathBuf), IpcError> {
    for ancestor in path.ancestors() {
        if ancestor == Path::new("") {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let canonical = std::fs::canonicalize(ancestor).map_err(IpcError::Io)?;
                return Ok((ancestor.to_path_buf(), canonical));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(IpcError::Io(e)),
        }
    }
    Err(IpcError::Io(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("socket parent has no existing ancestor: {}", path.display()),
    )))
}

fn normalized_relative_components(parent: &Path, start: &Path) -> Result<Vec<PathBuf>, IpcError> {
    if parent == start {
        return Ok(Vec::new());
    }
    let relative = parent.strip_prefix(start).map_err(|e| {
        IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket parent {} is not under existing ancestor {}: {e}",
                parent.display(),
                start.display()
            ),
        ))
    })?;

    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(normal) => parts.push(PathBuf::from(normal)),
            Component::CurDir | Component::ParentDir => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "invalid path traversal segment in socket parent: {}",
                        parent.display()
                    ),
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid socket parent path shape: {}", parent.display()),
                )));
            }
        }
    }
    Ok(parts)
}

fn canonical_parent_path(start_canonical: &Path, parts: &[PathBuf]) -> PathBuf {
    let mut current = start_canonical.to_path_buf();
    for part in parts {
        current.push(part);
    }
    current
}

fn create_missing_parent_segments(start: &Path, parts: &[PathBuf]) -> Result<(), IpcError> {
    use std::os::unix::fs::PermissionsExt;

    if parts.is_empty() {
        return Ok(());
    }

    let mut current = start.to_path_buf();
    for component in parts {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(IpcError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("socket parent segment is a symlink: {}", current.display()),
                    )));
                }
                if !meta.is_dir() {
                    return Err(IpcError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "socket parent segment is not a directory: {}",
                            current.display()
                        ),
                    )));
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o700))?;
                tracing::debug!(
                    target: "forgeclaw_ipc::server",
                    path = %current.display(),
                    "created socket directory segment with mode 0700"
                );
            }
            Err(e) => return Err(IpcError::Io(e)),
        }
    }
    Ok(())
}

fn validate_parent_canonical_identity(
    parent: &Path,
    canonical_parent: &Path,
) -> Result<(), IpcError> {
    let resolved_parent = std::fs::canonicalize(parent).map_err(IpcError::Io)?;
    if resolved_parent != canonical_parent {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket parent canonical identity mismatch: expected {}, got {}",
                canonical_parent.display(),
                resolved_parent.display()
            ),
        )));
    }
    Ok(())
}

fn validate_lexical_ancestor_chain(start: &Path) -> Result<(), IpcError> {
    for ancestor in start.ancestors() {
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

fn validate_parent_directory_shape(parent: &Path) -> Result<(), IpcError> {
    let meta = std::fs::symlink_metadata(parent).map_err(IpcError::Io)?;
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
    Ok(())
}

fn validate_parent_directory_mode(parent: &Path) -> Result<(), IpcError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::symlink_metadata(parent)
        .map_err(IpcError::Io)?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o700 {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "socket parent {path} must have mode 0700 (found {mode:o})",
                path = parent.display(),
            ),
        )));
    }
    Ok(())
}

fn symlink_resolves_to(path: &Path, expected: &Path) -> bool {
    std::fs::canonicalize(path)
        .map(|resolved| resolved == expected)
        .unwrap_or(false)
}

#[cfg(all(unix, target_os = "macos"))]
fn allowed_system_symlink_ancestor(path: &Path) -> bool {
    (path == Path::new("/var") && symlink_resolves_to(path, Path::new("/private/var")))
        || (path == Path::new("/tmp") && symlink_resolves_to(path, Path::new("/private/tmp")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn allowed_system_symlink_ancestor(path: &Path) -> bool {
    (path == Path::new("/var/run") && symlink_resolves_to(path, Path::new("/run")))
        || (path == Path::new("/var/lock") && symlink_resolves_to(path, Path::new("/run/lock")))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::allowed_system_symlink_ancestor;

    #[cfg(all(unix, target_os = "macos"))]
    #[test]
    fn macos_allows_vetted_system_symlinks_when_present() {
        for path in ["/var", "/tmp"] {
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                if meta.file_type().is_symlink() {
                    assert!(
                        allowed_system_symlink_ancestor(Path::new(path)),
                        "expected {path} to be an allowed system symlink"
                    );
                }
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_allows_vetted_system_symlinks_when_present() {
        for path in ["/var/run", "/var/lock"] {
            if let Ok(meta) = std::fs::symlink_metadata(path) {
                if meta.file_type().is_symlink() {
                    assert!(
                        allowed_system_symlink_ancestor(Path::new(path)),
                        "expected {path} to be an allowed system symlink"
                    );
                }
            }
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_rejects_non_vetted_symlink_paths() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;

        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("mkdir target");
        let link = dir.path().join("link");
        symlink(&target, &link).expect("symlink");
        assert!(
            !allowed_system_symlink_ancestor(&link),
            "temporary symlink should not be allowlisted"
        );
    }
}
