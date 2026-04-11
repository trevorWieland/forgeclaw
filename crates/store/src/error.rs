//! Store error types and classification.
//!
//! Every store error carries a [`classify`](StoreError::classify) method
//! that maps it into [`forgeclaw_core::ErrorClass`] so upstream callers
//! can decide whether to retry, circuit-break, or halt without inspecting
//! the `Display` string.
//!
//! # Credential redaction contract
//!
//! `StoreError::Database` does **not** retain the raw `sea_orm::DbErr`
//! that produced it. At the `From<DbErr>` boundary the original error is
//! walked once, every layer has its `to_string()` sanitized by a
//! scheme-scanning redactor, and the result is stored as a plain
//! `String` alongside a [`DatabaseCategory`] classification hint. The
//! raw `DbErr` is then dropped.
//!
//! This means `Display`, `Debug`, AND `std::error::Error::source()` on
//! a `StoreError::Database` cannot leak connection URLs under any
//! common logging pattern (`tracing::error!(error = ?err)`,
//! `anyhow::Error::chain()`, etc.). The trade-off is that the exact
//! error-chain walk used for debugging is not available on
//! `StoreError`; callers who need the raw error must observe it
//! inside this crate before conversion.

use std::time::Duration;

use forgeclaw_core::ErrorClass;
use sea_orm::DbErr;

/// Errors returned by the `store` crate.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A database error. Original `DbErr` is intentionally dropped at
    /// the conversion boundary; `message` is pre-sanitized.
    #[error("database error [{category:?}]: {message}")]
    Database {
        /// Sanitized, human-readable summary of the underlying error.
        message: String,
        /// Classification of the underlying database failure.
        category: DatabaseCategory,
    },

    /// Caller-supplied limit is negative or otherwise cannot be used as
    /// a paging bound. Zero is accepted and returns an empty result.
    #[error("invalid limit: {reason}")]
    InvalidLimit {
        /// Human-readable description of the rejection.
        reason: String,
    },

    /// A row decoded from the database contains a value the current
    /// code doesn't understand (e.g. unknown `TaskStatus`). Treated as
    /// a schema-drift signal.
    #[error("schema drift in column `{column}`: {reason}")]
    SchemaDrift {
        /// Column whose value could not be decoded.
        column: String,
        /// Human-readable description of the mismatch.
        reason: String,
    },

    /// A requested row was not present. Callers use this to distinguish
    /// "missing" from "store failure" for update paths that do not
    /// naturally return `Option`.
    #[error("{entity} not found")]
    NotFound {
        /// Entity type that was missing.
        entity: String,
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

/// Coarse classification of a `sea_orm::DbErr`, kept inside
/// `StoreError::Database` so `classify()` can map to an `ErrorClass`
/// without re-inspecting the raw error (which is intentionally dropped
/// for credential-redaction reasons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseCategory {
    /// Connection or pool acquisition failure — retryable.
    Connection,
    /// Runtime Exec / Query failure — inspect message for deadlock / busy.
    Runtime,
    /// Migration infrastructure failure.
    Migration,
    /// Schema mismatch (column decode, type errors).
    Schema,
    /// Integrity constraint violation (unique, foreign key, etc.).
    Integrity,
    /// Anything else.
    Other,
}

impl From<DbErr> for StoreError {
    fn from(err: DbErr) -> Self {
        let category = categorize(&err);
        let message = sanitize_full_chain(&err);
        // Emit a structured debug event at the moment of conversion so
        // operators retain per-call diagnostic granularity even though
        // the raw `DbErr` is about to be dropped.
        tracing::debug!(
            target: "forgeclaw_store::error",
            ?category,
            error = %message,
            "converting DbErr into StoreError"
        );
        Self::Database { message, category }
    }
}

