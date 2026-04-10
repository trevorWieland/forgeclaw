//! Error taxonomy for deterministic recovery.
//!
//! Every error in Forgeclaw is classified into an [`ErrorClass`] so that
//! callers can decide how to recover without inspecting error messages.

use std::time::Duration;

use crate::id::{ContainerId, ProviderId};

/// Classification of errors for recovery decisions.
///
/// Each variant maps to a specific recovery strategy:
/// - [`Transient`](ErrorClass::Transient) — automatic retry with backoff
/// - [`Auth`](ErrorClass::Auth) — circuit-break the provider, notify channel
/// - [`Config`](ErrorClass::Config) — report and halt, don't retry
/// - [`Container`](ErrorClass::Container) — cleanup and re-provision
/// - [`Fatal`](ErrorClass::Fatal) — graceful shutdown with queue drain
#[derive(Debug, Clone, thiserror::Error)]
pub enum ErrorClass {
    /// A transient failure that should be retried after a delay.
    #[error("transient error (retry after {}ms)", retry_after.as_millis())]
    Transient {
        /// How long to wait before retrying.
        retry_after: Duration,
    },

    /// An authentication or authorization failure for a specific provider.
    #[error("auth error for provider {provider}: {reason}")]
    Auth {
        /// The provider that failed authentication.
        provider: ProviderId,
        /// Human-readable description of the failure.
        reason: String,
    },

    /// A configuration error (missing or invalid config value).
    #[error("config error for key '{key}': {reason}")]
    Config {
        /// The configuration key that is invalid or missing.
        key: String,
        /// Human-readable description of the problem.
        reason: String,
    },

    /// A container lifecycle error.
    #[error("container error{}: {reason}", id.as_ref().map_or_else(String::new, |id| format!(" for {id}")))]
    Container {
        /// The container that failed, if known.
        id: Option<ContainerId>,
        /// Human-readable description of the failure.
        reason: String,
    },

    /// A fatal error requiring graceful shutdown.
    #[error("fatal error: {reason}")]
    Fatal {
        /// Human-readable description of the fatal condition.
        reason: String,
    },
}

impl ErrorClass {
    /// Returns `true` if this error is transient and should be retried.
    pub fn is_retriable(&self) -> bool {
        matches!(self, Self::Transient { .. })
    }
}

/// Errors that can occur when loading or validating configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// An I/O error reading the config file.
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    /// A TOML parsing or deserialization error.
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),

    /// One or more cross-reference validation errors.
    #[error("config validation failed: {}", errors.join("; "))]
    Validation {
        /// List of validation error descriptions.
        errors: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_display() {
        let err = ErrorClass::Transient {
            retry_after: Duration::from_millis(500),
        };
        insta::assert_snapshot!(err.to_string(), @"transient error (retry after 500ms)");
    }

    #[test]
    fn auth_display() {
        let err = ErrorClass::Auth {
            provider: ProviderId::from("anthropic"),
            reason: "invalid API key".to_owned(),
        };
        insta::assert_snapshot!(err.to_string(), @"auth error for provider anthropic: invalid API key");
    }

    #[test]
    fn config_display() {
        let err = ErrorClass::Config {
            key: "runtime.log_level".to_owned(),
            reason: "unknown level 'verbose'".to_owned(),
        };
        insta::assert_snapshot!(
            err.to_string(),
            @"config error for key 'runtime.log_level': unknown level 'verbose'"
        );
    }

    #[test]
    fn container_display_with_id() {
        let err = ErrorClass::Container {
            id: Some(ContainerId::from("ctr-1")),
            reason: "OOM killed".to_owned(),
        };
        insta::assert_snapshot!(err.to_string(), @"container error for ctr-1: OOM killed");
    }

    #[test]
    fn container_display_without_id() {
        let err = ErrorClass::Container {
            id: None,
            reason: "startup failure".to_owned(),
        };
        insta::assert_snapshot!(err.to_string(), @"container error: startup failure");
    }

    #[test]
    fn fatal_display() {
        let err = ErrorClass::Fatal {
            reason: "database unavailable".to_owned(),
        };
        insta::assert_snapshot!(err.to_string(), @"fatal error: database unavailable");
    }

    #[test]
    fn is_retriable_only_for_transient() {
        assert!(
            ErrorClass::Transient {
                retry_after: Duration::from_secs(1)
            }
            .is_retriable()
        );

        assert!(
            !ErrorClass::Auth {
                provider: ProviderId::from("x"),
                reason: String::new(),
            }
            .is_retriable()
        );

        assert!(
            !ErrorClass::Config {
                key: String::new(),
                reason: String::new(),
            }
            .is_retriable()
        );

        assert!(
            !ErrorClass::Container {
                id: None,
                reason: String::new(),
            }
            .is_retriable()
        );

        assert!(
            !ErrorClass::Fatal {
                reason: String::new(),
            }
            .is_retriable()
        );
    }
}
