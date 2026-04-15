use std::sync::Arc;

use crate::codec::DEFAULT_MAX_UNKNOWN_SKIPS;
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::peer_cred::SessionIdentity;
use crate::policy::{DEFAULT_MAX_UNKNOWN_BYTES, UnknownTrafficBudget};
use crate::util::truncate_for_log;

use super::{UNAUTHORIZED_LOG_BURST, UNAUTHORIZED_LOG_EVERY, UnauthorizedLogDecision};

pub(crate) fn log_unknown_message(
    identity: &Arc<SessionIdentity>,
    message_type: &str,
    budget: &UnknownTrafficBudget,
) {
    let group = identity_fields(identity);
    let limits = budget.limits();
    tracing::warn!(
        target: "forgeclaw_ipc::server",
        message_type = %truncate_for_log(message_type),
        skip_count = budget.count(),
        skip_count_limit = DEFAULT_MAX_UNKNOWN_SKIPS,
        skip_bytes = budget.bytes(),
        skip_bytes_limit = DEFAULT_MAX_UNKNOWN_BYTES,
        lifetime_skip_count = budget.total_count(),
        lifetime_skip_count_limit = limits.lifetime_message_limit,
        lifetime_skip_bytes = budget.total_bytes(),
        lifetime_skip_bytes_limit = limits.lifetime_byte_limit,
        rate_limit_enabled = limits.rate_limiter_enabled(),
        rate_limit_burst_capacity = limits.rate_limit_burst_capacity,
        rate_limit_refill_per_second = limits.rate_limit_refill_per_second,
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
    identity: &Arc<SessionIdentity>,
    protocol_phase: &'static str,
    command: &'static str,
    reason: &'static str,
    decision: UnauthorizedLogDecision,
) {
    let group = identity_fields(identity);
    tracing::debug!(
        target: "forgeclaw_ipc::server",
        protocol_phase,
        command,
        reason,
        attempt = decision.attempt,
        enforcement_action = decision.action.as_str(),
        strikes = decision.strikes,
        disconnect_after_strikes = decision.disconnect_after_strikes,
        backoff_ms = decision.backoff_ms,
        tokens_remaining = decision.tokens_remaining,
        sampled_warn = decision.should_log,
        suppressed_since_last = decision.suppressed_since_last,
        sample_burst = UNAUTHORIZED_LOG_BURST,
        sample_every = UNAUTHORIZED_LOG_EVERY,
        group_id = %group.group_id,
        group_name = %group.group_name,
        is_main = group.is_main,
        peer_uid = group.peer_uid,
        peer_gid = group.peer_gid,
        peer_pid = group.peer_pid,
        "audit unauthorized IPC command rejection"
    );
    if !decision.should_log {
        return;
    }
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
    identity: &Arc<SessionIdentity>,
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

pub(crate) fn log_outbound_validation_rejection(
    identity: &Arc<SessionIdentity>,
    protocol_phase: &'static str,
    err: &IpcError,
) {
    let IpcError::Protocol(ProtocolError::OutboundValidation {
        direction,
        message_type,
        field_path,
        reason,
    }) = err
    else {
        return;
    };

    let group = identity_fields(identity);
    tracing::warn!(
        target: "forgeclaw_ipc::server",
        protocol_phase,
        direction,
        message_type,
        field_path = %truncate_for_log(field_path),
        reason = %truncate_for_log(reason),
        group_id = %group.group_id,
        group_name = %group.group_name,
        is_main = group.is_main,
        peer_uid = group.peer_uid,
        peer_gid = group.peer_gid,
        peer_pid = group.peer_pid,
        "rejected outbound IPC message before serialization"
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
        IpcError::Protocol(ProtocolError::OutboundValidation {
            direction,
            message_type,
            field_path,
            reason,
        }) => {
            format!(
                "outbound validation failed: direction={}, type={}, field={}, reason={}",
                truncate_for_log(direction),
                truncate_for_log(message_type),
                truncate_for_log(field_path),
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
        IpcError::Protocol(ProtocolError::PeerCredentialRejected {
            reason,
            expected_uid,
            expected_gid,
            actual_uid,
            actual_gid,
        }) => {
            format!(
                "peer credential rejected: reason={}, expected_uid={expected_uid:?}, expected_gid={expected_gid:?}, actual_uid={actual_uid:?}, actual_gid={actual_gid:?}",
                truncate_for_log(reason)
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

fn identity_fields(identity: &Arc<SessionIdentity>) -> IdentityFields {
    let group = identity.group();
    let creds = identity.peer_credentials();
    IdentityFields {
        group_id: truncate_for_log(group.id.as_ref()),
        group_name: truncate_for_log(group.name.as_ref()),
        is_main: group.is_main,
        peer_uid: creds.map(|c| c.uid),
        peer_gid: creds.map(|c| c.gid),
        peer_pid: creds.and_then(|c| c.pid),
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
        IpcError::Protocol(ProtocolError::TooManyUnknownMessagesTotal { .. }) => {
            "too_many_unknown_messages_total"
        }
        IpcError::Protocol(ProtocolError::TooManyUnknownBytesTotal { .. }) => {
            "too_many_unknown_bytes_total"
        }
        IpcError::Protocol(ProtocolError::UnknownMessageRateLimitExceeded { .. }) => {
            "unknown_message_rate_limited"
        }
        IpcError::Protocol(ProtocolError::Unauthorized { .. }) => "unauthorized",
        IpcError::Protocol(ProtocolError::UnauthorizedCommandAbuse { .. }) => {
            "unauthorized_command_abuse"
        }
        IpcError::Protocol(ProtocolError::GroupMismatch { .. }) => "group_mismatch",
        IpcError::Protocol(ProtocolError::PeerCredentialRejected { .. }) => {
            "peer_credential_rejected"
        }
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. }) => "lifecycle_violation",
        IpcError::Protocol(ProtocolError::JobIdMismatch { .. }) => "job_id_mismatch",
        IpcError::Protocol(ProtocolError::HeartbeatTimeout { .. }) => "heartbeat_timeout",
        IpcError::Protocol(ProtocolError::ShutdownDeadlineExceeded { .. }) => {
            "shutdown_deadline_exceeded"
        }
        IpcError::Protocol(ProtocolError::InvalidCommandPayload { .. }) => {
            "invalid_command_payload"
        }
        IpcError::Protocol(ProtocolError::OutboundValidation { .. }) => "outbound_validation",
        IpcError::Closed => "closed",
        IpcError::Timeout(_) => "timeout",
    }
}
