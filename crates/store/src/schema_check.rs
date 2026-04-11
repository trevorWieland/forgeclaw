//! Test-only schema-drift check.
//!
//! Compares the full schema shape each SeaORM entity expects against
//! the shape the live database actually has after migrations run.
//! Each column is compared on `(name, not_null, is_primary_key)` so
//! the check fires on:
//!
//! - a column added or removed on one side
//! - a column that flipped from `NOT NULL` to nullable (or back)
//! - a column that moved in or out of the primary key
//!
//! Type comparison is intentionally out of scope because SeaQuery
//! emits different canonical type strings on SQLite vs PostgreSQL
//! (`text` vs `text` happens to match, but `timestamp with time zone`
//! and `bigint` normalize differently), and the marginal drift cases
//! it would catch are already covered by the runtime decode errors
//! sea-orm raises when a field's declared type and the stored value
//! disagree.
//!
//! This is not a compile-time check, but it is a hard CI failure —
//! every build runs it against SQLite, and the Postgres parity job
//! runs it against real Postgres via the same harness.

use std::collections::{BTreeSet, HashSet};

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DbBackend, EntityTrait, Iden, Iterable,
    PrimaryKeyToColumn, Schema, Statement,
    sea_query::{ColumnDef, ColumnSpec},
};

use crate::entities::{events, groups, messages, sessions, state, tasks};
use crate::error::StoreError;

/// One column's observable shape for drift purposes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ColumnShape {
    name: String,
    not_null: bool,
    is_primary_key: bool,
}

/// Run the drift check against `db`. Returns `Ok(())` if every
/// entity's expected shape matches the live table's shape; otherwise
/// [`StoreError::SchemaDrift`] with a diagnostic describing the
/// first mismatched table.
pub async fn check(db: &DatabaseConnection) -> Result<(), StoreError> {
    check_entity(db, messages::Entity, "messages").await?;
    check_entity(db, groups::Entity, "groups").await?;
    check_entity(db, tasks::Entity, "tasks").await?;
    check_entity(db, state::Entity, "state").await?;
    check_entity(db, sessions::Entity, "sessions").await?;
    check_entity(db, events::Entity, "events").await?;
    Ok(())
}

async fn check_entity<E>(
    db: &DatabaseConnection,
    entity: E,
    table: &'static str,
) -> Result<(), StoreError>
where
    E: EntityTrait,
{
    let expected = expected_shape::<E>(entity, db.get_database_backend());
    let actual = actual_shape(db, table).await?;
    if expected != actual {
        let missing: Vec<&ColumnShape> = expected.difference(&actual).collect();
        let extra: Vec<&ColumnShape> = actual.difference(&expected).collect();
        return Err(StoreError::SchemaDrift {
            table: Some(table.to_owned()),
            column: None,
            reason: format!(
                "entity/table shape mismatch — \
                 missing in DB: {missing:?}, extra in DB: {extra:?}"
            ),
        });
    }
    Ok(())
}

/// Shape the entity definition declares, derived from SeaORM's
/// `Schema::create_table_from_entity` plus the entity's `PrimaryKey`
/// associated type.
fn expected_shape<E>(entity: E, backend: DatabaseBackend) -> BTreeSet<ColumnShape>
where
    E: EntityTrait,
{
    let schema = Schema::new(match backend {
        DatabaseBackend::Sqlite => DbBackend::Sqlite,
        DatabaseBackend::Postgres => DbBackend::Postgres,
        DatabaseBackend::MySql => DbBackend::MySql,
    });
    let stmt = schema.create_table_from_entity(entity);

    let pk_names: HashSet<String> = <<E as EntityTrait>::PrimaryKey as Iterable>::iter()
        .map(|pk| Iden::to_string(&pk.into_column()))
        .collect();

    stmt.get_columns()
        .iter()
        .map(|c| {
            let name = ColumnDef::get_column_name(c);
            ColumnShape {
                not_null: column_def_is_not_null(c),
                is_primary_key: pk_names.contains(&name),
                name,
            }
        })
        .collect()
}

