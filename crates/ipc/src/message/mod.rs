//! Protocol message types.
//!
//! This module defines the two top-level enums that constitute the
//! Forgeclaw IPC protocol, plus their payload structs and shared
//! helpers:
//!
//! - [`ContainerToHost`] — every message an agent container may send
//!   to the host.
//! - [`HostToContainer`] — every message the host may send to a
//!   container.
//!
//! Both enums are serde-tagged with `#[serde(tag = "type",
//! rename_all = "snake_case")]` so the wire representation matches
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md) exactly
//! (e.g. `{"type": "ready", ...}`).
//!
//! Unknown fields inside a payload are tolerated (forward-compatible)
//! but unknown message *types* deserialize as
//! [`crate::error::ProtocolError::UnknownMessageType`] inside the
//! codec — policy (log-and-continue vs tear-down) is left to the
//! caller, per `docs/IPC_PROTOCOL.md` §Error Handling.

pub mod authorized;
mod collections;
pub mod command;
pub mod container_to_host;
pub mod host_to_container;
pub(crate) mod limits;
pub mod semantic;
pub mod shared;

use serde::{Deserialize, Serialize};

pub use authorized::{
    AuthorizedCommand, GroupCommand, MainGroupCommand, OwnershipPending,
    PrivilegedAuthorizedCommand, ScopedAuthorizedCommand,
};
pub use collections::{BoundedCollectionError, HistoricalMessages, SelfImprovementListItems};
pub use command::{
    BranchPolicy, CancelTaskPayload, CommandBody, CommandPayload, DispatchSelfImprovementPayload,
    DispatchTanrenPayload, GroupExtensions, GroupExtensionsError, GroupExtensionsVersion,
    GroupExtensionsVersionError, PauseTaskPayload, RegisterGroupPayload, ScheduleTaskPayload,
    ScheduleType, SendMessagePayload, TanrenPhase,
};
pub use container_to_host::{
    ErrorPayload, HeartbeatPayload, OutputCompletePayload, OutputDeltaPayload, Percent,
    PercentError, ProgressPayload, ReadyPayload,
};
pub use host_to_container::{
    InitConfig, InitContext, InitPayload, MessagesPayload, ShutdownPayload,
};
pub use semantic::{
    AbsoluteHttpUrl, AbsoluteHttpUrlError, AdapterName, AdapterVersion, BoundedTextError,
    BranchName, ContextModeText, EnvironmentProfileText, GroupName, ListItemText, MessageText,
    ModelText, OutputDeltaText, OutputResultText, ProjectName, PromptText, ProtocolVersionText,
    ScheduleValueText, SenderName, SessionIdText, ShortText, StageName, TokenText,
};
pub use semantic::{IanaTimezone, IpcTimestamp, TimestampError, TimezoneError};
pub use shared::{
    ErrorCode, GroupCapabilities, GroupInfo, HistoricalMessage, ShutdownReason, StopReason,
    TokenUsage,
};

/// A message sent from an agent container to the host.
///
/// Serialized as an internally-tagged JSON object with a `type`
/// discriminator. See [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
/// §Container → Host for the wire format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContainerToHost {
    /// `ready` — adapter has initialized and is ready to receive work.
    Ready(ReadyPayload),
    /// `output_delta` — incremental text chunk streamed from the model.
    OutputDelta(OutputDeltaPayload),
    /// `output_complete` — final result for a job.
    OutputComplete(OutputCompletePayload),
    /// `progress` — optional progress signal for long-running work.
    Progress(ProgressPayload),
    /// `command` — agent-to-host command (send message, schedule task,
    /// dispatch Tanren, etc.).
    Command(CommandPayload),
    /// `error` — adapter reports an error.
    Error(ErrorPayload),
    /// `heartbeat` — periodic liveness signal.
    Heartbeat(HeartbeatPayload),
}

impl ContainerToHost {
    /// The set of `type` discriminator values this crate recognizes.
    /// Used by the codec's two-pass decode to structurally distinguish
    /// "unknown message type" from "malformed known message".
    pub const KNOWN_TYPES: &'static [&'static str] = &[
        "ready",
        "output_delta",
        "output_complete",
        "progress",
        "command",
        "error",
        "heartbeat",
    ];

    /// Returns the wire `type` name of this message, suitable for
    /// logging and for constructing
    /// [`crate::error::ProtocolError::UnexpectedMessage`].
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Ready(_) => "ready",
            Self::OutputDelta(_) => "output_delta",
            Self::OutputComplete(_) => "output_complete",
            Self::Progress(_) => "progress",
            Self::Command(_) => "command",
            Self::Error(_) => "error",
            Self::Heartbeat(_) => "heartbeat",
        }
    }
}

