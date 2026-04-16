//! Parent-directory ownership policy tests.
//!
//! `ParentOwnerPolicy` is checked twice: once at `validate_socket_dir`
//! (pre-bind) and once inside `attest_post_bind` (post-bind, after the
//! listener exists). A foreign-UID parent fails fast both times; the
//! default policy (`MatchEffectiveUid`) accepts a parent the running
//! process owns.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use forgeclaw_ipc::{IpcError, IpcServer, IpcServerOptions, ParentOwnerPolicy};
use proptest::prelude::*;
use tempfile::tempdir;

fn hardened_options_with_policy(policy: ParentOwnerPolicy) -> IpcServerOptions {
    let mut options =
        IpcServerOptions::hardened_current_process().expect("hardened current process");
    options.parent_owner_policy = policy;
    options
}

#[tokio::test]
async fn bind_accepts_parent_dir_owned_by_effective_uid() {
    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("owner-ok");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    // Default hardened options use `MatchEffectiveUid`, which matches
    // the uid we just used to create the directory.
    let _server = IpcServer::bind_with_options(
        &path,
        IpcServerOptions::hardened_current_process().expect("hardened"),
    )
    .expect("bind accepts own-uid parent");
}

#[tokio::test]
async fn bind_rejects_parent_dir_with_mismatched_uid() {
    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("owner-mismatch");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    // Pick a UID we provably don't own.
    let options = hardened_options_with_policy(ParentOwnerPolicy::RequireUid(u32::MAX - 1));
    let err = IpcServer::bind_with_options(&path, options)
        .expect_err("bind must reject foreign-uid parent");
    let IpcError::SocketParentOwnership {
        expected_uid,
        actual_uid,
        ..
    } = err
    else {
        unreachable!("expected SocketParentOwnership, got {err:?}")
    };
    assert_eq!(expected_uid, u32::MAX - 1);
    assert_ne!(actual_uid, expected_uid);
}

#[tokio::test]
async fn insecure_capture_only_disables_owner_policy() {
    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("owner-disabled");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    // `insecure_capture_only()` hard-wires `ParentOwnerPolicy::Disabled`
    // and emits a `tracing::warn` once per process; here we just
    // confirm the bind succeeds regardless of owner enforcement.
    let options = IpcServerOptions::insecure_capture_only();
    assert!(matches!(
        options.parent_owner_policy,
        ParentOwnerPolicy::Disabled
    ));
    let _server =
        IpcServer::bind_with_options(&path, options).expect("insecure mode accepts any owner");
}

proptest! {
    /// Pure branch coverage: `RequireUid(expected)` accepts iff the
    /// actual parent UID equals `expected`. We model `actual_uid` by
    /// constructing a temp dir (whose UID we own), so we can only
    /// assert the equality contract when expected == current_uid;
    /// otherwise the enforcement must reject. The branch table is
    /// complete across arbitrary `expected` values.
    #[test]
    fn require_uid_rejects_iff_not_matching(expected_uid in any::<u32>()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let dir = tempdir().expect("tempdir");
            let sub = dir.path().join("prop-owner");
            std::fs::create_dir(&sub).expect("mkdir");
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
            let path = sub.join("ipc.sock");

            let options = hardened_options_with_policy(ParentOwnerPolicy::RequireUid(expected_uid));
            let result = IpcServer::bind_with_options(&path, options);

            // Snapshot our own UID the same way the policy does:
            // open a socketpair and read peer_cred. This mirrors
            // `peer_cred::effective_uid()`.
            let (a, _b) = tokio::net::UnixStream::pair().expect("pair");
            let effective = a.peer_cred().expect("peer_cred").uid();

            if expected_uid == effective {
                prop_assert!(result.is_ok(), "matching uid must accept: {result:?}");
            } else {
                match result {
                    Err(IpcError::SocketParentOwnership { .. }) => {}
                    other => prop_assert!(
                        false,
                        "mismatched uid must surface SocketParentOwnership, got {other:?}"
                    ),
                }
            }
            Ok(())
        }).expect("runtime loop");
    }
}
