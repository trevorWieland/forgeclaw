//! Payload types for messages sent from the container to the host.
//!
//! Every type here mirrors a message in
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Container → Host.

use std::fmt;

use forgeclaw_core::JobId;
use serde::{Deserialize, Serialize};

use super::semantic::{
    IdentifierText, IpcTimestamp, OutputDeltaText, OutputResultText, SessionIdText, ShortText,
};
use super::shared::{ErrorCode, StopReason, TokenUsage};

/// Deserialize an `Option<T>` that must be present in the JSON (but
/// may be `null`). Adding `deserialize_with` prevents serde's default
/// "missing key → None" behavior: the key must exist on the wire,
/// while `null` still maps to `None`.
fn deserialize_required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(feature = "json-schema")]
fn required_nullable_result_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let text = generator.subschema_for::<OutputResultText>();
    schemars::json_schema!({
        "oneOf": [
            text,
            { "type": "null" }
        ]
    })
}

/// Error returned when a percentage value exceeds the valid 0-100 range.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("percentage {value} exceeds maximum of 100")]
pub struct PercentError {
    /// The invalid value that was rejected.
    pub value: u8,
}

/// A completion percentage bounded to 0-100.
///
/// Used in [`ProgressPayload`] to enforce the documented range at
/// both deserialization time and in the exported JSON Schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Percent(u8);

impl Percent {
    /// Maximum valid percentage value.
    pub const MAX: u8 = 100;

    /// Create a validated percentage.
    ///
    /// # Errors
    ///
    /// Returns [`PercentError`] if `value` exceeds 100.
    pub fn new(value: u8) -> Result<Self, PercentError> {
        if value > Self::MAX {
            return Err(PercentError { value });
        }
        Ok(Self(value))
    }

    /// Returns the inner value.
    #[must_use]
    pub fn value(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.0)
    }
}

impl<'de> Deserialize<'de> for Percent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = u8::deserialize(deserializer)?;
        Self::new(v).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "json-schema")]
impl schemars::JsonSchema for Percent {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Percent".into()
    }

    fn json_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "format": "uint8",
            "minimum": 0,
            "maximum": 100,
            "description": "Completion percentage bounded to 0-100."
        })
    }
}

/// `ready` — sent immediately after connect. Signals the adapter has
/// initialized and is ready to receive work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ReadyPayload {
    /// Name of the agent adapter (e.g. `claude-code`).
    pub adapter: IdentifierText,
    /// Adapter implementation version.
    pub adapter_version: IdentifierText,
    /// IPC protocol version the adapter supports (e.g. `"1.0"`).
    pub protocol_version: IdentifierText,
}

/// `output_delta` — incremental text chunk streamed from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct OutputDeltaPayload {
    /// Incremental text chunk.
    pub text: OutputDeltaText,
    /// Job identifier this chunk belongs to.
    pub job_id: JobId,
}

/// `output_complete` — final result for a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct OutputCompletePayload {
    /// Job identifier this completion belongs to.
    pub job_id: JobId,
    /// Final text output. `None` for tools-only turns.
    ///
    /// Required on the wire even when `null` — adapters must not omit
    /// this key. See `docs/IPC_PROTOCOL.md` §`output_complete`.
    #[serde(deserialize_with = "deserialize_required_nullable")]
    #[cfg_attr(
        feature = "json-schema",
        schemars(required, schema_with = "required_nullable_result_schema")
    )]
    pub result: Option<OutputResultText>,
    /// Adapter-specific session identifier for resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionIdText>,
    /// Token accounting for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
}

/// `progress` — optional progress signal for long-running operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ProgressPayload {
    /// Job this progress signal belongs to.
    pub job_id: JobId,
    /// Logical stage name (e.g. `tool_execution`).
    pub stage: IdentifierText,
    /// Optional human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ShortText>,
    /// Optional completion percentage (0-100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<Percent>,
}

/// `error` — the agent reports an error to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ErrorPayload {
    /// Error classification.
    pub code: ErrorCode,
    /// Human-readable description of the failure.
    pub message: ShortText,
    /// Whether the container should be shut down after this error.
    pub fatal: bool,
    /// Job identifier, if the error relates to a specific job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
}

