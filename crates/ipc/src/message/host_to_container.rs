//! Payload types for messages sent from the host to the container.
//!
//! Every type here mirrors a message in
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Host → Container.

use forgeclaw_core::JobId;
use serde::{Deserialize, Serialize};

use super::shared::{GroupInfo, HistoricalMessage, ShutdownReason};

/// Context bundle carried inside an `init` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitContext {
    /// Historical messages making up the current context window.
    pub messages: Vec<HistoricalMessage>,
    /// Summary of the group the agent is serving.
    pub group: GroupInfo,
    /// IANA timezone name for resolving relative time references.
    pub timezone: String,
}

/// Provider-proxy and adapter configuration carried inside an `init`
/// message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitConfig {
    /// Base URL the adapter should target for provider calls.
    pub provider_proxy_url: String,
    /// Bearer token the adapter should present to the provider proxy.
    pub provider_proxy_token: String,
    /// Model identifier to request from the provider.
    pub model: String,
    /// Maximum output tokens for this job.
    pub max_tokens: u32,
    /// Adapter session identifier to resume (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Whether tool use is permitted for this job.
    pub tools_enabled: bool,
    /// Hard timeout (in seconds) for the whole job.
    pub timeout_seconds: u32,
}

/// `init` — initial context and configuration for processing a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct MessagesPayload {
    /// Job identifier this follow-up belongs to.
    pub job_id: JobId,
    /// New historical messages to append to the context window.
    pub messages: Vec<HistoricalMessage>,
}

/// `shutdown` — request graceful shutdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    use crate::message::shared::{GroupInfo, HistoricalMessage, ShutdownReason};
    use forgeclaw_core::{GroupId, JobId};

    fn sample_init() -> InitPayload {
        InitPayload {
            job_id: JobId::from("job-abc123"),
            context: InitContext {
                messages: vec![
                    HistoricalMessage {
                        sender: "Alice".to_owned(),
                        text: "Hey @bot, what's the weather?".to_owned(),
                        timestamp: "2026-04-03T10:00:00Z".to_owned(),
                    },
                    HistoricalMessage {
                        sender: "Bob".to_owned(),
                        text: "Also curious!".to_owned(),
                        timestamp: "2026-04-03T10:01:00Z".to_owned(),
                    },
                ],
                group: GroupInfo {
                    id: GroupId::from("group-main"),
                    name: "Main Group".to_owned(),
                    is_main: true,
                },
                timezone: "America/New_York".to_owned(),
            },
            config: InitConfig {
                provider_proxy_url: "http://host.docker.internal:9090/v1".to_owned(),
                provider_proxy_token: "abc123def456".to_owned(),
                model: "claude-sonnet-4-20250514".to_owned(),
                max_tokens: 32000,
                session_id: Some("sess-xyz789".to_owned()),
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
                sender: "Alice".to_owned(),
                text: "Also check the logs please".to_owned(),
                timestamp: "2026-04-03T10:05:00Z".to_owned(),
            }],
        };
        let json = serde_json::to_value(&m).expect("serialize");
        let back: MessagesPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, m);
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
