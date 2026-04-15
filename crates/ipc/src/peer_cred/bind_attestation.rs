use std::io;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use crate::error::IpcError;

use super::validate_socket_dir;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

/// Snapshot of parent-directory identity captured before `bind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParentFingerprint {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}

/// Pre-bind attestation context used to detect bind-path races.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BindAttestation {
    parent: PathBuf,
    parent_fingerprint: ParentFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListenerSocketAttestation {
    Match,
    Mismatch(String),
    Inconclusive(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttestationEvidence {
    Match,
    Mismatch(String),
    Inconclusive(String),
}

pub(crate) fn capture_bind_attestation(socket_path: &Path) -> Result<BindAttestation, IpcError> {
    let parent = socket_path.parent().ok_or_else(|| {
        IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has no parent directory",
        ))
    })?;
    Ok(BindAttestation {
        parent: parent.to_path_buf(),
        parent_fingerprint: parent_fingerprint(parent)?,
    })
}

pub(crate) fn socket_path_fingerprint(socket_path: &Path) -> Result<(u64, u64), IpcError> {
    socket_fingerprint(socket_path)
}

pub(crate) fn listener_matches_socket_definitive(
    listener: &UnixListener,
    socket_path: &Path,
) -> bool {
    let Ok(path_fingerprint) = socket_fingerprint(socket_path) else {
        return false;
    };
    matches!(
        listener_inode_evidence(listener, path_fingerprint),
        AttestationEvidence::Match
    )
}

pub(crate) fn attest_post_bind(
    socket_path: &Path,
    listener: &UnixListener,
    attestation: &BindAttestation,
) -> Result<(), IpcError> {
    let current_parent = parent_fingerprint(&attestation.parent)?;
    if current_parent != attestation.parent_fingerprint {
        return Err(IpcError::BindRace(format!(
            "socket parent fingerprint changed during bind: {}",
            attestation.parent.display()
        )));
    }

    validate_socket_dir(socket_path)?;

    let path_fingerprint = socket_fingerprint(socket_path)?;
    map_attestation_verdict(
        socket_path,
        listener_socket_attestation(listener, socket_path, path_fingerprint),
    )
}

fn map_attestation_verdict(
    socket_path: &Path,
    verdict: ListenerSocketAttestation,
) -> Result<(), IpcError> {
    match verdict {
        ListenerSocketAttestation::Match => Ok(()),
        ListenerSocketAttestation::Mismatch(reason) => Err(IpcError::BindRace(format!(
            "listener/path attestation mismatch after bind at {}: {reason}",
            socket_path.display()
        ))),
        ListenerSocketAttestation::Inconclusive(reason) => Err(IpcError::BindRace(format!(
            "listener/path attestation inconclusive after bind at {}: {reason}",
            socket_path.display()
        ))),
    }
}

fn listener_socket_attestation(
    listener: &UnixListener,
    socket_path: &Path,
    path_fingerprint: (u64, u64),
) -> ListenerSocketAttestation {
    let path_evidence = listener_path_evidence(listener, socket_path);
    let inode_evidence = listener_inode_evidence(listener, path_fingerprint);
    merge_evidence(path_evidence, inode_evidence)
}

fn merge_evidence(
    path_evidence: AttestationEvidence,
    inode_evidence: AttestationEvidence,
) -> ListenerSocketAttestation {
    match (path_evidence, inode_evidence) {
        (AttestationEvidence::Match, AttestationEvidence::Match) => {
            ListenerSocketAttestation::Match
        }
        (AttestationEvidence::Match, AttestationEvidence::Mismatch(reason))
        | (AttestationEvidence::Mismatch(reason), AttestationEvidence::Match) => {
            ListenerSocketAttestation::Mismatch(format!(
                "conflicting listener/path identity evidence: {reason}"
            ))
        }
        (AttestationEvidence::Match, AttestationEvidence::Inconclusive(reason))
        | (AttestationEvidence::Inconclusive(reason), AttestationEvidence::Match) => {
            ListenerSocketAttestation::Inconclusive(format!(
                "partial listener/path identity evidence (one probe unavailable): {reason}"
            ))
        }
        (AttestationEvidence::Mismatch(reason), AttestationEvidence::Inconclusive(_))
        | (AttestationEvidence::Inconclusive(_), AttestationEvidence::Mismatch(reason)) => {
            ListenerSocketAttestation::Mismatch(reason)
        }
        (
            AttestationEvidence::Mismatch(path_reason),
            AttestationEvidence::Mismatch(inode_reason),
        ) => ListenerSocketAttestation::Mismatch(format!("{path_reason}; {inode_reason}")),
        (
            AttestationEvidence::Inconclusive(path_reason),
            AttestationEvidence::Inconclusive(inode_reason),
        ) => ListenerSocketAttestation::Inconclusive(format!("{path_reason}; {inode_reason}")),
    }
}

