use std::sync::{Arc, Mutex};

use crate::codec::DEFAULT_MAX_UNKNOWN_SKIPS;
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::peer_cred::SessionIdentity;
use crate::policy::{DEFAULT_MAX_UNKNOWN_BYTES, UnknownFrameBudget};
use crate::util::truncate_for_log;

use super::{UNAUTHORIZED_LOG_BURST, UNAUTHORIZED_LOG_EVERY, UnauthorizedLogDecision};

pub(crate) fn log_unknown_message(
    identity: &Arc<Mutex<SessionIdentity>>,
    message_type: &str,
    budget: &UnknownFrameBudget,
) {
    let group = identity_fields(identity);
    tracing::warn!(
        target: "forgeclaw_ipc::server",
        message_type = %truncate_for_log(message_type),
        skip_count = budget.count(),
        skip_count_limit = DEFAULT_MAX_UNKNOWN_SKIPS,
        skip_bytes = budget.bytes(),
        skip_bytes_limit = DEFAULT_MAX_UNKNOWN_BYTES,
        group_id = %group.group_id,
        group_name = %group.group_name,
        is_main = group.is_main,
        peer_uid = group.peer_uid,
        peer_gid = group.peer_gid,
        peer_pid = group.peer_pid,
        "ignoring unknown message type"
    );
}

pub(crate) fn log_unauthorized_command(
    identity: &Arc<Mutex<SessionIdentity>>,
    protocol_phase: &'static str,
    command: &'static str,
    reason: &'static str,
    decision: UnauthorizedLogDecision,
) {
    if !decision.should_log {
        return;
    }
    let group = identity_fields(identity);
    tracing::warn!(
        target: "forgeclaw_ipc::server",
        protocol_phase,
        command,
        reason,
        attempt = decision.attempt,
        suppressed_since_last = decision.suppressed_since_last,
        enforcement_action = decision.action.as_str(),
        strikes = decision.strikes,
        disconnect_after_strikes = decision.disconnect_after_strikes,
        backoff_ms = decision.backoff_ms,
        tokens_remaining = decision.tokens_remaining,
        sample_burst = UNAUTHORIZED_LOG_BURST,
        sample_every = UNAUTHORIZED_LOG_EVERY,
        group_id = %group.group_id,
        group_name = %group.group_name,
        is_main = group.is_main,
        peer_uid = group.peer_uid,
        peer_gid = group.peer_gid,
        peer_pid = group.peer_pid,
        "rejected unauthorized IPC command"
    );
}

pub(crate) fn log_fatal_protocol_error(
    identity: &Arc<Mutex<SessionIdentity>>,
    protocol_phase: &'static str,
    err: &IpcError,
) {
    if !err.is_fatal() {
        return;
    }
    let group = identity_fields(identity);
    tracing::error!(
        target: "forgeclaw_ipc::server",
        protocol_phase,
        error_class = error_class(err),
        error = %format_error_for_log(err),
        group_id = %group.group_id,
        group_name = %group.group_name,
        is_main = group.is_main,
        peer_uid = group.peer_uid,
        peer_gid = group.peer_gid,
        peer_pid = group.peer_pid,
        "fatal IPC protocol failure"
    );
}

