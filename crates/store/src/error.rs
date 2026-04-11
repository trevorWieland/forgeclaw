//! Store error types and classification.
//!
//! Every store error carries a [`classify`](StoreError::classify) method
//! that maps it into [`forgeclaw_core::ErrorClass`] so upstream callers
//! can decide whether to retry, circuit-break, or halt without inspecting
//! the `Display` string.
//!
//! Connection-string sanitization is built in: any `sea_orm::DbErr` that
//! transits through [`StoreError::from`] has its source message rewritten
//! to redact `postgres://…` / `sqlite://…` URLs before the error is ever
//! formatted. See the module-private `sanitize_message` helper.

use std::time::Duration;

use forgeclaw_core::ErrorClass;
use sea_orm::DbErr;

/// Errors returned by the `store` crate.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A database error from SeaORM, with its message sanitized of
    /// any embedded connection URLs.
    #[error("database error: {message}")]
    Database {
        /// Sanitized, human-readable summary of the underlying error.
        message: String,
        /// Original error for diagnostic chain walking.
        #[source]
        source: DbErr,
    },

    /// A cursor value that cannot be used to drive a query.
    #[error("invalid cursor: {reason}")]
    InvalidCursor {
        /// Why the cursor was rejected.
        reason: String,
    },

    /// A connection URL whose scheme is not supported.
    #[error("invalid store URL: {reason}")]
    InvalidUrl {
        /// Human-readable description of the rejection.
        reason: String,
    },

    /// A `serde_json` serialization failure when encoding a JSON payload.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl From<DbErr> for StoreError {
    fn from(err: DbErr) -> Self {
        let raw = err.to_string();
        Self::Database {
            message: sanitize_message(&raw),
            source: err,
        }
    }
}

impl StoreError {
    /// Classify this error for the recovery machinery in
    /// [`forgeclaw_core::ErrorClass`].
    ///
    /// Network / pool / deadlock errors become
    /// [`Transient`](ErrorClass::Transient); bad URLs become
    /// [`Config`](ErrorClass::Config); everything else (schema mismatch,
    /// serialization bug, integrity violation, migration failure) becomes
    /// [`Fatal`](ErrorClass::Fatal).
    #[must_use]
    pub fn classify(&self) -> ErrorClass {
        match self {
            Self::Database { source, .. } => classify_db_err(source),
            Self::InvalidUrl { reason } => ErrorClass::Config {
                key: "store.url".to_owned(),
                reason: reason.clone(),
            },
            Self::InvalidCursor { reason } => ErrorClass::Fatal {
                reason: format!("invalid cursor: {reason}"),
            },
            Self::Serialization(e) => ErrorClass::Fatal {
                reason: format!("json serialization: {e}"),
            },
        }
    }
}

/// Strip connection-string URLs out of an error message.
///
/// Matches `postgresql://`, `postgres://`, and `sqlite://` prefixes and
/// replaces the entire match (up to the next whitespace, quote, or
/// closing bracket) with `<url-redacted>`.
fn sanitize_message(raw: &str) -> String {
    let schemes = ["postgresql://", "postgres://", "sqlite://"];
    let mut out = raw.to_owned();
    for scheme in schemes {
        while let Some(start) = out.find(scheme) {
            let tail = &out[start..];
            let end_offset = tail
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '>' || c == ')')
                .unwrap_or(tail.len());
            let end = start + end_offset;
            out.replace_range(start..end, "<url-redacted>");
        }
    }
    out
}

fn classify_db_err(err: &DbErr) -> ErrorClass {
    match err {
        DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => ErrorClass::Transient {
            retry_after: Duration::from_millis(500),
        },
        DbErr::Exec(runtime) | DbErr::Query(runtime) => classify_runtime(&runtime.to_string()),
        DbErr::Migration(reason) => ErrorClass::Fatal {
            reason: format!("migration failed: {}", sanitize_message(reason)),
        },
        _ => ErrorClass::Fatal {
            reason: sanitize_message(&err.to_string()),
        },
    }
}

/// Classify a runtime (Exec / Query) error string by pattern-matching
/// on known SQLSTATE codes and SQLite busy messages.
fn classify_runtime(raw: &str) -> ErrorClass {
    let sanitized = sanitize_message(raw);
    let lower = sanitized.to_ascii_lowercase();

    // Postgres: 40001 = serialization failure, 40P01 = deadlock.
    // SQLite: "database is locked" / "database is busy".
    let is_transient = lower.contains("40001")
        || lower.contains("40p01")
        || lower.contains("database is locked")
        || lower.contains("database is busy")
        || lower.contains("deadlock");

    if is_transient {
        ErrorClass::Transient {
            retry_after: Duration::from_millis(100),
        }
    } else {
        ErrorClass::Fatal { reason: sanitized }
    }
}

#[cfg(test)]
mod tests {
    use super::{StoreError, sanitize_message};
    use forgeclaw_core::ErrorClass;

    #[test]
    fn sanitize_removes_postgres_credentials() {
        let raw = "failed to connect to postgres://admin:hunter2@db.example/forgeclaw";
        let clean = sanitize_message(raw);
        assert!(!clean.contains("hunter2"), "password leaked: {clean}");
        assert!(!clean.contains("admin"), "username leaked: {clean}");
        assert!(clean.contains("<url-redacted>"));
    }

    #[test]
    fn sanitize_removes_sqlite_url() {
        let raw = "open failed: sqlite:///var/tmp/secret.db";
        let clean = sanitize_message(raw);
        assert!(!clean.contains("secret.db"), "path leaked: {clean}");
        assert!(clean.contains("<url-redacted>"));
    }

    #[test]
    fn sanitize_passes_through_plain_messages() {
        let raw = "column 'foo' not found";
        assert_eq!(sanitize_message(raw), raw);
    }

    #[test]
    fn invalid_url_classifies_as_config() {
        let err = StoreError::InvalidUrl {
            reason: "unknown scheme".to_owned(),
        };
        let class = err.classify();
        assert!(
            matches!(&class, ErrorClass::Config { key, .. } if key == "store.url"),
            "expected Config with key=store.url, got {class:?}"
        );
    }

    #[test]
    fn invalid_cursor_classifies_as_fatal() {
        let err = StoreError::InvalidCursor {
            reason: "bad".to_owned(),
        };
        assert!(matches!(err.classify(), ErrorClass::Fatal { .. }));
    }
}
