//! Shared payload types used by more than one protocol message.
//!
//! Every type in this module mirrors a structure documented in
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md).

use forgeclaw_core::GroupId;
use serde::{Deserialize, Serialize};

use super::semantic::{IdentifierText, IpcTimestamp, MessageText};

/// A historical chat message included in the `init.context.messages`
/// array or the `messages` follow-up envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HistoricalMessage {
    /// Display name of the message sender.
    pub sender: IdentifierText,
    /// Raw message text.
    pub text: MessageText,
    /// RFC3339 timestamp for when this historical message was emitted.
    pub timestamp: IpcTimestamp,
}

/// Capabilities granted to a group, determining which command
/// families are available beyond the base scoped set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GroupCapabilities {
    /// Whether this group may dispatch Tanren jobs.
    #[serde(default)]
    pub tanren: bool,
}

/// Summary of the group the agent is serving for this job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GroupInfo {
    /// Stable group identifier (e.g. `group-main`).
    pub id: GroupId,
    /// Human-readable group name.
    pub name: IdentifierText,
    /// Whether this group is the `main` group with elevated privileges.
    pub is_main: bool,
    /// Capabilities granted to this group. Determines which command
    /// families the IPC layer will accept from sessions bound to
    /// this group.
    #[serde(default)]
    pub capabilities: GroupCapabilities,
}

/// Token accounting reported alongside an `output_complete` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct TokenUsage {
    /// Input tokens billed to the provider for this turn.
    pub input_tokens: u64,
    /// Output tokens billed to the provider for this turn.
    pub output_tokens: u64,
}

/// Why the model stopped generating the current turn.
///
/// Closed enum on the wire — a value not in this list is a protocol
/// error, not a silent fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model emitted an end-of-turn signal.
    EndTurn,
    /// The model hit its maximum output-token cap.
    MaxTokens,
    /// The model invoked a tool and is waiting for a tool result.
    ToolUse,
}

/// Classification of an agent-reported error.
///
/// Closed enum on the wire — a value not in this list is a protocol
/// error. See `docs/IPC_PROTOCOL.md` §Error Handling for meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The model provider returned an error (e.g. HTTP 429).
    ProviderError,
    /// Tool execution inside the agent failed.
    ToolError,
    /// The agent adapter hit an internal failure.
    AdapterError,
    /// The host sent a malformed message.
    ProtocolError,
    /// An adapter-side timeout fired.
    Timeout,
}

/// Why the host is asking the container to shut down.
///
/// Closed enum on the wire — a value not in this list is a protocol
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    /// The container was idle beyond its configured idle TTL.
    IdleTimeout,
    /// The host itself is shutting down.
    HostShutdown,
    /// The warm pool evicted this container.
    Eviction,
    /// A failed job triggered a recover-by-restart path.
    ErrorRecovery,
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorCode, GroupCapabilities, GroupInfo, HistoricalMessage, ShutdownReason, StopReason,
        TokenUsage,
    };
    use forgeclaw_core::GroupId;

    #[test]
    fn stop_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).expect("serialize"),
            "\"end_turn\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::MaxTokens).expect("serialize"),
            "\"max_tokens\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).expect("serialize"),
            "\"tool_use\""
        );
    }

    #[test]
    fn stop_reason_rejects_unknown_value() {
        let err = serde_json::from_str::<StopReason>("\"explode\"")
            .expect_err("unknown stop reason should error");
        assert!(err.to_string().contains("unknown variant"));
    }

    #[test]
    fn error_code_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ErrorCode::ProviderError).expect("serialize"),
            "\"provider_error\""
        );
    }

    #[test]
    fn shutdown_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ShutdownReason::IdleTimeout).expect("serialize"),
            "\"idle_timeout\""
        );
    }

    #[test]
    fn token_usage_roundtrip() {
        let tu = TokenUsage {
            input_tokens: 1500,
            output_tokens: 800,
        };
        let json = serde_json::to_string(&tu).expect("serialize");
        let back: TokenUsage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, tu);
    }

    #[test]
    fn historical_message_roundtrip() {
        let msg = HistoricalMessage {
            sender: "Alice".parse().expect("valid sender"),
            text: "Hey @bot".parse().expect("valid text"),
            timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: HistoricalMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, msg);
    }

    #[test]
    fn group_info_roundtrip() {
        let g = GroupInfo {
            id: GroupId::new("group-main").expect("valid group id"),
            name: "Main Group".parse().expect("valid group name"),
            is_main: true,
            capabilities: GroupCapabilities::default(),
        };
        let json = serde_json::to_string(&g).expect("serialize");
        let back: GroupInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, g);
    }

    #[test]
    fn group_info_capabilities_default_on_missing() {
        let json = r#"{"id":"group-a","name":"A","is_main":false}"#;
        let g: GroupInfo = serde_json::from_str(json).expect("deserialize");
        assert_eq!(g.capabilities, GroupCapabilities::default());
    }

    #[test]
    fn group_capabilities_tanren_roundtrip() {
        let g = GroupInfo {
            id: GroupId::new("group-tanren").expect("valid group id"),
            name: "Tanren Group".parse().expect("valid name"),
            is_main: false,
            capabilities: GroupCapabilities { tanren: true },
        };
        let json = serde_json::to_string(&g).expect("serialize");
        let back: GroupInfo = serde_json::from_str(&json).expect("deserialize");
        assert!(back.capabilities.tanren);
    }
}
