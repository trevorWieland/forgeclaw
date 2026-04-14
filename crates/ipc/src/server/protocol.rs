//! Shared server-side protocol enforcement helpers.

use std::sync::{Arc, Mutex};

use tokio::time::Instant;

use crate::codec::DEFAULT_MAX_UNKNOWN_SKIPS;
use crate::error::{FrameError, IpcError, ProtocolError};
use crate::message::{ContainerToHost, HostToContainer};
use crate::peer_cred::SessionIdentity;
use crate::policy::{DEFAULT_HEARTBEAT_TIMEOUT, DEFAULT_MAX_UNKNOWN_BYTES, UnknownFrameBudget};
use crate::util::truncate_for_log;

/// Runtime lifecycle phase after handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConnectionPhase {
    /// Container is processing a job.
    Processing,
    /// Container is connected but idle.
    Idle,
    /// Host has requested shutdown and is waiting for completion.
    DrainingAwaitingCompletion,
    /// Host has already observed completion while draining.
    DrainingCompleted,
}

impl ConnectionPhase {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Processing => "processing",
            Self::Idle => "idle",
            Self::DrainingAwaitingCompletion | Self::DrainingCompleted => "draining",
        }
    }
}

/// Per-connection runtime protocol state.
#[derive(Debug, Clone, Copy)]
pub(super) struct ConnectionState {
    phase: ConnectionPhase,
    heartbeat_deadline: Option<Instant>,
}

impl ConnectionState {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            phase: ConnectionPhase::Processing,
            heartbeat_deadline: Some(now + DEFAULT_HEARTBEAT_TIMEOUT),
        }
    }

    pub(super) fn phase_name(self) -> &'static str {
        self.phase.as_str()
    }
}

pub(super) fn recv_deadline(state: &ConnectionState) -> Option<Instant> {
    state.heartbeat_deadline
}

pub(super) fn heartbeat_timeout_error(state: &ConnectionState) -> IpcError {
    IpcError::Protocol(ProtocolError::HeartbeatTimeout {
        phase: state.phase_name(),
        timeout_secs: DEFAULT_HEARTBEAT_TIMEOUT.as_secs(),
    })
}

pub(super) fn enforce_outbound_state(
    state: &mut ConnectionState,
    msg: &HostToContainer,
    now: Instant,
) -> Result<(), IpcError> {
    match msg {
        HostToContainer::Init(_) => Err(lifecycle_violation(
            state.phase,
            "host_to_container",
            msg.type_name(),
            "init is only legal during handshake",
        )),
        HostToContainer::Messages(_) => {
            if matches!(
                state.phase,
                ConnectionPhase::DrainingAwaitingCompletion | ConnectionPhase::DrainingCompleted
            ) {
                return Err(lifecycle_violation(
                    state.phase,
                    "host_to_container",
                    msg.type_name(),
                    "messages are not allowed after shutdown",
                ));
            }
            state.phase = ConnectionPhase::Processing;
            state.heartbeat_deadline = Some(now + DEFAULT_HEARTBEAT_TIMEOUT);
            Ok(())
        }
        HostToContainer::Shutdown(_) => {
            if matches!(
                state.phase,
                ConnectionPhase::DrainingAwaitingCompletion | ConnectionPhase::DrainingCompleted
            ) {
                return Err(lifecycle_violation(
                    state.phase,
                    "host_to_container",
                    msg.type_name(),
                    "shutdown already requested",
                ));
            }
            state.phase = ConnectionPhase::DrainingAwaitingCompletion;
            state.heartbeat_deadline = None;
            Ok(())
        }
    }
}

pub(super) fn enforce_inbound_state(
    state: &mut ConnectionState,
    msg: &ContainerToHost,
    now: Instant,
) -> Result<(), IpcError> {
    match msg {
        ContainerToHost::Ready(_) => Err(lifecycle_violation(
            state.phase,
            "container_to_host",
            msg.type_name(),
            "ready is only legal during handshake",
        )),
        ContainerToHost::OutputDelta(_) | ContainerToHost::Progress(_) => {
            if !matches!(state.phase, ConnectionPhase::Processing) {
                return Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "message requires active processing",
                ));
            }
            Ok(())
        }
        ContainerToHost::Command(_) => {
            if matches!(
                state.phase,
                ConnectionPhase::DrainingAwaitingCompletion | ConnectionPhase::DrainingCompleted
            ) {
                return Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "commands are rejected after shutdown",
                ));
            }
            Ok(())
        }
        ContainerToHost::OutputComplete(_) => match state.phase {
            ConnectionPhase::Processing => {
                state.phase = ConnectionPhase::Idle;
                state.heartbeat_deadline = None;
                Ok(())
            }
            ConnectionPhase::DrainingAwaitingCompletion => {
                state.phase = ConnectionPhase::DrainingCompleted;
                Ok(())
            }
            ConnectionPhase::DrainingCompleted => Err(lifecycle_violation(
                state.phase,
                "container_to_host",
                msg.type_name(),
                "job already completed during draining",
            )),
            ConnectionPhase::Idle => Err(lifecycle_violation(
                state.phase,
                "container_to_host",
                msg.type_name(),
                "no in-flight job to complete",
            )),
        },
        ContainerToHost::Heartbeat(_) => {
            if matches!(state.phase, ConnectionPhase::Processing) {
                state.heartbeat_deadline = Some(now + DEFAULT_HEARTBEAT_TIMEOUT);
            }
            Ok(())
        }
        ContainerToHost::Error(_) => Ok(()),
    }
}

pub(super) fn log_unknown_message(
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

pub(super) fn log_fatal_protocol_error(
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

fn lifecycle_violation(
    phase: ConnectionPhase,
    direction: &'static str,
    message_type: &'static str,
    reason: &'static str,
) -> IpcError {
    IpcError::Protocol(ProtocolError::LifecycleViolation {
        phase: phase.as_str(),
        direction,
        message_type,
        reason,
    })
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
                group_name: truncate_for_log(&group.name),
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
        IpcError::Frame(FrameError::Oversize { .. }) => "frame_oversize",
        IpcError::Frame(FrameError::EmptyFrame) => "frame_empty",
        IpcError::Frame(FrameError::Truncated { .. }) => "frame_truncated",
        IpcError::Frame(FrameError::InvalidUtf8) => "frame_invalid_utf8",
        IpcError::Frame(FrameError::MalformedJson(_)) => "frame_malformed_json",
        IpcError::Serialize(_) => "serialize",
        IpcError::Protocol(ProtocolError::UnsupportedVersion { .. }) => "unsupported_version",
        IpcError::Protocol(ProtocolError::UnexpectedMessage { .. }) => "unexpected_message",
        IpcError::Protocol(ProtocolError::UnknownMessageType(_)) => "unknown_message_type",
        IpcError::Protocol(ProtocolError::TooManyUnknownMessages { .. }) => {
            "too_many_unknown_messages"
        }
        IpcError::Protocol(ProtocolError::TooManyUnknownBytes { .. }) => "too_many_unknown_bytes",
        IpcError::Protocol(ProtocolError::Unauthorized { .. }) => "unauthorized",
        IpcError::Protocol(ProtocolError::GroupMismatch { .. }) => "group_mismatch",
        IpcError::Protocol(ProtocolError::LifecycleViolation { .. }) => "lifecycle_violation",
        IpcError::Protocol(ProtocolError::HeartbeatTimeout { .. }) => "heartbeat_timeout",
        IpcError::Closed => "closed",
        IpcError::Timeout(_) => "timeout",
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