fn listener_path_evidence(listener: &UnixListener, socket_path: &Path) -> AttestationEvidence {
    let mut saw_comparable_path = false;
    let mut mismatch_reason: Option<String> = None;

    if let Ok(addr) = listener.local_addr() {
        if let Some(bound_path) = addr.as_pathname() {
            match canonical_path_match(bound_path, socket_path) {
                Some(true) => return AttestationEvidence::Match,
                Some(false) => {
                    saw_comparable_path = true;
                    mismatch_reason = Some(format!(
                        "listener local address differs from socket path ({})",
                        bound_path.display()
                    ));
                }
                None => {}
            }
        }
    }

    for target in listener_fd_targets(listener) {
        match canonical_path_match(&target, socket_path) {
            Some(true) => return AttestationEvidence::Match,
            Some(false) => {
                saw_comparable_path = true;
                mismatch_reason = Some(format!(
                    "listener fd target differs from socket path ({})",
                    target.display()
                ));
            }
            None => {}
        }
    }

    if saw_comparable_path {
        AttestationEvidence::Mismatch(
            mismatch_reason
                .unwrap_or_else(|| "listener path evidence disagrees with socket path".to_owned()),
        )
    } else {
        AttestationEvidence::Inconclusive(
            "could not derive comparable listener pathname evidence".to_owned(),
        )
    }
}

fn canonical_path_match(left: &Path, right: &Path) -> Option<bool> {
    if left == right {
        return Some(true);
    }
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left_real), Ok(right_real)) => Some(left_real == right_real),
        _ => None,
    }
}

fn listener_fd_targets(listener: &UnixListener) -> Vec<PathBuf> {
    use std::os::fd::AsRawFd;

    let fd = listener.as_raw_fd();
    let mut targets = Vec::new();
    for fd_path in [format!("/proc/self/fd/{fd}"), format!("/dev/fd/{fd}")] {
        if let Ok(target) = std::fs::read_link(fd_path) {
            targets.push(target);
        }
    }
    targets
}

fn listener_inode_evidence(
    listener: &UnixListener,
    path_fingerprint: (u64, u64),
) -> AttestationEvidence {
    match listener_fingerprint(listener) {
        Some(listener_fp) if listener_fp == path_fingerprint => AttestationEvidence::Match,
        Some((dev, ino)) => AttestationEvidence::Mismatch(format!(
            "listener inode ({dev},{ino}) differs from socket inode ({},{})",
            path_fingerprint.0, path_fingerprint.1
        )),
        None => AttestationEvidence::Inconclusive(
            "could not fingerprint listener file descriptor path after bind".to_owned(),
        ),
    }
}

fn listener_fingerprint(listener: &UnixListener) -> Option<(u64, u64)> {
    if let Ok(addr) = listener.local_addr() {
        if let Some(bound_path) = addr.as_pathname() {
            if let Some(fp) = path_fingerprint_if_socket(bound_path) {
                return Some(fp);
            }
        }
    }

    for target in listener_fd_targets(listener) {
        if let Some(fp) = path_fingerprint_if_socket(&target) {
            return Some(fp);
        }
    }
    None
}

fn parent_fingerprint(parent: &Path) -> Result<ParentFingerprint, IpcError> {
    let meta = std::fs::symlink_metadata(parent).map_err(IpcError::Io)?;
    Ok(ParentFingerprint {
        dev: meta.dev(),
        ino: meta.ino(),
        mode: meta.mode() & 0o777,
        uid: meta.uid(),
        gid: meta.gid(),
    })
}

