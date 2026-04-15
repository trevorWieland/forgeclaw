//! Explicit outbound runtime validation for wire constraints.

use crate::error::{IpcError, ProtocolError};
use crate::message::limits;
use crate::message::{CommandBody, ContainerToHost, HostToContainer};

const CORE_ID_MAX_LEN: usize = 128;

pub(crate) fn validate_outbound_host_to_container(msg: &HostToContainer) -> Result<(), IpcError> {
    match msg {
        HostToContainer::Init(payload) => {
            validate_core_id_field(
                "host_to_container",
                "init",
                "job_id",
                payload.job_id.as_ref(),
            )?;
            validate_collection_len(
                "host_to_container",
                "init",
                "context.messages",
                payload.context.messages.len(),
                limits::MAX_HISTORICAL_MESSAGES,
            )?;
            validate_core_id_field(
                "host_to_container",
                "init",
                "context.group.id",
                payload.context.group.id.as_ref(),
            )
        }
        HostToContainer::Messages(payload) => {
            validate_core_id_field(
                "host_to_container",
                "messages",
                "job_id",
                payload.job_id.as_ref(),
            )?;
            validate_collection_len(
                "host_to_container",
                "messages",
                "messages",
                payload.messages.len(),
                limits::MAX_HISTORICAL_MESSAGES,
            )
        }
        HostToContainer::Shutdown(_) => Ok(()),
    }
}

pub(crate) fn validate_outbound_container_to_host(msg: &ContainerToHost) -> Result<(), IpcError> {
    match msg {
        ContainerToHost::Ready(_) | ContainerToHost::Heartbeat(_) => Ok(()),
        ContainerToHost::OutputDelta(payload) => validate_core_id_field(
            "container_to_host",
            "output_delta",
            "job_id",
            payload.job_id.as_ref(),
        ),
        ContainerToHost::OutputComplete(payload) => {
            validate_core_id_field(
                "container_to_host",
                "output_complete",
                "job_id",
                payload.job_id.as_ref(),
            )?;
            if let Some(session_id) = &payload.session_id {
                validate_max_len_field(
                    "container_to_host",
                    "output_complete",
                    "session_id",
                    session_id.as_ref(),
                    limits::MAX_SESSION_ID_TEXT_CHARS,
                )?;
            }
            if let Some(result) = &payload.result {
                validate_max_len_field(
                    "container_to_host",
                    "output_complete",
                    "result",
                    result.as_ref(),
                    limits::MAX_OUTPUT_RESULT_TEXT_CHARS,
                )?;
            }
            Ok(())
        }
        ContainerToHost::Progress(payload) => validate_core_id_field(
            "container_to_host",
            "progress",
            "job_id",
            payload.job_id.as_ref(),
        ),
        ContainerToHost::Error(payload) => {
            if let Some(job_id) = &payload.job_id {
                validate_core_id_field("container_to_host", "error", "job_id", job_id.as_ref())?;
            }
            Ok(())
        }
        ContainerToHost::Command(payload) => validate_outbound_command_payload(&payload.body),
    }
}

fn validate_outbound_command_payload(body: &CommandBody) -> Result<(), IpcError> {
    match body {
        CommandBody::SendMessage(payload) => validate_core_id_field(
            "container_to_host",
            "command",
            "payload.target_group",
            payload.target_group.as_ref(),
        ),
        CommandBody::ScheduleTask(payload) => validate_core_id_field(
            "container_to_host",
            "command",
            "payload.group",
            payload.group.as_ref(),
        ),
        CommandBody::PauseTask(payload) => validate_core_id_field(
            "container_to_host",
            "command",
            "payload.task_id",
            payload.task_id.as_ref(),
        ),
        CommandBody::CancelTask(payload) => validate_core_id_field(
            "container_to_host",
            "command",
            "payload.task_id",
            payload.task_id.as_ref(),
        ),
        CommandBody::RegisterGroup(payload) => {
            if let Some(extensions) = &payload.extensions {
                if let Err(err) = extensions.validate_wire_invariants() {
                    return Err(outbound_validation_error(
                        "container_to_host",
                        "command",
                        "payload.extensions",
                        err.to_string(),
                    ));
                }
            }
            Ok(())
        }
        CommandBody::DispatchTanren(_) => Ok(()),
        CommandBody::DispatchSelfImprovement(payload) => {
            validate_collection_len(
                "container_to_host",
                "command",
                "payload.scopes",
                payload.scopes.len(),
                limits::MAX_SELF_IMPROVEMENT_LIST_ITEMS,
            )?;
            validate_collection_len(
                "container_to_host",
                "command",
                "payload.acceptance_tests",
                payload.acceptance_tests.len(),
                limits::MAX_SELF_IMPROVEMENT_LIST_ITEMS,
            )
        }
    }
}

