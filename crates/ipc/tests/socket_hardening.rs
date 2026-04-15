//! Socket bind and credential hardening tests.
//!
//! Verifies filesystem permission enforcement, parent-directory
//! safety, and peer credential capture on accepted connections.

use forgeclaw_core::GroupId;
use forgeclaw_ipc::{
    GroupCapabilities, GroupInfo, IpcClient, IpcError, IpcServer, IpcServerOptions,
    PeerCredentialPolicy, PeerCredentialPolicyError, ProtocolError,
};
use tempfile::tempdir;

#[cfg(unix)]
#[tokio::test]
async fn bind_creates_socket_with_restricted_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    // Use a subdirectory with known-safe permissions.
    let sub = dir.path().join("safe");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("set mode");
    let path = sub.join("ipc.sock");
    let _server = IpcServer::bind(&path).expect("bind");
    let meta = std::fs::metadata(&path).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket should be owner read/write only");
}

#[cfg(unix)]
#[tokio::test]
async fn bind_smoke_succeeds_for_valid_socket_path() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("smoke");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    let server = IpcServer::bind(&path).expect("bind should succeed for valid path");
    assert_eq!(server.path(), path.as_path());
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_relative_socket_path() {
    let err = IpcServer::bind("ipc-relative.sock").expect_err("relative path must be rejected");
    assert!(
        err.to_string().contains("must be absolute"),
        "expected absolute-path enforcement error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_curdir_component_without_side_effects() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).expect("mkdir root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("chmod root");
    let target = root.join("child");
    let path = root.join(".").join("child").join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("curdir component must be rejected");
    assert!(
        err.to_string()
            .contains("must not contain traversal component"),
        "expected traversal component rejection, got: {err}"
    );
    assert!(
        !target.exists(),
        "rejecting curdir path must not create directories"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_parentdir_component_without_side_effects() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("root");
    std::fs::create_dir(&root).expect("mkdir root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("chmod root");
    let escaped = root.parent().expect("parent").join("escaped");
    let path = root
        .join("nested")
        .join("..")
        .join("escaped")
        .join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("parentdir component must be rejected");
    assert!(
        err.to_string()
            .contains("must not contain traversal component"),
        "expected traversal component rejection, got: {err}"
    );
    assert!(
        !escaped.exists(),
        "rejecting parentdir path must not create directories"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_group_writable_parent_directory() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o770))
        .expect("set group-writable mode");
    let path = sub.join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject unsafe dir");
    assert!(
        err.to_string().contains("must have mode 0700"),
        "expected strict parent-mode error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_parent_directory_with_mode_0755() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).expect("create sub");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).expect("set mode");
    let path = sub.join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject non-0700 parent");
    assert!(
        err.to_string().contains("must have mode 0700"),
        "expected strict parent-mode error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_creates_missing_parent_with_safe_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("created_by_ipc");
    let path = sub.join("ipc.sock");
    let _server = IpcServer::bind(&path).expect("bind");
    let meta = std::fs::metadata(&sub).expect("metadata");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "created dir should be 0700");
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_intermediate_symlink_ancestor() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real");
    let nested = real.join("nested");
    std::fs::create_dir(&real).expect("create real");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).expect("chmod real");
    std::fs::create_dir(&nested).expect("create nested");
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700))
        .expect("chmod nested");

    let link = dir.path().join("link");
    symlink(&real, &link).expect("create symlink");

    let path = link.join("nested").join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject lexical symlink ancestor");
    assert!(
        err.to_string().contains("symlink in socket ancestor chain"),
        "expected symlink ancestor error, got: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bind_rejects_missing_parent_through_symlink_without_side_effects() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let dir = tempdir().expect("tempdir");
    let real = dir.path().join("real");
    std::fs::create_dir(&real).expect("create real");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o700)).expect("chmod real");

    let link = dir.path().join("link");
    symlink(&real, &link).expect("create symlink");

    let path = link.join("missing").join("ipc.sock");
    let err = IpcServer::bind(&path).expect_err("should reject lexical symlink ancestor");
    assert!(
        err.to_string().contains("symlink in socket ancestor chain"),
        "expected symlink ancestor error, got: {err}"
    );
    assert!(
        !real.join("missing").exists(),
        "bind must fail-closed and not create dirs through symlinked paths"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_connection_has_peer_credentials() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("cred");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("set mode");
    let path = sub.join("ipc.sock");
    let server = IpcServer::bind(&path).expect("bind");
    let group = GroupInfo {
        id: GroupId::new("group-cred").expect("valid group id"),
        name: "Cred".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let accept_task = tokio::spawn(async move { server.accept(group).await.expect("accept") });
    let _pending_client = IpcClient::connect(&path).await.expect("connect");
    let conn = accept_task.await.expect("join");
    let id = conn.identity();
    let creds = id.peer_credentials().expect("credentials on unix");
    // The connecting process is us — verify the struct is populated.
    let _ = creds.uid;
    let _ = creds.gid;
}

#[cfg(unix)]
#[tokio::test]
async fn drop_skips_cleanup_when_path_inode_no_longer_matches_listener() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("drop-race");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");
    let parked_path = sub.join("ipc-parked.sock");

    let server = IpcServer::bind(&path).expect("bind");
    assert!(path.exists(), "socket should exist after bind");

    std::fs::rename(&path, &parked_path).expect("rename server socket");
    let replacement = std::os::unix::net::UnixListener::bind(&path).expect("bind replacement");

    drop(server);

    assert!(
        path.exists(),
        "drop must not unlink a socket inode not owned by the listener"
    );

    drop(replacement);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&parked_path);
}