fn socket_fingerprint(socket_path: &Path) -> Result<(u64, u64), IpcError> {
    let meta = std::fs::symlink_metadata(socket_path).map_err(IpcError::Io)?;
    if !meta.file_type().is_socket() {
        return Err(IpcError::BindRace(format!(
            "socket path replaced with non-socket after bind: {}",
            socket_path.display()
        )));
    }
    Ok((meta.dev(), meta.ino()))
}

fn path_fingerprint_if_socket(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_socket() {
        return None;
    }
    Some((meta.dev(), meta.ino()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;
    use tokio::net::UnixListener;

    use super::{
        AttestationEvidence, ListenerSocketAttestation, attest_post_bind, capture_bind_attestation,
        map_attestation_verdict, merge_evidence,
    };
    use crate::error::IpcError;
    use crate::peer_cred::validate_socket_dir;

    #[test]
    fn merge_evidence_treats_conflicts_as_mismatch() {
        let verdict = merge_evidence(
            AttestationEvidence::Match,
            AttestationEvidence::Mismatch("inode mismatch".to_owned()),
        );
        assert!(matches!(verdict, ListenerSocketAttestation::Mismatch(_)));
    }

    #[test]
    fn merge_evidence_keeps_double_inconclusive_as_inconclusive() {
        let verdict = merge_evidence(
            AttestationEvidence::Inconclusive("no comparable path".to_owned()),
            AttestationEvidence::Inconclusive("no inode fingerprint".to_owned()),
        );
        assert!(matches!(
            verdict,
            ListenerSocketAttestation::Inconclusive(_)
        ));
    }

    #[test]
    fn merge_evidence_requires_both_path_and_inode_matches() {
        let verdict = merge_evidence(
            AttestationEvidence::Match,
            AttestationEvidence::Inconclusive("inode probe unavailable".to_owned()),
        );
        assert!(matches!(
            verdict,
            ListenerSocketAttestation::Inconclusive(_)
        ));
    }

    #[test]
    fn inconclusive_verdict_is_fail_closed_bind_race() {
        let err = map_attestation_verdict(
            Path::new("/tmp/ipc.sock"),
            ListenerSocketAttestation::Inconclusive("missing definitive inode evidence".to_owned()),
        )
        .expect_err("inconclusive evidence must fail closed");
        assert!(matches!(err, IpcError::BindRace(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_bind_attestation_accepts_matching_listener_and_path() {
        let dir = tempdir().expect("tempdir");
        let parent = dir.path().join("s");
        std::fs::create_dir(&parent).expect("mkdir");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let path = parent.join("ipc.sock");
        validate_socket_dir(&path).expect("validate");
        let attestation = capture_bind_attestation(&path).expect("capture");
        let listener = UnixListener::bind(&path).expect("bind");

        attest_post_bind(&path, &listener, &attestation).expect("matching listener/path succeeds");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_bind_attestation_rejects_listener_path_inode_mismatch() {
        let dir = tempdir().expect("tempdir");
        let parent = dir.path().join("s");
        std::fs::create_dir(&parent).expect("mkdir");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let path_a = parent.join("a.sock");
        let path_b = parent.join("b.sock");

        validate_socket_dir(&path_a).expect("validate");
        let attestation = capture_bind_attestation(&path_a).expect("capture");
        let listener_a = UnixListener::bind(&path_a).expect("bind a");
        let listener_b = UnixListener::bind(&path_b).expect("bind b");

        let err = attest_post_bind(&path_a, &listener_b, &attestation)
            .expect_err("mismatched listener/path should fail");
        assert!(matches!(err, IpcError::BindRace(_)));

        drop(listener_a);
        drop(listener_b);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_bind_attestation_rejects_parent_fingerprint_mutation() {
        let dir = tempdir().expect("tempdir");
        let parent = dir.path().join("s");
        std::fs::create_dir(&parent).expect("mkdir");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let path = parent.join("ipc.sock");
        validate_socket_dir(&path).expect("validate");
        let attestation = capture_bind_attestation(&path).expect("capture");
        let listener = UnixListener::bind(&path).expect("bind");

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("chmod mutate");

        let err =
            attest_post_bind(&path, &listener, &attestation).expect_err("fingerprint drift fails");
        assert!(matches!(err, IpcError::BindRace(_)));
    }
}
