//! Payload types for messages sent from the container to the host.
//!
//! Every type here mirrors a message in
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Container → Host.

use forgeclaw_core::JobId;
use serde::{Deserialize, Serialize};

use super::shared::{ErrorCode, StopReason, TokenUsage};

/// `ready` — sent immediately after connect. Signals the adapter has
/// initialized and is ready to receive work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyPayload {
    /// Name of the agent adapter (e.g. `claude-code`).
    pub adapter: String,
    /// Adapter implementation version.
    pub adapter_version: String,
    /// IPC protocol version the adapter supports (e.g. `"1.0"`).
    pub protocol_version: String,
}

/// `output_delta` — incremental text chunk streamed from the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputDeltaPayload {
    /// Incremental text chunk.
    pub text: String,
    /// Job identifier this chunk belongs to.
    pub job_id: JobId,
}

/// `output_complete` — final result for a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputCompletePayload {
    /// Job identifier this completion belongs to.
    pub job_id: JobId,
    /// Final text output. `None` for tools-only turns.
    pub result: Option<String>,
    /// Adapter-specific session identifier for resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Token accounting for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<TokenUsage>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
}

/// `progress` — optional progress signal for long-running operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressPayload {
    /// Job this progress signal belongs to.
    pub job_id: JobId,
    /// Logical stage name (e.g. `tool_execution`).
    pub stage: String,
    /// Optional human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional completion percentage (0-100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
}

/// `error` — the agent reports an error to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPayload {
    /// Error classification.
    pub code: ErrorCode,
    /// Human-readable description of the failure.
    pub message: String,
    /// Whether the container should be shut down after this error.
    pub fatal: bool,
    /// Job identifier, if the error relates to a specific job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
}

/// `heartbeat` — periodic liveness signal during processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    /// ISO 8601 timestamp when the heartbeat was emitted. Carried as a
    /// string so the crate stays neutral to timestamp precision.
    pub timestamp: String,
}

#[cfg(test)]
mod tests {
    use super::{
        ErrorPayload, HeartbeatPayload, OutputCompletePayload, OutputDeltaPayload, ProgressPayload,
        ReadyPayload,
    };
    use crate::message::shared::{ErrorCode, StopReason, TokenUsage};
    use forgeclaw_core::JobId;
    use serde_json::json;

    #[test]
    fn ready_roundtrip() {
        let r = ReadyPayload {
            adapter: "claude-code".to_owned(),
            adapter_version: "1.0.0".to_owned(),
            protocol_version: "1.0".to_owned(),
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
            text: "Here's what I found...".to_owned(),
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
            result: Some("Here's the full response...".to_owned()),
            session_id: Some("sess-xyz789".to_owned()),
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
            stage: "tool_execution".to_owned(),
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
            message: "Model returned 429: rate limited".to_owned(),
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
            timestamp: "2026-04-03T10:30:00Z".to_owned(),
        };
        let json = serde_json::to_value(&h).expect("serialize");
        let back: HeartbeatPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, h);
    }
}
