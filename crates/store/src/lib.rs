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
//!   runtime.
//! - **Composite-key cursor pagination.** [`Cursor`] is
//!   `(timestamp, message_id)` and strictly exclusive, so concurrent
//!   writes at the same millisecond are neither skipped nor duplicated.
//! - **Error classification.** [`StoreError::classify`] maps every
//!   error into [`forgeclaw_core::ErrorClass`] so upstream callers can
//!   retry, circuit-break, or halt without parsing Display strings.
//! - **No secrets in error output.** Connection URLs are stripped from
//!   error messages via [`StoreError::from`] before Display ever runs.
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
//! behind the `postgres-tests` Cargo feature and read
//! `FORGECLAW_TEST_POSTGRES_URL` at runtime; they are skipped cleanly
//! when either is absent.

pub mod error;
pub mod ids;
pub mod types;

mod entities;
mod migrations;
mod ops;
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
