//! Centralized protocol semantics gates by negotiated version.

use crate::error::{IpcError, ProtocolError};
use crate::message::command::{ClassifiedCommand, PrivilegedCommand};
use crate::message::{ContainerToHost, HostToContainer};
use crate::version::NegotiatedProtocolVersion;

/// Version-selected protocol behavior profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolSemantics {
    /// Protocol 1.0 baseline semantics.
    V1_0,
    /// Protocol 1.x semantics for minor versions after 1.0.
    V1_1Plus,
}

impl ProtocolSemantics {
    #[must_use]
    pub(crate) fn from_negotiated(version: NegotiatedProtocolVersion) -> Self {
        match (version.major(), version.minor()) {
            (1, 0) => Self::V1_0,
            (1, _) => Self::V1_1Plus,
            _ => Self::V1_0,
        }
    }
}

pub(crate) fn validate_host_to_container(
    semantics: ProtocolSemantics,
    msg: &HostToContainer,
) -> Result<(), IpcError> {
    match semantics {
        ProtocolSemantics::V1_0 => Ok(()),
        ProtocolSemantics::V1_1Plus => {
            if let HostToContainer::Shutdown(payload) = msg {
                if payload.deadline_ms == 0 {
                    return Err(IpcError::Protocol(ProtocolError::LifecycleViolation {
                        phase: "processing",
                        direction: "host_to_container",
                        message_type: "shutdown",
                        reason: "protocol >=1.1 requires shutdown.deadline_ms > 0",
                    }));
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_container_to_host(
    semantics: ProtocolSemantics,
    msg: &ContainerToHost,
) -> Result<(), IpcError> {
    match semantics {
        ProtocolSemantics::V1_0 => Ok(()),
        ProtocolSemantics::V1_1Plus => {
            if let ContainerToHost::Error(payload) = msg {
                if payload.fatal && payload.job_id.is_none() {
                    return Err(IpcError::Protocol(ProtocolError::LifecycleViolation {
                        phase: "processing",
                        direction: "container_to_host",
                        message_type: "error",
                        reason: "protocol >=1.1 requires fatal errors to include job_id",
                    }));
                }
            }
            Ok(())
        }
    }
}

pub(crate) fn validate_classified_command(
    semantics: ProtocolSemantics,
    cmd: &ClassifiedCommand,
) -> Result<(), IpcError> {
    match semantics {
        ProtocolSemantics::V1_0 => Ok(()),
        ProtocolSemantics::V1_1Plus => {
            if let ClassifiedCommand::Privileged(PrivilegedCommand::RegisterGroup(payload)) = cmd {
                if payload.extensions.is_none() {
                    return Err(IpcError::Protocol(ProtocolError::InvalidCommandPayload {
                        command: "register_group",
                        reason: "protocol >=1.1 requires payload.extensions".to_owned(),
                    }));
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProtocolSemantics, validate_classified_command, validate_container_to_host};
    use crate::error::{IpcError, ProtocolError};
    use crate::message::{
        CommandBody, CommandPayload, ContainerToHost, ErrorCode, ErrorPayload, SendMessagePayload,
    };
    use crate::version::NegotiatedProtocolVersion;
    use forgeclaw_core::GroupId;

    #[test]
    fn semantics_resolves_v1_0() {
        let semantics =
            ProtocolSemantics::from_negotiated(NegotiatedProtocolVersion::new_for_tests(1, 0));
        assert_eq!(semantics, ProtocolSemantics::V1_0);
    }

    #[test]
    fn semantics_resolves_future_minor() {
        let semantics =
            ProtocolSemantics::from_negotiated(NegotiatedProtocolVersion::new_for_tests(1, 7));
        assert_eq!(semantics, ProtocolSemantics::V1_1Plus);
    }

    #[test]
    fn command_gate_hook_runs_for_all_minor_profiles() {
        let cmd = CommandBody::SendMessage(SendMessagePayload {
            target_group: GroupId::new("group-main").expect("valid group id"),
            text: "hello".parse().expect("valid text"),
        })
        .classify();
        validate_classified_command(ProtocolSemantics::V1_0, &cmd).expect("v1.0 command gate");
        validate_classified_command(ProtocolSemantics::V1_1Plus, &cmd)
            .expect("future-minor command gate");
    }

    #[test]
    fn inbound_gate_hook_runs_for_all_minor_profiles() {
        let msg = ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::new("group-main").expect("valid group id"),
                text: "hello".parse().expect("valid text"),
            }),
        });
        validate_container_to_host(ProtocolSemantics::V1_0, &msg).expect("v1.0 inbound gate");
        validate_container_to_host(ProtocolSemantics::V1_1Plus, &msg)
            .expect("future-minor inbound gate");
    }

    #[test]
    fn future_minor_inbound_gate_rejects_fatal_error_without_job_id() {
        let msg = ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "fatal".parse().expect("valid message"),
            fatal: true,
            job_id: None,
        });
        let err = validate_container_to_host(ProtocolSemantics::V1_1Plus, &msg)
            .expect_err("future-minor gate should reject");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation { .. })
        ));
    }
}
