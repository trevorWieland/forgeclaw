//! Schema migrations — a single Rust source of truth for both backends.
//!
//! Each migration module implements [`MigrationTrait`] via SeaQuery's
//! [`SchemaManager`]. The same Rust code emits the correct DDL for
//! SQLite and PostgreSQL — there are no parallel `.sql` files.
//!
//! ## Immutability
//!
//! Once a migration has been applied to a real database, its module is
//! frozen forever. New schema changes go in a new `mYYYYMMDD_NNNNNN_*.rs`
//! module registered with [`Migrator::migrations`].
//!
//! ## Atomicity
//!
//! PostgreSQL runs each migration inside a transaction. SQLite does not
//! wrap DDL in a transaction, so future multi-step migrations must
//! tolerate partial application on SQLite (typically via idempotent
//! `IF NOT EXISTS` guards, which the initial schema already uses).

use sea_orm_migration::{MigrationTrait, MigratorTrait};

mod m20260411_000001_init;

/// The store-crate migrator, registering every migration in
/// chronological order.
#[derive(Debug)]
pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![Box::new(m20260411_000001_init::Migration)]
    }
}
