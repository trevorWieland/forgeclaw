//! Forgeclaw persistence layer.
//!
//! `forgeclaw-store` is the authoritative source of truth for messages,
//! groups, tasks, sessions, generic state, and an audit log of events.
//! Every other runtime crate (`router`, `scheduler`, `tanren`,
//! `channels`, `health`) reaches persistence through this crate's
//! public [`Store`] type.
//!
//! # Backends
//!
//! Supports **SQLite** (local development, tests, small deployments)
//! and **PostgreSQL** (production) behind a single API. Backend
//! selection is driven entirely by the connection URL — there is no
//! dialect-specific code in the public surface, and migrations are
//! written once (in Rust, via SeaQuery) for both dialects.
//!
//! # Design anchors
//!
//! - **Compile-time typed queries.** Every query goes through SeaORM's
//!   typed `Entity` + `Column` API. Wrong column names, wrong operand
//!   types, and structurally-invalid filters fail compilation — not at
//!   runtime. A schema-drift test (`tests/schema_drift.rs`) additionally
//!   compares every entity's expected columns against the live
//!   migrated schema on both backends; drift is a CI failure.
//! - **Store-owned monotonic cursor.** [`Cursor`] is a single
//!   store-owned `seq` assigned by the database on insert. No caller
//!   can produce a cursor key smaller than one already delivered, so
//!   backdated inserts cannot be skipped. A separate MVCC visibility
//!   caveat applies on PostgreSQL — see [`Cursor`] for details.
//! - **Bounded reads.** Every query method (`get_messages_since`,
//!   `get_due_tasks`, `list_events`) clamps its caller-supplied limit
//!   against [`MAX_PAGE_SIZE`]. Callers that need more rows page via
//!   repeated calls. Zero / negative limits are validated up front.
//! - **Error classification.** [`StoreError::classify`] maps every
//!   error into [`forgeclaw_core::ErrorClass`] so upstream callers can
//!   retry, circuit-break, or halt without parsing Display strings.
//! - **No secrets in error output.** `StoreError::Database` does
//!   **not** retain the raw `sea_orm::DbErr` — the full error chain is
//!   walked once at the `From<DbErr>` boundary, every layer is
//!   sanitized via a scheme-scanning redactor, and the result is
//!   stored as a plain `String` alongside a [`error::DatabaseCategory`]
//!   classification hint. `Display`, `Debug`, and
//!   `std::error::Error::source()` on a `StoreError` cannot leak
//!   connection URLs under any common structured-logging pattern.
//!
//! # Usage
//!
//! ```no_run
//! use forgeclaw_store::Store;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let store = Store::connect("sqlite://./forgeclaw.db").await?;
//! store.migrate().await?;
//! // ... store.store_message(...).await?;
//! # Ok(()) }
//! ```
//!
//! # Testing
//!
//! SQLite in-memory tests cover the full API surface and run on any
//! machine with no external services. Postgres parity tests are gated
//! behind the `postgres-tests` Cargo feature. When that feature is
//! on, `FORGECLAW_TEST_POSTGRES_URL` **must** be set — the parity
//! tests fail loudly on a missing URL rather than silently passing.
//! The same shared harness (`tests/common/mod.rs`) runs against both
//! backends so there is no drift between SQLite and Postgres coverage.

pub mod error;
pub mod ids;
pub mod types;

/// Hard upper bound on the number of rows any single query may return.
///
/// Every query method in [`Store`] — `get_messages_since`,
/// `get_due_tasks`, `list_events` — clamps its caller-supplied limit
/// against this value before hitting the database. Callers that need
/// more rows page through via cursors / repeated calls. The cap
/// bounds per-call memory and avoids a backlog spike turning a single
/// call into an unbounded allocation.
pub const MAX_PAGE_SIZE: i64 = 10_000;

mod entities;
mod migrations;
mod ops;
#[doc(hidden)]
pub mod schema_check;
mod store;

pub use error::StoreError;
pub use ids::generate_id;
pub use store::Store;
pub use types::event::{EventFilter, NewEvent, StoredEvent};
pub use types::group::RegisteredGroup;
pub use types::message::{Cursor, NewMessage, StoredMessage};
pub use types::task::{
    NewTask, RunOutcome, ScheduleKind, StoredTask, TaskRunResult, TaskStatus, UnknownScheduleKind,
    UnknownTaskStatus,
};
