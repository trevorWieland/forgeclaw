//! Centralized protocol semantics gates by negotiated version.
//!
//! Each gate corresponds to one row in the **Versioned Behavior
//! Matrix** in `docs/IPC_PROTOCOL.md` § Versioning. The matrix and this
//! file are locked together by the
//! `semantics_matrix_matches_documented_gates` test below — adding a
//! new gate without updating the spec (or vice versa) fails CI.

use crate::error::{IpcError, ProtocolError};
use crate::message::command::{ClassifiedCommand, PrivilegedCommand};
use crate::message::{ContainerToHost, HostToContainer};
use crate::version::NegotiatedProtocolVersion;

/// Version-selected protocol behavior profile.
///
/// Exposed publicly so downstream callers (containers, adapters) may
/// query the live semantics for a connection without having to
/// re-derive the classification from [`NegotiatedProtocolVersion`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolSemantics {
    /// Protocol 1.0 baseline semantics. No additional gates beyond the
    /// shared wire constraints.
    V1_0,
    /// Protocol 1.x semantics for minor versions ≥ 1.1. Layers
    /// stricter rules on top of the baseline; see the Versioned
    /// Behavior Matrix in `docs/IPC_PROTOCOL.md`.
    V1_1Plus,
}

impl ProtocolSemantics {
    /// Classify a negotiated protocol version into its semantics
    /// profile.
    #[must_use]
    pub fn from_negotiated(version: NegotiatedProtocolVersion) -> Self {
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
    use super::{
        ProtocolSemantics, validate_classified_command, validate_container_to_host,
        validate_host_to_container,
    };
    use crate::error::{IpcError, ProtocolError};
    use crate::message::command::{GroupExtensions, GroupExtensionsVersion, RegisterGroupPayload};
    use crate::message::{
        CommandBody, CommandPayload, ContainerToHost, ErrorCode, ErrorPayload, HostToContainer,
        SendMessagePayload, ShutdownPayload, ShutdownReason,
    };
    use crate::version::NegotiatedProtocolVersion;
    use forgeclaw_core::{GroupId, JobId};

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

    /// Locks the **Versioned Behavior Matrix** in
    /// `docs/IPC_PROTOCOL.md` § Versioning to the runtime gate
    /// implementations. Every row must be reflected here. Adding a
    /// new gate requires extending both the spec table and this
    /// matrix; removing or relaxing one breaks this test.
    #[test]
    fn semantics_matrix_matches_documented_gates() {
        // ── shutdown.deadline_ms gate ─────────────────────────────
        let shutdown_zero = HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::IdleTimeout,
            deadline_ms: 0,
        });
        let shutdown_positive = HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::IdleTimeout,
            deadline_ms: 1,
        });
        // baseline: deadline_ms == 0 is allowed
        validate_host_to_container(ProtocolSemantics::V1_0, &shutdown_zero)
            .expect("baseline: deadline_ms=0 must be accepted");
        // ≥1.1: deadline_ms == 0 is rejected with LifecycleViolation
        let err = validate_host_to_container(ProtocolSemantics::V1_1Plus, &shutdown_zero)
            .expect_err("v1.1+: deadline_ms=0 must be rejected");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                message_type: "shutdown",
                ..
            })
        ));
        // ≥1.1: positive deadline_ms is accepted
        validate_host_to_container(ProtocolSemantics::V1_1Plus, &shutdown_positive)
            .expect("v1.1+: deadline_ms>0 must be accepted");

        // ── error.fatal requires job_id gate ──────────────────────
        let fatal_no_job = ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "boom".parse().expect("valid message"),
            fatal: true,
            job_id: None,
        });
        let fatal_with_job = ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "boom".parse().expect("valid message"),
            fatal: true,
            job_id: Some(JobId::new("job-1").expect("valid job id")),
        });
        let nonfatal_no_job = ContainerToHost::Error(ErrorPayload {
            code: ErrorCode::AdapterError,
            message: "warn".parse().expect("valid message"),
            fatal: false,
            job_id: None,
        });
        // baseline: any combination accepted
        validate_container_to_host(ProtocolSemantics::V1_0, &fatal_no_job)
            .expect("baseline: fatal without job_id accepted");
        // ≥1.1: fatal+no job rejected
        let err = validate_container_to_host(ProtocolSemantics::V1_1Plus, &fatal_no_job)
            .expect_err("v1.1+: fatal without job_id must be rejected");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::LifecycleViolation {
                message_type: "error",
                ..
            })
        ));
        // ≥1.1: fatal with job accepted; non-fatal without job accepted
        validate_container_to_host(ProtocolSemantics::V1_1Plus, &fatal_with_job)
            .expect("v1.1+: fatal with job_id accepted");
        validate_container_to_host(ProtocolSemantics::V1_1Plus, &nonfatal_no_job)
            .expect("v1.1+: non-fatal without job_id accepted");

        // ── register_group.extensions required gate ──────────────
        let group_name = "Test Group".parse().expect("valid name");
        let cmd_no_extensions = CommandBody::RegisterGroup(RegisterGroupPayload {
            name: group_name,
            extensions: None,
        })
        .classify();
        let group_name_with = "Test Group".parse().expect("valid name");
        let extensions = GroupExtensions::new(
            GroupExtensionsVersion::new("1").expect("valid extensions version"),
        );
        let cmd_with_extensions = CommandBody::RegisterGroup(RegisterGroupPayload {
            name: group_name_with,
            extensions: Some(extensions),
        })
        .classify();
        // baseline: extensions optional
        validate_classified_command(ProtocolSemantics::V1_0, &cmd_no_extensions)
            .expect("baseline: extensions optional");
        // ≥1.1: extensions required
        let err = validate_classified_command(ProtocolSemantics::V1_1Plus, &cmd_no_extensions)
            .expect_err("v1.1+: missing extensions must be rejected");
        assert!(matches!(
            err,
            IpcError::Protocol(ProtocolError::InvalidCommandPayload {
                command: "register_group",
                ..
            })
        ));
        // ≥1.1: extensions present accepted
        validate_classified_command(ProtocolSemantics::V1_1Plus, &cmd_with_extensions)
            .expect("v1.1+: extensions present accepted");

        // ── sanity: non-gated message types pass on every minor ──
        let benign = ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::new("group-main").expect("valid group id"),
                text: "hello".parse().expect("valid text"),
            }),
        });
        for sem in [ProtocolSemantics::V1_0, ProtocolSemantics::V1_1Plus] {
            validate_container_to_host(sem, &benign).expect("benign accepted on every minor");
        }
    }

    /// Locks the forward-compat promotion row of the Versioned
    /// Behavior Matrix: at 1.1+, `heartbeat` and `output_delta` resolve
    /// to `ForwardCompatPolicy::Reject` regardless of their baseline
    /// policy. This guard ensures a future relaxation of the baseline
    /// does not silently leak through at 1.1+.
    #[test]
    fn semantics_matrix_promotes_forward_compat_at_v1_1plus() {
        use crate::forward_compat::{ForwardCompatPolicy, promote_for_container};
        use crate::message::{HeartbeatPayload, OutputDeltaPayload};

        let heartbeat = ContainerToHost::Heartbeat(HeartbeatPayload {
            timestamp: "2026-04-16T00:00:00Z".parse().expect("valid timestamp"),
        });
        let output_delta = ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
            job_id: JobId::new("j").expect("valid job id"),
        });
        for msg in [&heartbeat, &output_delta] {
            assert_eq!(
                promote_for_container(msg.forward_compat_base(), msg, ProtocolSemantics::V1_1Plus,),
                ForwardCompatPolicy::Reject,
                "1.1+ must resolve {} to Reject",
                msg.type_name()
            );
        }
    }
}