fn validate_collection_len(
    direction: &'static str,
    message_type: &'static str,
    field_path: &'static str,
    actual: usize,
    max: usize,
) -> Result<(), IpcError> {
    if actual <= max {
        return Ok(());
    }
    Err(outbound_validation_error(
        direction,
        message_type,
        field_path,
        format!("count {actual} exceeds maximum {max}"),
    ))
}

fn validate_max_len_field(
    direction: &'static str,
    message_type: &'static str,
    field_path: &'static str,
    value: &str,
    max: usize,
) -> Result<(), IpcError> {
    let actual = value.chars().count();
    if actual <= max {
        return Ok(());
    }
    Err(outbound_validation_error(
        direction,
        message_type,
        field_path,
        format!("length {actual} exceeds maximum {max}"),
    ))
}

fn validate_core_id_field(
    direction: &'static str,
    message_type: &'static str,
    field_path: &'static str,
    value: &str,
) -> Result<(), IpcError> {
    if value.trim().is_empty() {
        return Err(outbound_validation_error(
            direction,
            message_type,
            field_path,
            "must contain at least one non-whitespace character",
        ));
    }
    validate_max_len_field(direction, message_type, field_path, value, CORE_ID_MAX_LEN)
}

fn outbound_validation_error(
    direction: &'static str,
    message_type: &'static str,
    field_path: &'static str,
    reason: impl Into<String>,
) -> IpcError {
    IpcError::Protocol(ProtocolError::OutboundValidation {
        direction,
        message_type,
        field_path: field_path.to_owned(),
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use forgeclaw_core::{GroupId, JobId};

    use crate::message::{
        ContainerToHost, GroupCapabilities, GroupInfo, HistoricalMessages, HostToContainer,
        InitConfig, InitContext, InitPayload, MessagesPayload, OutputDeltaPayload,
    };

    use super::{
        validate_core_id_field, validate_outbound_container_to_host,
        validate_outbound_host_to_container,
    };

    #[test]
    fn validate_core_id_field_rejects_whitespace() {
        let err = validate_core_id_field("container_to_host", "command", "payload.group", "  ")
            .expect_err("whitespace id should fail");
        assert!(matches!(
            err,
            crate::IpcError::Protocol(crate::ProtocolError::OutboundValidation { .. })
        ));
    }

    #[test]
    fn validate_core_id_field_rejects_over_128_chars() {
        let value = "x".repeat(129);
        let err = validate_core_id_field("host_to_container", "init", "job_id", &value)
            .expect_err("overflow id should fail");
        assert!(matches!(
            err,
            crate::IpcError::Protocol(crate::ProtocolError::OutboundValidation { .. })
        ));
    }

    #[test]
    fn outbound_validators_accept_valid_messages() {
        let group = GroupInfo {
            id: GroupId::new("group-main").expect("valid group id"),
            name: "Main".parse().expect("valid name"),
            is_main: true,
            capabilities: GroupCapabilities::default(),
        };
        let init = HostToContainer::Init(InitPayload {
            job_id: JobId::new("job-1").expect("valid job id"),
            context: InitContext {
                messages: HistoricalMessages::default(),
                group: group.clone(),
                timezone: "UTC".parse().expect("valid timezone"),
            },
            config: InitConfig {
                provider_proxy_url: "http://proxy.local".parse().expect("valid proxy url"),
                provider_proxy_token: "token".parse().expect("valid token"),
                model: "claude-sonnet-4-6".parse().expect("valid model"),
                max_tokens: 1024,
                session_id: None,
                tools_enabled: true,
                timeout_seconds: 300,
            },
        });
        validate_outbound_host_to_container(&init).expect("valid host message");

        let messages = HostToContainer::Messages(MessagesPayload {
            job_id: JobId::new("job-1").expect("valid job id"),
            messages: HistoricalMessages::default(),
        });
        validate_outbound_host_to_container(&messages).expect("valid messages payload");

        let delta = ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "hello".parse().expect("valid output text"),
            job_id: JobId::new("job-1").expect("valid job id"),
        });
        validate_outbound_container_to_host(&delta).expect("valid container message");
    }
}
