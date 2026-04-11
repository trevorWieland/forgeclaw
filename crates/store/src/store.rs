//! The public [`Store`] struct and its async method surface.
//!
//! Each method is a thin delegator to a function in [`crate::ops`]. The
//! internal database connection is a [`sea_orm::DatabaseConnection`] —
//! an enum that transparently holds either an SQLite or a PostgreSQL
//! pool — so the ops code has no dialect branching.

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{GroupId, TaskId};
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use crate::error::StoreError;
use crate::migrations::Migrator;
use crate::ops;
use crate::types::event::{EventFilter, NewEvent, StoredEvent};
use crate::types::group::RegisteredGroup;
use crate::types::message::{Cursor, NewMessage, StoredMessage};
use crate::types::task::{NewTask, StoredTask, TaskRunResult};

/// The persistence handle used by every other Forgeclaw subsystem.
///
/// Construct via [`Store::connect`] for a real database URL, or
/// [`Store::connect_sqlite_memory`] for in-memory SQLite (handy for
/// unit tests). Call [`Store::migrate`] once at startup before any
/// other method.
#[derive(Debug, Clone)]
pub struct Store {
    db: DatabaseConnection,
}

impl Store {
    /// Connect to the store backend identified by `url`.
    ///
    /// Supported schemes: `sqlite://…`, `sqlite::memory:`, `postgres://…`,
    /// `postgresql://…`. Unknown schemes return [`StoreError::InvalidUrl`].
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        if !is_supported_scheme(url) {
            return Err(StoreError::InvalidUrl {
                reason: format!("unsupported URL scheme: {}", scheme_hint(url)),
            });
        }
        let mut opts = ConnectOptions::new(url.to_owned());
        opts.sqlx_logging(false);
        let db = Database::connect(opts).await?;
        Ok(Self { db })
    }

    /// Connect to a fresh in-memory SQLite database. Each call yields
    /// a distinct, independent database.
    pub async fn connect_sqlite_memory() -> Result<Self, StoreError> {
        Self::connect("sqlite::memory:").await
    }

    /// Run every pending migration.
    ///
    /// Safe to call more than once: SeaORM tracks applied migrations
    /// in its own metadata table and skips ones already in place.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        Migrator::up(&self.db, None).await?;
        Ok(())
    }

    // ----- Messages -----

    /// Insert a message into the log.
    pub async fn store_message(&self, msg: &NewMessage) -> Result<(), StoreError> {
        ops::messages::store_message(&self.db, msg).await
    }

    /// Return up to `limit` messages in `group` strictly after `cursor`,
    /// ordered by `(created_at, id)` ascending.
    pub async fn get_messages_since(
        &self,
        group: &GroupId,
        cursor: &Cursor,
        limit: i64,
    ) -> Result<Vec<StoredMessage>, StoreError> {
        ops::messages::get_messages_since(&self.db, group, cursor, limit).await
    }

    // ----- Groups -----

    /// Fetch a group by id, returning `None` if it doesn't exist.
    pub async fn get_group(&self, id: &GroupId) -> Result<Option<RegisteredGroup>, StoreError> {
        ops::groups::get_group(&self.db, id).await
    }

    /// Insert or update a group row, preserving `created_at`.
    pub async fn upsert_group(&self, group: &RegisteredGroup) -> Result<(), StoreError> {
        ops::groups::upsert_group(&self.db, group).await
    }

    // ----- Tasks -----

    /// Create a scheduled task; returns the freshly-generated [`TaskId`].
    pub async fn create_task(&self, task: &NewTask) -> Result<TaskId, StoreError> {
        ops::tasks::create_task(&self.db, task).await
    }

    /// Return every task whose `next_run <= now` with status `active`.
    pub async fn get_due_tasks(&self, now: DateTime<Utc>) -> Result<Vec<StoredTask>, StoreError> {
        ops::tasks::get_due_tasks(&self.db, now).await
    }

    /// Record the outcome of a task run and advance scheduling fields.
    pub async fn update_task_after_run(
        &self,
        id: &TaskId,
        result: &TaskRunResult,
    ) -> Result<(), StoreError> {
        ops::tasks::update_task_after_run(&self.db, id, result).await
    }

    // ----- State (generic key/value) -----

    /// Fetch a state value by key.
    pub async fn get_state(&self, key: &str) -> Result<Option<String>, StoreError> {
        ops::state::get_state(&self.db, key).await
    }

    /// Insert or update a state value.
    pub async fn set_state(&self, key: &str, value: &str) -> Result<(), StoreError> {
        ops::state::set_state(&self.db, key, value).await
    }

    // ----- Sessions -----

    /// Fetch the current agent session id for a group.
    pub async fn get_session(&self, group: &GroupId) -> Result<Option<String>, StoreError> {
        ops::sessions::get_session(&self.db, group).await
    }

    /// Set the agent session id for a group.
    pub async fn set_session(&self, group: &GroupId, session_id: &str) -> Result<(), StoreError> {
        ops::sessions::set_session(&self.db, group, session_id).await
    }

    // ----- Events -----

    /// Append a new event to the audit log.
    pub async fn record_event(&self, event: &NewEvent) -> Result<(), StoreError> {
        ops::events::record_event(&self.db, event).await
    }

    /// List events matching `filter`, newest first, limited to `limit`.
    pub async fn list_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        ops::events::list_events(&self.db, filter, limit).await
    }
}

fn is_supported_scheme(url: &str) -> bool {
    url.starts_with("sqlite://")
        || url.starts_with("sqlite::memory:")
        || url.starts_with("postgres://")
        || url.starts_with("postgresql://")
}

fn scheme_hint(url: &str) -> String {
    url.split("://")
        .next()
        .unwrap_or("<empty>")
        .chars()
        .take(16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Store, is_supported_scheme};

    #[test]
    fn supported_schemes() {
        assert!(is_supported_scheme("sqlite://foo.db"));
        assert!(is_supported_scheme("sqlite::memory:"));
        assert!(is_supported_scheme("postgres://u:p@h/db"));
        assert!(is_supported_scheme("postgresql://u:p@h/db"));
    }

    #[test]
    fn rejected_schemes() {
        assert!(!is_supported_scheme("mysql://host/db"));
        assert!(!is_supported_scheme("file:///foo.db"));
        assert!(!is_supported_scheme("not-a-url"));
    }

    #[tokio::test]
    async fn connect_rejects_unknown_scheme() {
        let err = Store::connect("mysql://root@localhost/db").await;
        assert!(err.is_err());
    }
}
