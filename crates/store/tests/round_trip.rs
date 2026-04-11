//! Write → read → assert round-trips for every entity.
//!
//! Runs entirely against in-memory SQLite. Every scenario from
//! `common::` runs here so the SQLite backend is exercised against
//! byte-identical Rust to the PostgreSQL parity suite.

mod common;

use forgeclaw_store::{Store, generate_id};
use tempfile::NamedTempFile;

async fn fresh_store() -> Store {
    let store = Store::connect_sqlite_memory()
        .await
        .expect("connect sqlite memory");
    store.migrate().await.expect("migrate");
    store
}

#[tokio::test]
async fn messages_round_trip() {
    let store = fresh_store().await;
    common::scenario_messages_round_trip(&store).await;
}

#[tokio::test]
async fn groups_upsert_preserves_created_at() {
    let store = fresh_store().await;
    common::scenario_groups_upsert_preserves_created_at(&store).await;
}

#[tokio::test]
async fn get_group_missing_returns_none() {
    let store = fresh_store().await;
    common::scenario_get_group_missing_returns_none(&store).await;
}

#[tokio::test]
async fn state_round_trip_and_overwrite() {
    let store = fresh_store().await;
    common::scenario_state_round_trip(&store).await;
}

#[tokio::test]
async fn sessions_round_trip_per_group() {
    let store = fresh_store().await;
    common::scenario_sessions_round_trip(&store).await;
}

#[tokio::test]
async fn get_session_missing_returns_none() {
    let store = fresh_store().await;
    common::scenario_get_session_missing_returns_none(&store).await;
}

#[tokio::test]
async fn tasks_due_and_update_after_run() {
    let store = fresh_store().await;
    common::scenario_tasks_due_and_update(&store).await;
}

#[tokio::test]
async fn update_missing_task_is_not_found() {
    let store = fresh_store().await;
    common::scenario_update_missing_task_is_not_found(&store).await;
}

#[tokio::test]
async fn events_filters() {
    let store = fresh_store().await;
    common::scenario_events_filters(&store).await;
}

#[tokio::test]
async fn event_with_no_group_round_trips() {
    let store = fresh_store().await;
    common::scenario_event_with_no_group(&store).await;
}

// -----------------------------------------------------------------
// SQLite-only edge cases that don't fit the shared harness
// -----------------------------------------------------------------

#[tokio::test]
async fn id_helper_is_usable() {
    let _ = generate_id();
}

#[tokio::test]
async fn file_backed_connect_migrate_round_trip() {
    // Prove the regular `connect(url)` path — not just
    // `connect_sqlite_memory` — works against a real SQLite file,
    // including migrate() + a round trip.
    let file = NamedTempFile::new().expect("tempfile");
    let path = file.path().to_str().expect("utf-8 path").to_owned();
    let url = format!("sqlite://{path}");
    // Keep the file alive for the duration of the test.
    drop(file);
    // Re-create the path (NamedTempFile::new created and opened it;
    // dropping closes the handle but the path is still a valid
    // disk location and sqlx will create the file with `?mode=rwc`).
    let url = format!("{url}?mode=rwc");

    let store = Store::connect(&url).await.expect("connect file-backed");
    store.migrate().await.expect("migrate file-backed");

    store
        .set_state("file-k", "file-v")
        .await
        .expect("set_state");
    assert_eq!(
        store.get_state("file-k").await.expect("get_state"),
        Some("file-v".to_owned())
    );

    // Clean up by dropping the store; the tempfile path will be
    // removed when the OS reclaims it.
    drop(store);
    std::fs::remove_file(path).ok();
}
