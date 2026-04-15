//! Payload types for messages sent from the host to the container.
//!
//! Every type here mirrors a message in
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Host → Container.

use forgeclaw_core::JobId;
use serde::{Deserialize, Serialize};

use super::collections::HistoricalMessages;
use super::semantic::{AbsoluteHttpUrl, IanaTimezone, ModelText, SessionIdText, TokenText};
use super::shared::{GroupInfo, ShutdownReason};

/// Context bundle carried inside an `init` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InitContext {
    /// Historical messages making up the current context window.
    pub messages: HistoricalMessages,
    /// Summary of the group the agent is serving.
    pub group: GroupInfo,
    /// IANA timezone name for resolving relative time references.
    pub timezone: IanaTimezone,
}

/// Provider-proxy and adapter configuration carried inside an `init`
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InitConfig {
    /// Base URL the adapter should target for provider calls.
    pub provider_proxy_url: AbsoluteHttpUrl,
    /// Bearer token the adapter should present to the provider proxy.
    pub provider_proxy_token: TokenText,
    /// Model identifier to request from the provider.
    pub model: ModelText,
    /// Maximum output tokens for this job.
    pub max_tokens: u32,
    /// Adapter session identifier to resume (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionIdText>,
    /// Whether tool use is permitted for this job.
    pub tools_enabled: bool,
    /// Hard timeout (in seconds) for the whole job.
    pub timeout_seconds: u32,
}

/// `init` — initial context and configuration for processing a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct InitPayload {
    /// Unique job identifier for correlation with downstream messages.
    pub job_id: JobId,
    /// Message history, group info, timezone.
    pub context: InitContext,
    /// Provider proxy URL/token, model, limits.
    pub config: InitConfig,
}

/// `messages` — follow-up messages that arrived while the container
/// was processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct MessagesPayload {
    /// Job identifier this follow-up belongs to.
    pub job_id: JobId,
    /// New historical messages to append to the context window.
    pub messages: HistoricalMessages,
}

/// `shutdown` — request graceful shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ShutdownPayload {
    /// Why the host is asking for shutdown.
    pub reason: ShutdownReason,
    /// Deadline in milliseconds. After this deadline the host will
    /// forcibly terminate the container.
    pub deadline_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{InitConfig, InitContext, InitPayload, MessagesPayload, ShutdownPayload};
    use crate::message::HistoricalMessages;
    use crate::message::semantic::{AbsoluteHttpUrl, ModelText, SessionIdText, TokenText};
    use crate::message::shared::{GroupCapabilities, GroupInfo, HistoricalMessage, ShutdownReason};
    use forgeclaw_core::{GroupId, JobId};

    fn sample_init() -> InitPayload {
        InitPayload {
            job_id: JobId::from("job-abc123"),
            context: InitContext {
                messages: vec![
                    HistoricalMessage {
                        sender: "Alice".parse().expect("valid sender"),
                        text: "Hey @bot, what's the weather?".parse().expect("valid text"),
                        timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
                    },
                    HistoricalMessage {
                        sender: "Bob".parse().expect("valid sender"),
                        text: "Also curious!".parse().expect("valid text"),
                        timestamp: "2026-04-03T10:01:00Z".parse().expect("valid timestamp"),
                    },
                ]
                .try_into()
                .expect("messages within bound"),
                group: GroupInfo {
                    id: GroupId::from("group-main"),
                    name: "Main Group".parse().expect("valid group name"),
                    is_main: true,
                    capabilities: GroupCapabilities::default(),
                },
                timezone: "America/New_York".parse().expect("valid timezone"),
            },
            config: InitConfig {
                provider_proxy_url: AbsoluteHttpUrl::new("http://host.docker.internal:9090/v1")
                    .expect("valid url"),
                provider_proxy_token: TokenText::new("abc123def456").expect("valid token"),
                model: ModelText::new("claude-sonnet-4-20250514").expect("valid model"),
                max_tokens: 32000,
                session_id: Some(SessionIdText::new("sess-xyz789").expect("valid session")),
                tools_enabled: true,
                timeout_seconds: 1800,
            },
        }
    }

    #[test]
    fn init_roundtrip() {
        let i = sample_init();
        let json = serde_json::to_value(&i).expect("serialize");
        let back: InitPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, i);
    }

    #[test]
    fn init_skips_session_id_when_none() {
        let mut i = sample_init();
        i.config.session_id = None;
        let json = serde_json::to_value(&i).expect("serialize");
        let config = json
            .get("config")
            .and_then(|v| v.as_object())
            .expect("config object");
        assert!(!config.contains_key("session_id"));
    }

    #[test]
    fn messages_roundtrip() {
        let m = MessagesPayload {
            job_id: JobId::from("job-abc123"),
            messages: vec![HistoricalMessage {
                sender: "Alice".parse().expect("valid sender"),
                text: "Also check the logs please"
                    .parse()
                    .expect("valid message text"),
                timestamp: "2026-04-03T10:05:00Z".parse().expect("valid timestamp"),
            }]
            .try_into()
            .expect("messages within bound"),
        };
        let json = serde_json::to_value(&m).expect("serialize");
        let back: MessagesPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, m);
    }

    #[test]
    fn messages_rejects_more_than_256() {
        let msg = HistoricalMessage {
            sender: "A".parse().expect("valid sender"),
            text: "x".parse().expect("valid text"),
            timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
        };
        let payload = serde_json::json!({
            "job_id": "job-1",
            "messages": std::iter::repeat_n(serde_json::to_value(&msg).expect("serialize"), 257)
                .collect::<Vec<_>>()
        });
        let err = serde_json::from_value::<MessagesPayload>(payload)
            .expect_err("messages >256 should fail");
        assert!(err.to_string().contains("256"));
    }

    #[test]
    fn historical_messages_constructor_rejects_over_256() {
        let msg = HistoricalMessage {
            sender: "A".parse().expect("valid sender"),
            text: "x".parse().expect("valid text"),
            timestamp: "2026-04-03T10:00:00Z".parse().expect("valid timestamp"),
        };
        let err = HistoricalMessages::new(std::iter::repeat_n(msg, 257).collect())
            .expect_err("messages >256 should fail");
        assert_eq!(err.max, 256);
        assert_eq!(err.actual, 257);
    }

    #[test]
    fn shutdown_roundtrip() {
        let s = ShutdownPayload {
            reason: ShutdownReason::IdleTimeout,
            deadline_ms: 10_000,
        };
        let json = serde_json::to_value(&s).expect("serialize");
        assert_eq!(json["reason"], "idle_timeout");
        assert_eq!(json["deadline_ms"], 10_000);
        let back: ShutdownPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, s);
    }
}
