//! Test-only schema-drift check.
//!
//! Compares the set of columns each SeaORM entity expects against
//! the set of columns the live database actually has after
//! migrations run. A mismatch is the clearest possible symptom of
//! "entity changed but migration didn't" (or vice versa).
//!
//! This is not a compile-time check, but it is a hard CI failure —
//! every build runs it against SQLite, and the Postgres job runs it
//! against real Postgres.

use std::collections::BTreeSet;

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbBackend, EntityTrait, Schema,
    Statement, sea_query::ColumnDef,
};

use crate::entities::{events, groups, messages, sessions, state, tasks};
use crate::error::StoreError;

/// Run the drift check against `db`. Returns `Ok(())` if every entity's
/// expected columns match the live table's columns; otherwise
/// [`StoreError::SchemaDrift`] with a diagnostic describing the first
/// mismatched table.
pub async fn check(db: &DatabaseConnection) -> Result<(), StoreError> {
    check_entity(db, messages::Entity, "messages").await?;
    check_entity(db, groups::Entity, "groups").await?;
    check_entity(db, tasks::Entity, "tasks").await?;
    check_entity(db, state::Entity, "state").await?;
    check_entity(db, sessions::Entity, "sessions").await?;
    check_entity(db, events::Entity, "events").await?;
    Ok(())
}

async fn check_entity<E: EntityTrait>(
    db: &DatabaseConnection,
    entity: E,
    table: &'static str,
) -> Result<(), StoreError> {
    let expected = expected_columns(entity, db.get_database_backend());
    let actual = actual_columns(db, table).await?;
    if expected != actual {
        let missing: Vec<&String> = expected.difference(&actual).collect();
        let extra: Vec<&String> = actual.difference(&expected).collect();
        return Err(StoreError::SchemaDrift {
            column: table.to_owned(),
            reason: format!(
                "entity/table column mismatch — \
                 missing in DB: {missing:?}, extra in DB: {extra:?}"
            ),
        });
    }
    Ok(())
}

/// Columns the entity definition declares, via SeaORM's
/// `Schema::create_table_from_entity`. The returned
/// `TableCreateStatement` carries a `Vec<ColumnDef>` whose names
/// match the ones the DDL will emit.
fn expected_columns<E: EntityTrait>(entity: E, backend: DatabaseBackend) -> BTreeSet<String> {
    let schema = Schema::new(match backend {
        DatabaseBackend::Sqlite => DbBackend::Sqlite,
        DatabaseBackend::Postgres => DbBackend::Postgres,
        DatabaseBackend::MySql => DbBackend::MySql,
    });
    let stmt = schema.create_table_from_entity(entity);
    stmt.get_columns()
        .iter()
        .map(ColumnDef::get_column_name)
        .collect()
}

async fn actual_columns(
    db: &DatabaseConnection,
    table: &str,
) -> Result<BTreeSet<String>, StoreError> {
    let backend = db.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => format!("PRAGMA table_info(\"{table}\")"),
        DatabaseBackend::Postgres => format!(
            "SELECT column_name AS name FROM information_schema.columns \
             WHERE table_name = '{table}' AND table_schema = 'public'"
        ),
        DatabaseBackend::MySql => {
            return Err(StoreError::SchemaDrift {
                column: table.to_owned(),
                reason: "MySQL backend is not supported".to_owned(),
            });
        }
    };

    let rows = db.query_all(Statement::from_string(backend, sql)).await?;

    let mut out = BTreeSet::new();
    for row in rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| StoreError::SchemaDrift {
                column: table.to_owned(),
                reason: format!("could not decode column name: {e}"),
            })?;
        out.insert(name);
    }
    Ok(out)
}

/// Public test hook: run the schema-drift check against a live
/// [`crate::Store`]. Hidden from the public API so production code
/// can't rely on it, but callable from integration tests.
#[doc(hidden)]
pub async fn __check_for_test(store: &crate::Store) -> Result<(), StoreError> {
    check(store.__db_for_test()).await
}