#[cfg(unix)]
#[tokio::test]
async fn strict_peer_credential_policy_accepts_exact_uid_gid_match() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("strict-accept");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    let owner = std::fs::metadata(&sub).expect("metadata");
    let options = IpcServerOptions::hardened(Some(owner.uid()), Some(owner.gid()));
    let server = IpcServer::bind_with_options(&path, options).expect("bind");
    let group = GroupInfo {
        id: GroupId::new("group-cred-strict").expect("valid group id"),
        name: "CredStrict".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let accept_task = tokio::spawn(async move { server.accept(group).await.expect("accept") });
    let _pending_client = IpcClient::connect(&path).await.expect("connect");
    let pending = accept_task.await.expect("join");
    let identity = pending.identity();
    assert!(
        identity.peer_credentials().is_some(),
        "credentials must be captured"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn strict_peer_credential_policy_rejects_uid_mismatch() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("strict-reject");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path = sub.join("ipc.sock");

    let options = IpcServerOptions::hardened(Some(u32::MAX), None);
    let server = IpcServer::bind_with_options(&path, options).expect("bind");
    let group = GroupInfo {
        id: GroupId::new("group-cred-reject").expect("valid group id"),
        name: "CredReject".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let accept_task = tokio::spawn(async move { server.accept(group).await });
    let _pending_client = IpcClient::connect(&path).await.expect("connect");
    let result = accept_task.await.expect("join");
    assert!(
        matches!(
            result,
            Err(IpcError::Protocol(
                ProtocolError::PeerCredentialRejected { .. }
            ))
        ),
        "expected peer credential rejection, got {result:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn custom_peer_credential_policy_can_allow_or_deny() {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("custom-policy");
    std::fs::create_dir(&sub).expect("mkdir");
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let path_allow = sub.join("ipc-allow.sock");
    let path_deny = sub.join("ipc-deny.sock");

    let allow_options = IpcServerOptions {
        peer_credential_policy: PeerCredentialPolicy::Custom(Arc::new(|creds, _group| {
            if creds.is_some() {
                Ok(())
            } else {
                Err(PeerCredentialPolicyError::new("missing credentials"))
            }
        })),
        ..IpcServerOptions::default()
    };
    let allow_server =
        IpcServer::bind_with_options(&path_allow, allow_options).expect("bind allow");
    let allow_group = GroupInfo {
        id: GroupId::new("group-custom-allow").expect("valid group id"),
        name: "CustomAllow".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let allow_accept = tokio::spawn(async move { allow_server.accept(allow_group).await });
    let _allow_client = IpcClient::connect(&path_allow)
        .await
        .expect("allow connect");
    let allow_result = allow_accept.await.expect("join allow");
    assert!(allow_result.is_ok(), "custom allow policy should accept");

    let deny_options = IpcServerOptions {
        peer_credential_policy: PeerCredentialPolicy::Custom(Arc::new(|_creds, _group| {
            Err(PeerCredentialPolicyError::new("blocked by custom policy"))
        })),
        ..IpcServerOptions::default()
    };
    let deny_server = IpcServer::bind_with_options(&path_deny, deny_options).expect("bind deny");
    let deny_group = GroupInfo {
        id: GroupId::new("group-custom-deny").expect("valid group id"),
        name: "CustomDeny".parse().expect("valid name"),
        is_main: true,
        capabilities: GroupCapabilities::default(),
    };
    let deny_accept = tokio::spawn(async move { deny_server.accept(deny_group).await });
    let _deny_client = IpcClient::connect(&path_deny).await.expect("deny connect");
    let deny_result = deny_accept.await.expect("join deny");
    assert!(
        matches!(
            deny_result,
            Err(IpcError::Protocol(
                ProtocolError::PeerCredentialRejected { .. }
            ))
        ),
        "custom deny policy should reject, got {deny_result:?}"
    );
}