/// A message sent from the host to an agent container.
///
/// Serialized as an internally-tagged JSON object with a `type`
/// discriminator. See [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
/// §Host → Container for the wire format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToContainer {
    /// `init` — initial context and configuration for a job.
    Init(InitPayload),
    /// `messages` — follow-up messages for an in-flight job.
    Messages(MessagesPayload),
    /// `shutdown` — request graceful shutdown.
    Shutdown(ShutdownPayload),
}

impl HostToContainer {
    /// The set of `type` discriminator values this crate recognizes.
    pub const KNOWN_TYPES: &'static [&'static str] = &["init", "messages", "shutdown"];

    /// Returns the wire `type` name of this message, suitable for
    /// logging and for constructing
    /// [`crate::error::ProtocolError::UnexpectedMessage`].
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Init(_) => "init",
            Self::Messages(_) => "messages",
            Self::Shutdown(_) => "shutdown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CommandBody, CommandPayload, ContainerToHost, HeartbeatPayload, HistoricalMessages,
        HostToContainer, InitConfig, InitContext, InitPayload, MessagesPayload,
        OutputCompletePayload, OutputDeltaPayload, ProgressPayload, ReadyPayload,
        SendMessagePayload, ShutdownPayload,
    };
    use crate::message::shared::{GroupCapabilities, GroupInfo, ShutdownReason, StopReason};
    use forgeclaw_core::{GroupId, JobId};
    use serde_json::json;

    #[test]
    fn container_to_host_ready_tag() {
        let msg = ContainerToHost::Ready(ReadyPayload {
            adapter: "claude-code".parse().expect("valid adapter"),
            adapter_version: "1.0.0".parse().expect("valid adapter version"),
            protocol_version: "1.0".parse().expect("valid protocol version"),
        });
        let json = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(json["type"], "ready");
        assert_eq!(json["adapter"], "claude-code");
        let back: ContainerToHost = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn container_to_host_output_delta_tag() {
        let msg = ContainerToHost::OutputDelta(OutputDeltaPayload {
            text: "x".parse().expect("valid text"),
            job_id: JobId::new("job-1").expect("valid job id"),
        });
        let json = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(json["type"], "output_delta");
        assert_eq!(json["text"], "x");
        assert_eq!(json["job_id"], "job-1");
    }

    #[test]
    fn container_to_host_unknown_type_rejected() {
        let bogus = json!({"type": "bogus_message_type", "foo": 1});
        let err = serde_json::from_value::<ContainerToHost>(bogus)
            .expect_err("bogus variant should fail to deserialize");
        // serde's "unknown variant" message is our signal that we can
        // translate this into `ProtocolError::UnknownMessageType` at
        // the codec layer.
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn container_to_host_unknown_fields_tolerated() {
        // A future minor version may add fields to `ready`; older
        // decoders must still accept them.
        let fwd = json!({
            "type": "ready",
            "adapter": "claude-code",
            "adapter_version": "1.0.0",
            "protocol_version": "1.0",
            "new_field_from_1_1": "ignored"
        });
        let parsed: ContainerToHost = serde_json::from_value(fwd).expect("deserialize");
        assert!(matches!(parsed, ContainerToHost::Ready(_)));
    }

    #[test]
    fn container_to_host_command_wire_shape() {
        let msg = ContainerToHost::Command(CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::new("group-main").expect("valid group id"),
                text: "hello".parse().expect("valid text"),
            }),
        });
        let json = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(json["type"], "command");
        assert_eq!(json["command"], "send_message");
        assert_eq!(json["payload"]["target_group"], "group-main");
        let back: ContainerToHost = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn container_to_host_type_name_matches_wire() {
        let cases: &[(ContainerToHost, &str)] = &[
            (
                ContainerToHost::Ready(ReadyPayload {
                    adapter: "a".parse().expect("valid adapter"),
                    adapter_version: "v".parse().expect("valid adapter version"),
                    protocol_version: "1.0".parse().expect("valid protocol version"),
                }),
                "ready",
            ),
            (
                ContainerToHost::OutputDelta(OutputDeltaPayload {
                    text: String::new().parse().expect("valid output delta"),
                    job_id: JobId::new("j").expect("valid job id"),
                }),
                "output_delta",
            ),
            (
                ContainerToHost::OutputComplete(OutputCompletePayload {
                    job_id: JobId::new("j").expect("valid job id"),
                    result: None,
                    session_id: None,
                    token_usage: None,
                    stop_reason: StopReason::EndTurn,
                }),
                "output_complete",
            ),
            (
                ContainerToHost::Progress(ProgressPayload {
                    job_id: JobId::new("j").expect("valid job id"),
                    stage: "x".parse().expect("valid stage"),
                    detail: None,
                    percent: None,
                }),
                "progress",
            ),
            (
                ContainerToHost::Heartbeat(HeartbeatPayload {
                    timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
                }),
                "heartbeat",
            ),
        ];
        for (msg, expected) in cases {
            assert_eq!(msg.type_name(), *expected);
        }
    }

    #[test]
    fn host_to_container_init_tag() {
        let msg = HostToContainer::Init(InitPayload {
            job_id: JobId::new("job-abc123").expect("valid job id"),
            context: InitContext {
                messages: HistoricalMessages::default(),
                group: GroupInfo {
                    id: GroupId::new("group-main").expect("valid group id"),
                    name: "Main".parse().expect("valid name"),
                    is_main: true,
                    capabilities: GroupCapabilities::default(),
                },
                timezone: "UTC".parse().expect("valid timezone"),
            },
            config: InitConfig {
                provider_proxy_url: "http://proxy".parse().expect("valid proxy url"),
                provider_proxy_token: "token".parse().expect("valid proxy token"),
                model: "model".parse().expect("valid model"),
                max_tokens: 1000,
                session_id: None,
                tools_enabled: true,
                timeout_seconds: 600,
            },
        });
        let json = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(json["type"], "init");
        let back: HostToContainer = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn host_to_container_messages_tag() {
        let msg = HostToContainer::Messages(MessagesPayload {
            job_id: JobId::new("j").expect("valid job id"),
            messages: HistoricalMessages::default(),
        });
        assert_eq!(
            serde_json::to_value(&msg).expect("serialize")["type"],
            "messages"
        );
    }

    #[test]
    fn host_to_container_shutdown_tag() {
        let msg = HostToContainer::Shutdown(ShutdownPayload {
            reason: ShutdownReason::HostShutdown,
            deadline_ms: 5_000,
        });
        let json = serde_json::to_value(&msg).expect("serialize");
        assert_eq!(json["type"], "shutdown");
        assert_eq!(json["reason"], "host_shutdown");
        assert_eq!(json["deadline_ms"], 5_000);
    }

    #[test]
    fn host_to_container_type_name_matches_wire() {
        let init = HostToContainer::Init(InitPayload {
            job_id: JobId::new("j").expect("valid job id"),
            context: InitContext {
                messages: HistoricalMessages::default(),
                group: GroupInfo {
                    id: GroupId::new("g").expect("valid group id"),
                    name: "n".parse().expect("valid name"),
                    is_main: false,
                    capabilities: GroupCapabilities::default(),
                },
                timezone: "UTC".parse().expect("valid timezone"),
            },
            config: InitConfig {
                provider_proxy_url: "https://u.example".parse().expect("valid proxy url"),
                provider_proxy_token: "t".parse().expect("valid proxy token"),
                model: "m".parse().expect("valid model"),
                max_tokens: 1,
                session_id: None,
                tools_enabled: false,
                timeout_seconds: 1,
            },
        });
        assert_eq!(init.type_name(), "init");
        assert_eq!(
            HostToContainer::Messages(MessagesPayload {
                job_id: JobId::new("j").expect("valid job id"),
                messages: HistoricalMessages::default(),
            })
            .type_name(),
            "messages"
        );
        assert_eq!(
            HostToContainer::Shutdown(ShutdownPayload {
                reason: ShutdownReason::Eviction,
                deadline_ms: 0,
            })
            .type_name(),
            "shutdown"
        );
    }
}
