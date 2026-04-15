//! Shared lifecycle transition rules for IPC client/server state.

use forgeclaw_core::JobId;

use crate::error::{IpcError, ProtocolError};
use crate::message::{CommandBody, CommandPayload, ContainerToHost, HostToContainer};

/// Runtime lifecycle phase after handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionPhase {
    /// Container is processing a job.
    Processing,
    /// Container is connected but idle.
    Idle,
    /// Host requested shutdown and is waiting for completion.
    DrainingAwaitingCompletion,
    /// Terminal phase: connection must close and reject further traffic.
    Closed,
}

impl ConnectionPhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Processing => "processing",
            Self::Idle => "idle",
            Self::DrainingAwaitingCompletion => "draining",
            Self::Closed => "closed",
        }
    }
}

/// Lifecycle transition side effect for the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LifecycleAction {
    None,
    CloseAfterCurrentFrame,
}

impl LifecycleAction {
    #[must_use]
    pub(crate) fn should_close_after_frame(self) -> bool {
        matches!(self, Self::CloseAfterCurrentFrame)
    }
}

/// Shared per-connection lifecycle state.
#[derive(Debug, Clone)]
pub(crate) struct LifecycleState {
    phase: ConnectionPhase,
    active_job_id: JobId,
}

impl LifecycleState {
    #[must_use]
    pub(crate) fn new(active_job_id: JobId) -> Self {
        Self {
            phase: ConnectionPhase::Processing,
            active_job_id,
        }
    }

    #[must_use]
    pub(crate) fn phase_name(&self) -> &'static str {
        self.phase.as_str()
    }

    #[must_use]
    pub(crate) fn phase(&self) -> ConnectionPhase {
        self.phase
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn active_job_id(&self) -> &JobId {
        &self.active_job_id
    }
}

pub(crate) fn enforce_host_to_container(
    state: &mut LifecycleState,
    msg: &HostToContainer,
) -> Result<LifecycleAction, IpcError> {
    match msg {
        HostToContainer::Init(_) => Err(lifecycle_violation(
            state.phase,
            "host_to_container",
            msg.type_name(),
            "init is only legal during handshake",
        )),
        HostToContainer::Messages(payload) => {
            if matches!(
                state.phase,
                ConnectionPhase::DrainingAwaitingCompletion | ConnectionPhase::Closed
            ) {
                return Err(lifecycle_violation(
                    state.phase,
                    "host_to_container",
                    msg.type_name(),
                    "messages are not allowed after shutdown",
                ));
            }
            if matches!(state.phase, ConnectionPhase::Processing)
                && payload.job_id != state.active_job_id
            {
                return Err(job_id_mismatch(
                    "host_to_container",
                    "messages",
                    &state.active_job_id,
                    &payload.job_id,
                ));
            }
            if matches!(state.phase, ConnectionPhase::Idle) {
                state.active_job_id = payload.job_id.clone();
            }
            state.phase = ConnectionPhase::Processing;
            Ok(LifecycleAction::None)
        }
        HostToContainer::Shutdown(_) => {
            if matches!(
                state.phase,
                ConnectionPhase::DrainingAwaitingCompletion | ConnectionPhase::Closed
            ) {
                return Err(lifecycle_violation(
                    state.phase,
                    "host_to_container",
                    msg.type_name(),
                    "shutdown already requested",
                ));
            }
            state.phase = ConnectionPhase::DrainingAwaitingCompletion;
            Ok(LifecycleAction::None)
        }
    }
}

pub(crate) fn enforce_container_to_host(
    state: &mut LifecycleState,
    msg: &ContainerToHost,
) -> Result<LifecycleAction, IpcError> {
    if matches!(state.phase, ConnectionPhase::Closed) {
        return Err(lifecycle_violation(
            state.phase,
            "container_to_host",
            msg.type_name(),
            "connection is terminally closed",
        ));
    }
    match msg {
        ContainerToHost::Ready(_) => Err(lifecycle_violation(
            state.phase,
            "container_to_host",
            msg.type_name(),
            "ready is only legal during handshake",
        )),
        ContainerToHost::OutputDelta(payload) => {
            if !matches!(state.phase, ConnectionPhase::Processing) {
                return Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "message requires active processing",
                ));
            }
            require_active_job("container_to_host", msg.type_name(), state, &payload.job_id)?;
            Ok(LifecycleAction::None)
        }
        ContainerToHost::Progress(payload) => {
            if !matches!(state.phase, ConnectionPhase::Processing) {
                return Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "message requires active processing",
                ));
            }
            require_active_job("container_to_host", msg.type_name(), state, &payload.job_id)?;
            Ok(LifecycleAction::None)
        }
        ContainerToHost::Command(payload) => {
            if !matches!(state.phase, ConnectionPhase::Processing) {
                return Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "commands require active processing",
                ));
            }
            validate_command_payload(payload)?;
            Ok(LifecycleAction::None)
        }
        ContainerToHost::OutputComplete(payload) => {
            require_active_job("container_to_host", msg.type_name(), state, &payload.job_id)?;
            match state.phase {
                ConnectionPhase::Processing => {
                    state.phase = ConnectionPhase::Idle;
                    Ok(LifecycleAction::None)
                }
                ConnectionPhase::DrainingAwaitingCompletion => {
                    state.phase = ConnectionPhase::Closed;
                    Ok(LifecycleAction::CloseAfterCurrentFrame)
                }
                ConnectionPhase::Idle => Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "no in-flight job to complete",
                )),
                ConnectionPhase::Closed => Err(lifecycle_violation(
                    state.phase,
                    "container_to_host",
                    msg.type_name(),
                    "connection is terminally closed",
                )),
            }
        }
        ContainerToHost::Heartbeat(_) => Ok(LifecycleAction::None),
        ContainerToHost::Error(payload) => {
            if let Some(job_id) = &payload.job_id {
                require_active_job("container_to_host", msg.type_name(), state, job_id)?;
            }
            Ok(LifecycleAction::None)
        }
    }
}

fn require_active_job(
    direction: &'static str,
    message_type: &'static str,
    state: &LifecycleState,
    got: &JobId,
) -> Result<(), IpcError> {
    if got == &state.active_job_id {
        return Ok(());
    }
    Err(job_id_mismatch(
        direction,
        message_type,
        &state.active_job_id,
        got,
    ))
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

fn invalid_command_payload(command: &'static str, reason: impl Into<String>) -> IpcError {
    IpcError::Protocol(ProtocolError::InvalidCommandPayload {
        command,
        reason: reason.into(),
    })
}

fn job_id_mismatch(
    direction: &'static str,
    message_type: &'static str,
    expected: &JobId,
    got: &JobId,
) -> IpcError {
    IpcError::Protocol(ProtocolError::JobIdMismatch {
        direction,
        message_type,
        expected: expected.clone(),
        got: got.clone(),
    })
}

fn validate_command_payload(payload: &CommandPayload) -> Result<(), IpcError> {
    if let CommandBody::RegisterGroup(register) = &payload.body {
        if let Some(extensions) = &register.extensions {
            if let Err(err) = extensions.validate_wire_invariants() {
                return Err(invalid_command_payload("register_group", err.to_string()));
            }
        }
    }
    Ok(())
}