fn format_error_for_log(err: &IpcError) -> String {
    match err {
        IpcError::Protocol(ProtocolError::UnsupportedVersion { peer, local }) => {
            format!(
                "unsupported protocol version: peer={}, local={local}",
                truncate_for_log(peer)
            )
        }
        IpcError::Protocol(ProtocolError::UnknownMessageType(ty)) => {
            format!("unknown message type: {}", truncate_for_log(ty))
        }
        IpcError::Protocol(ProtocolError::JobIdMismatch { expected, got, .. }) => {
            format!(
                "job_id mismatch: expected={}, got={}",
                truncate_for_log(expected.as_ref()),
                truncate_for_log(got.as_ref())
            )
        }
        IpcError::Protocol(ProtocolError::InvalidCommandPayload { command, reason }) => {
            format!(
                "invalid command payload: command={}, reason={}",
                truncate_for_log(command),
                truncate_for_log(reason)
            )
        }
        IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse {
            command,
            strikes,
            disconnect_after_strikes,
            ..
        }) => {
            format!(
                "unauthorized command abuse: command={}, strikes={}, threshold={}",
                truncate_for_log(command),
                strikes,
                disconnect_after_strikes
            )
        }
        IpcError::Frame(FrameError::MalformedJson(msg)) => {
            format!("malformed JSON: {}", truncate_for_log(msg))
        }
        IpcError::Serialize(msg) => format!("serialization error: {}", truncate_for_log(msg)),
        _ => err.to_string(),
    }
}

#[derive(Debug)]
struct IdentityFields {
    group_id: String,
    group_name: String,
    is_main: bool,
    peer_uid: Option<u32>,
    peer_gid: Option<u32>,
    peer_pid: Option<i32>,
}

fn identity_fields(identity: &Arc<Mutex<SessionIdentity>>) -> IdentityFields {
    match identity.lock() {
        Ok(guard) => {
            let group = guard.group();
            let creds = guard.peer_credentials();
            IdentityFields {
                group_id: truncate_for_log(group.id.as_ref()),
                group_name: truncate_for_log(group.name.as_ref()),
                is_main: group.is_main,
                peer_uid: creds.map(|c| c.uid),
                peer_gid: creds.map(|c| c.gid),
                peer_pid: creds.and_then(|c| c.pid),
            }
        }
        Err(_) => IdentityFields {
            group_id: "<identity-lock-poisoned>".to_owned(),
            group_name: "<identity-lock-poisoned>".to_owned(),
            is_main: false,
            peer_uid: None,
            peer_gid: None,
            peer_pid: None,
        },
    }
}

fn error_class(err: &IpcError) -> &'static str {
    match err {
        IpcError::Io(_) => "io",
        IpcError::BindRace(_) => "bind_race",
        IpcError::Frame(FrameError::Oversize { .. }) => "frame_oversize",
        IpcError::Frame(FrameError::EmptyFrame) => "frame_empty",
        IpcError::Frame(FrameError::Truncated { .. }) => "frame_truncated",
        IpcError::Frame(FrameError::InvalidUtf8) => "frame_invalid_utf8",
        IpcError::Frame(FrameError::MalformedJson(_)) => "frame_malformed_json",
        IpcError::Serialize(_) => "serialize",
        IpcError::Protocol(ProtocolError::UnsupportedVersion { .. }) => "unsupported_version",
        IpcError::Protocol(ProtocolError::UnexpectedMessage { .. }) => "unexpected_message",
        IpcError::Protocol(ProtocolError::NotCommand { .. }) => "not_command",
        IpcError::Protocol(ProtocolError::UnknownMessageType(_)) => "unknown_message_type",
        IpcError::Protocol(ProtocolError::TooManyUnknownMessages { .. }) => {
            "too_many_unknown_messages"
        }
        IpcError::Protocol(ProtocolError::TooManyUnknownBytes { .. }) => "too_many_unknown_bytes",
        IpcError::Protocol(ProtocolError::Unauthorized { .. }) => "unauthorized",
        IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse { .. }) => {
            "unauthorized_command_abuse"
        }
        IpcError::Protocol(ProtocolError::GroupMismatch { .. }) => "group_mismatch",
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. }) => "lifecycle_violation",
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. }) => "job_id_mismatch",
        IpcError::Protocol(ProtocolError::HeartbeatTimeout { .. }) => "heartbeat_timeout",
        IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded { .. }) => {
            "shutdown_deadline_exceeded"
        }
        IpcError::Protocol(ProtocolError::InvalidCommandPayload { .. }) => {
            "invalid_command_payload"
        }
        IpcError::Closed => "closed",
        IpcError::Timeout(_) => "timeout",
    }
}