/// `heartbeat` — periodic liveness signal during processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct HeartbeatPayload {
    /// RFC3339 timestamp when the heartbeat was emitted.
    pub timestamp: IpcTimestamp,
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorPayload, HeartbeatPayload, OutputCompletePayload, OutputDeltaPayload, ProgressPayload,
        ReadyPayload,
    };
    use crate::message::semantic::{
        IdentifierText, OutputDeltaText, OutputResultText, SessionIdText, ShortText,
    };
    use crate::message::shared::{ErrorCode, StopReason, TokenUsage};
    use forgeclaw_core::JobId;
    use serde_json::json;

    #[test]
    fn ready_roundtrip() {
        let r = ReadyPayload {
            adapter: IdentifierText::new("claude-code").expect("valid adapter"),
            adapter_version: IdentifierText::new("1.0.0").expect("valid version"),
            protocol_version: IdentifierText::new("1.0").expect("valid protocol version"),
        };
        let json = serde_json::to_value(&r).expect("serialize");
        assert_eq!(
            json,
            json!({
                "adapter": "claude-code",
                "adapter_version": "1.0.0",
                "protocol_version": "1.0",
            })
        );
        let back: ReadyPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, r);
    }

    #[test]
    fn output_delta_roundtrip() {
        let d = OutputDeltaPayload {
            text: OutputDeltaText::new("Here's what I found...").expect("valid delta"),
            job_id: JobId::from("job-abc123"),
        };
        let json = serde_json::to_value(&d).expect("serialize");
        let back: OutputDeltaPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, d);
    }

    #[test]
    fn output_complete_roundtrip_full() {
        let c = OutputCompletePayload {
            job_id: JobId::from("job-abc123"),
            result: Some(OutputResultText::new("Here's the full response...").expect("valid")),
            session_id: Some(SessionIdText::new("sess-xyz789").expect("valid")),
            token_usage: Some(TokenUsage {
                input_tokens: 1500,
                output_tokens: 800,
            }),
            stop_reason: StopReason::EndTurn,
        };
        let json = serde_json::to_value(&c).expect("serialize");
        let back: OutputCompletePayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, c);
    }

    #[test]
    fn output_complete_skips_none_optionals() {
        let c = OutputCompletePayload {
            job_id: JobId::from("job-1"),
            result: None,
            session_id: None,
            token_usage: None,
            stop_reason: StopReason::ToolUse,
        };
        let json = serde_json::to_value(&c).expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(!obj.contains_key("session_id"));
        assert!(!obj.contains_key("token_usage"));
        // `result` is required even when null.
        assert_eq!(obj.get("result"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn progress_optionals_omitted_when_none() {
        let p = ProgressPayload {
            job_id: JobId::from("job-1"),
            stage: IdentifierText::new("tool_execution").expect("valid stage"),
            detail: None,
            percent: None,
        };
        let json = serde_json::to_value(&p).expect("serialize");
        let obj = json.as_object().expect("object");
        assert!(!obj.contains_key("detail"));
        assert!(!obj.contains_key("percent"));
    }

    #[test]
    fn error_payload_roundtrip() {
        let e = ErrorPayload {
            code: ErrorCode::ProviderError,
            message: ShortText::new("Model returned 429: rate limited").expect("valid message"),
            fatal: false,
            job_id: Some(JobId::from("job-abc123")),
        };
        let json = serde_json::to_value(&e).expect("serialize");
        let back: ErrorPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, e);
    }

    #[test]
    fn heartbeat_roundtrip() {
        let h = HeartbeatPayload {
            timestamp: "2026-04-03T10:30:00Z".parse().expect("valid timestamp"),
        };
        let json = serde_json::to_value(&h).expect("serialize");
        let back: HeartbeatPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, h);
    }

    #[test]
    fn output_complete_rejects_missing_result() {
        let json = json!({
            "job_id": "job-1",
            "stop_reason": "end_turn"
        });
        let err = serde_json::from_value::<OutputCompletePayload>(json)
            .expect_err("missing result should fail");
        assert!(
            err.to_string().contains("result"),
            "error should mention result field: {err}"
        );
    }
}