/// Interpret a `ColumnDef`'s spec list as "nullable vs NOT NULL".
///
/// SeaORM's `create_table_from_entity` path pushes an explicit
/// `ColumnSpec::NotNull` for every non-`Option` field and emits
/// nothing for `Option<T>` fields. So "has `NotNull` spec" means
/// NOT NULL, and "no `NotNull` spec" means nullable.
fn column_def_is_not_null(col: &ColumnDef) -> bool {
    col.get_column_spec()
        .iter()
        .any(|s| matches!(s, ColumnSpec::NotNull))
}

async fn actual_shape(
    db: &DatabaseConnection,
    table: &str,
) -> Result<BTreeSet<ColumnShape>, StoreError> {
    match db.get_database_backend() {
        DatabaseBackend::Sqlite => actual_shape_sqlite(db, table).await,
        DatabaseBackend::Postgres => actual_shape_postgres(db, table).await,
        DatabaseBackend::MySql => Err(StoreError::SchemaDrift {
            table: Some(table.to_owned()),
            column: None,
            reason: "MySQL backend is not supported".to_owned(),
        }),
    }
}

async fn actual_shape_sqlite(
    db: &DatabaseConnection,
    table: &str,
) -> Result<BTreeSet<ColumnShape>, StoreError> {
    let sql = format!("PRAGMA table_info(\"{table}\")");
    let rows = db
        .query_all(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await?;

    let mut out = BTreeSet::new();
    for row in rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| schema_drift_err(table, &format!("name: {e}")))?;
        // PRAGMA returns `notnull` and `pk` as integers (0 / 1 / PK position).
        let not_null: i32 = row
            .try_get("", "notnull")
            .map_err(|e| schema_drift_err(table, &format!("notnull: {e}")))?;
        let pk: i32 = row
            .try_get("", "pk")
            .map_err(|e| schema_drift_err(table, &format!("pk: {e}")))?;
        out.insert(ColumnShape {
            name,
            not_null: not_null != 0,
            is_primary_key: pk != 0,
        });
    }
    Ok(out)
}

async fn actual_shape_postgres(
    db: &DatabaseConnection,
    table: &str,
) -> Result<BTreeSet<ColumnShape>, StoreError> {
    // First query: every column's (name, nullability).
    let columns_sql = format!(
        "SELECT column_name AS name, is_nullable AS is_nullable \
         FROM information_schema.columns \
         WHERE table_name = '{table}' AND table_schema = 'public'"
    );
    let column_rows = db
        .query_all(Statement::from_string(
            DatabaseBackend::Postgres,
            columns_sql,
        ))
        .await?;

    // Second query: the set of primary-key column names.
    let pk_sql = format!(
        "SELECT kcu.column_name AS name \
         FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
           ON tc.constraint_name = kcu.constraint_name \
          AND tc.table_schema = kcu.table_schema \
         WHERE tc.constraint_type = 'PRIMARY KEY' \
           AND tc.table_name = '{table}' \
           AND tc.table_schema = 'public'"
    );
    let pk_rows = db
        .query_all(Statement::from_string(DatabaseBackend::Postgres, pk_sql))
        .await?;

    let mut pk_names: HashSet<String> = HashSet::new();
    for row in pk_rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| schema_drift_err(table, &format!("pk name: {e}")))?;
        pk_names.insert(name);
    }

    let mut out = BTreeSet::new();
    for row in column_rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| schema_drift_err(table, &format!("name: {e}")))?;
        let is_nullable: String = row
            .try_get("", "is_nullable")
            .map_err(|e| schema_drift_err(table, &format!("is_nullable: {e}")))?;
        out.insert(ColumnShape {
            is_primary_key: pk_names.contains(&name),
            not_null: is_nullable == "NO",
            name,
        });
    }
    Ok(out)
}

fn schema_drift_err(table: &str, reason: &str) -> StoreError {
    StoreError::SchemaDrift {
        table: Some(table.to_owned()),
        column: None,
        reason: reason.to_owned(),
    }
}

/// Public test hook: run the schema-drift check against a live
/// [`crate::Store`]. Hidden from the public API so production code
/// can't rely on it, but callable from integration tests.
#[doc(hidden)]
pub async fn __check_for_test(store: &crate::Store) -> Result<(), StoreError> {
    check(store.__db_for_test()).await
}
