use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::IpcError;

/// Validate that the socket directory is safe for binding.
pub(crate) fn validate_socket_dir(socket_path: &Path) -> Result<(), IpcError> {
    validate_socket_path_input(socket_path)?;
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
    validate_parent_canonical_identity(parent, &canonical_parent)
}

fn validate_socket_path_input(socket_path: &Path) -> Result<(), IpcError> {
    if !socket_path.is_absolute() {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("socket path must be absolute: {}", socket_path.display()),
        )));
    }
    if socket_path_contains_explicit_traversal_segment(socket_path) {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path must not contain traversal component `.` or `..`: {}",
                socket_path.display()
            ),
        )));
    }
    for component in socket_path.components() {
        match component {
            Component::ParentDir | Component::CurDir => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "socket path must not contain traversal component `{}`: {}",
                        component.as_os_str().to_string_lossy(),
                        socket_path.display()
                    ),
                )));
            }
            Component::Prefix(_) => {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported socket path prefix: {}", socket_path.display()),
                )));
            }
            Component::RootDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

fn socket_path_contains_explicit_traversal_segment(socket_path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        socket_path
            .as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .any(|segment| segment == b"." || segment == b"..")
    }

    #[cfg(not(unix))]
    {
        // Windows paths are not supported by this socket hardening module,
        // but keep behavior deterministic for tests.
        socket_path
            .to_string_lossy()
            .split('/')
            .any(|segment| segment == "." || segment == "..")
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