impl StoreError {
    /// Classify this error for the recovery machinery in
    /// [`forgeclaw_core::ErrorClass`].
    #[must_use]
    pub fn classify(&self) -> ErrorClass {
        match self {
            Self::Database { message, category } => classify_database(*category, message),
            Self::InvalidUrl { reason } => ErrorClass::Config {
                key: "store.url".to_owned(),
                reason: reason.clone(),
            },
            Self::InvalidLimit { reason } => ErrorClass::Fatal {
                reason: format!("invalid limit: {reason}"),
            },
            Self::SchemaDrift { column, reason } => ErrorClass::Fatal {
                reason: format!("schema drift in `{column}`: {reason}"),
            },
            Self::NotFound { entity } => ErrorClass::Fatal {
                reason: format!("{entity} not found"),
            },
            Self::Serialization(e) => ErrorClass::Fatal {
                reason: format!("json serialization: {e}"),
            },
        }
    }
}

/// Walk the full error-source chain once, sanitizing each layer's
/// `to_string()` output and joining them with `" | "`. This captures
/// as much diagnostic detail as possible at conversion time without
/// retaining the raw `DbErr` afterwards.
fn sanitize_full_chain(err: &DbErr) -> String {
    let mut parts = vec![sanitize_message(&err.to_string())];
    let mut cur: Option<&dyn std::error::Error> = std::error::Error::source(err);
    while let Some(e) = cur {
        parts.push(sanitize_message(&e.to_string()));
        cur = e.source();
    }
    parts.join(" | ")
}

fn categorize(err: &DbErr) -> DatabaseCategory {
    match err {
        DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => DatabaseCategory::Connection,
        DbErr::Exec(_) | DbErr::Query(_) => DatabaseCategory::Runtime,
        DbErr::Migration(_) => DatabaseCategory::Migration,
        DbErr::Type(_) | DbErr::Json(_) | DbErr::AttrNotSet(_) | DbErr::ConvertFromU64(_) => {
            DatabaseCategory::Schema
        }
        DbErr::RecordNotInserted | DbErr::RecordNotUpdated => DatabaseCategory::Integrity,
        _ => DatabaseCategory::Other,
    }
}

fn classify_database(category: DatabaseCategory, message: &str) -> ErrorClass {
    match category {
        DatabaseCategory::Connection => ErrorClass::Transient {
            retry_after: Duration::from_millis(500),
        },
        DatabaseCategory::Runtime => classify_runtime(message),
        DatabaseCategory::Migration => ErrorClass::Fatal {
            reason: format!("migration failed: {message}"),
        },
        DatabaseCategory::Schema => ErrorClass::Fatal {
            reason: format!("schema mismatch: {message}"),
        },
        DatabaseCategory::Integrity => ErrorClass::Fatal {
            reason: format!("db integrity: {message}"),
        },
        DatabaseCategory::Other => ErrorClass::Fatal {
            reason: message.to_owned(),
        },
    }
}

/// Classify a runtime (Exec / Query) error string by pattern-matching
/// on known SQLSTATE codes and SQLite busy messages.
fn classify_runtime(sanitized: &str) -> ErrorClass {
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
        ErrorClass::Fatal {
            reason: sanitized.to_owned(),
        }
    }
}

