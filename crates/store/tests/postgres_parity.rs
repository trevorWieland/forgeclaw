//! PostgreSQL parity tests.
//!
//! Gated behind the `postgres-tests` Cargo feature. When the feature
//! is on, `FORGECLAW_TEST_POSTGRES_URL` **must** be set or every test
//! panics at the point of use — a CI developer cannot accidentally
//! green-light Postgres parity by forgetting to wire up a database.
//!
//! Usage:
//! ```sh
//! export FORGECLAW_TEST_POSTGRES_URL=postgres://postgres:postgres@localhost/forgeclaw_test
//! cargo nextest run -p forgeclaw-store --features postgres-tests
//! ```
//!
//! Every scenario from the shared harness runs here, so the Postgres
//! and SQLite suites exercise byte-identical Rust code.

#![cfg(feature = "postgres-tests")]

mod common;

use forgeclaw_store::Store;

const ENV_VAR: &str = "FORGECLAW_TEST_POSTGRES_URL";

/// Obtain the Postgres URL or fail loudly via `.expect()` — this is
/// not a graceful skip. Enabling the feature means "run this suite",
/// and no configuration is a configuration error, not a reason to
/// report green.
fn require_url() -> String {
    std::env::var(ENV_VAR).expect(
        "postgres-tests feature is on but FORGECLAW_TEST_POSTGRES_URL is unset \
         — set the env var or disable the feature",
    )
}

async fn fresh_store() -> Store {
    let url = require_url();
    let store = Store::connect(&url).await.expect("connect postgres");
    store.migrate().await.expect("migrate postgres");

    // Wipe every table so consecutive tests don't see each other's
    // rows. Postgres state persists across test runs, so truncation
    // is required for determinism.
    for table in ["messages", "groups", "tasks", "state", "sessions", "events"] {
        store
            .__raw_execute_for_test(&format!("TRUNCATE TABLE {table} RESTART IDENTITY CASCADE"))
            .await
            .expect("truncate");
    }

    store
}

#[tokio::test]
async fn pg_messages_round_trip() {
    let store = fresh_store().await;
    common::scenario_messages_round_trip(&store).await;
}

#[tokio::test]
async fn pg_groups_upsert_preserves_created_at() {
    let store = fresh_store().await;
    common::scenario_groups_upsert_preserves_created_at(&store).await;
}

#[tokio::test]
async fn pg_get_group_missing_returns_none() {
    let store = fresh_store().await;
    common::scenario_get_group_missing_returns_none(&store).await;
}

#[tokio::test]
async fn pg_state_round_trip() {
    let store = fresh_store().await;
    common::scenario_state_round_trip(&store).await;
}

#[tokio::test]
async fn pg_sessions_round_trip() {
    let store = fresh_store().await;
    common::scenario_sessions_round_trip(&store).await;
}

#[tokio::test]
async fn pg_get_session_missing_returns_none() {
    let store = fresh_store().await;
    common::scenario_get_session_missing_returns_none(&store).await;
}

#[tokio::test]
async fn pg_tasks_due_and_update() {
    let store = fresh_store().await;
    common::scenario_tasks_due_and_update(&store).await;
}

#[tokio::test]
async fn pg_update_missing_task_is_not_found() {
    let store = fresh_store().await;
    common::scenario_update_missing_task_is_not_found(&store).await;
}

#[tokio::test]
async fn pg_events_filters() {
    let store = fresh_store().await;
    common::scenario_events_filters(&store).await;
}

#[tokio::test]
async fn pg_event_with_no_group_round_trips() {
    let store = fresh_store().await;
    common::scenario_event_with_no_group(&store).await;
}

#[tokio::test]
async fn pg_schema_matches_entities() {
    let store = fresh_store().await;
    forgeclaw_store::schema_check::__check_for_test(&store)
        .await
        .expect("schema drift check must pass on real Postgres");
}
