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

/// Regression for B1-r2: under concurrent inserts, the advisory lock
/// must guarantee that `seq` allocation order equals commit order.
/// Without it, PostgreSQL's MVCC snapshot can expose rows in a
/// different order from their seq assignment, which lets a reader
/// advance past a seq that is still in-flight and permanently miss
/// the row when it commits.
///
/// The test spawns N concurrent `store_message` calls. After they
/// all complete, the reader walks the cursor from the beginning
/// and asserts:
///   1. it observes exactly N rows (no row is permanently missed)
///   2. their `seq` values are strictly monotonic
///
/// Consecutiveness is NOT asserted because Postgres sequence caches
/// can legitimately produce gaps even without rollbacks. Monotonic +
/// complete is the full correctness contract.
#[tokio::test]
async fn pg_concurrent_inserts_preserve_seq_ordering() {
    use chrono::Utc;
    use forgeclaw_core::id::{ChannelId, GroupId};
    use forgeclaw_store::{Cursor, NewMessage};
    use std::sync::Arc;
    use tokio::task::JoinSet;

    const N: usize = 32;

    let store = Arc::new(fresh_store().await);
    let group = GroupId::new("pg-concurrent").expect("valid group id");
    let channel = ChannelId::new("ch").expect("valid channel id");

    let mut join_set: JoinSet<()> = JoinSet::new();
    for i in 0..N {
        let store = Arc::clone(&store);
        let group = group.clone();
        let channel = channel.clone();
        join_set.spawn(async move {
            let msg = NewMessage {
                id: format!("concurrent-{i:03}"),
                group_id: group,
                channel_id: channel,
                sender: "worker".into(),
                content: format!("msg {i}"),
                created_at: Utc::now(),
            };
            store
                .store_message(&msg)
                .await
                .expect("concurrent store_message");
        });
    }
    while let Some(res) = join_set.join_next().await {
        res.expect("task panicked");
    }

    let rows = store
        .get_messages_since(&group, &Cursor::beginning(), 1_000)
        .await
        .expect("get_messages_since");

    assert_eq!(rows.len(), N, "all inserted rows must be visible");

    for pair in rows.windows(2) {
        assert!(
            pair[0].seq < pair[1].seq,
            "seq order broken: {} then {}",
            pair[0].seq,
            pair[1].seq
        );
    }
}