/// Strip connection-string URLs out of an error message.
///
/// Matches `postgresql://`, `postgres://`, and `sqlite://` prefixes and
/// replaces the entire match (up to the next whitespace, quote, or
/// closing bracket) with `<url-redacted>`.
pub(crate) fn sanitize_message(raw: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{DatabaseCategory, StoreError, sanitize_message};
    use forgeclaw_core::ErrorClass;
    use sea_orm::{DbErr, RuntimeErr};

    fn synth_dberr_with_dsn() -> DbErr {
        // `RuntimeErr::Internal` wraps a raw string, which is exactly the
        // shape sqlx uses to smuggle connection strings into error
        // messages (e.g. `Error::Configuration`).
        DbErr::Conn(RuntimeErr::Internal(
            "failed to dial postgres://admin:hunter2@db.example/forgeclaw".to_owned(),
        ))
    }

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
    fn invalid_limit_classifies_as_fatal() {
        let err = StoreError::InvalidLimit {
            reason: "negative".to_owned(),
        };
        assert!(matches!(err.classify(), ErrorClass::Fatal { .. }));
    }

    #[test]
    fn schema_drift_classifies_as_fatal_with_prefix() {
        let err = StoreError::SchemaDrift {
            column: "status".to_owned(),
            reason: "unknown value `bogus`".to_owned(),
        };
        let class = err.classify();
        assert!(matches!(&class, ErrorClass::Fatal { .. }));
        if let ErrorClass::Fatal { reason } = class {
            assert!(reason.contains("schema drift in `status`"));
            assert!(reason.contains("unknown value"));
        }
    }

    #[test]
    fn not_found_classifies_as_fatal() {
        let err = StoreError::NotFound {
            entity: "task".to_owned(),
        };
        assert!(matches!(err.classify(), ErrorClass::Fatal { .. }));
    }

    #[test]
    fn connection_category_is_transient() {
        let err = StoreError::Database {
            message: "timeout".to_owned(),
            category: DatabaseCategory::Connection,
        };
        assert!(matches!(err.classify(), ErrorClass::Transient { .. }));
    }

    #[test]
    fn runtime_deadlock_is_transient() {
        let err = StoreError::Database {
            message: "ERROR: deadlock detected (SQLSTATE 40P01)".to_owned(),
            category: DatabaseCategory::Runtime,
        };
        assert!(matches!(err.classify(), ErrorClass::Transient { .. }));
    }

    #[test]
    fn runtime_plain_failure_is_fatal() {
        let err = StoreError::Database {
            message: "syntax error at or near `SELEKT`".to_owned(),
            category: DatabaseCategory::Runtime,
        };
        assert!(matches!(err.classify(), ErrorClass::Fatal { .. }));
    }

    #[test]
    fn integrity_category_is_fatal() {
        let err = StoreError::Database {
            message: "UNIQUE constraint failed".to_owned(),
            category: DatabaseCategory::Integrity,
        };
        let class = err.classify();
        assert!(matches!(&class, ErrorClass::Fatal { .. }));
        if let ErrorClass::Fatal { reason } = class {
            assert!(reason.contains("db integrity"));
        }
    }

    #[test]
    fn database_error_display_redacts_dsn() {
        let err: StoreError = synth_dberr_with_dsn().into();
        let display = format!("{err}");
        assert!(
            !display.contains("hunter2"),
            "password leaked via Display: {display}"
        );
        assert!(
            !display.contains("admin"),
            "username leaked via Display: {display}"
        );
        assert!(display.contains("<url-redacted>"));
    }

    #[test]
    fn database_error_debug_redacts_dsn() {
        let err: StoreError = synth_dberr_with_dsn().into();
        let debug = format!("{err:?}");
        assert!(
            !debug.contains("hunter2"),
            "password leaked via Debug: {debug}"
        );
        assert!(
            !debug.contains("admin"),
            "username leaked via Debug: {debug}"
        );
        assert!(
            !debug.contains("postgres://"),
            "raw URL leaked via Debug: {debug}"
        );
    }

    #[test]
    fn database_error_source_chain_is_redacted() {
        let err: StoreError = synth_dberr_with_dsn().into();
        let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&err);
        let mut depth = 0;
        while let Some(e) = cur {
            let layer = e.to_string();
            assert!(
                !layer.contains("hunter2"),
                "password leaked at source depth {depth}: {layer}"
            );
            assert!(
                !layer.contains("postgres://"),
                "raw URL leaked at source depth {depth}: {layer}"
            );
            cur = e.source();
            depth += 1;
            // Defensive: don't loop forever on a cycle we don't expect.
            assert!(depth < 16, "error chain unexpectedly deep");
        }
    }
}
