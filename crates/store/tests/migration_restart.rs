//! Partial-apply regression for the initial migration.
//!
//! The crate claims migrations are safe to rerun after partial apply
//! (e.g. a crash after some tables/indexes were created but before
//! SeaORM's `_seaql_migrations` row was written). Confirm this by
//! pre-creating a subset of the schema manually on the Store's own
//! connection, then running `Store::migrate()` on the same Store —
//! the call must succeed without erroring on "table/index already
//! exists".

use forgeclaw_store::Store;

/// Preseed a handful of tables and indexes using raw SQL that exactly
/// matches what the real migration will try to create. This simulates
/// a crash mid-migration where those objects survived but the
/// `_seaql_migrations` bookkeeping row did not.
async fn preseed_via_store(store: &Store) {
    let stmts = [
        "CREATE TABLE groups (
            id TEXT PRIMARY KEY NOT NULL,
            display_name TEXT NOT NULL,
            config_json TEXT NOT NULL,
            active INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        "CREATE TABLE messages (
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            group_id TEXT NOT NULL,
            channel_id TEXT NOT NULL,
            sender TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
        "CREATE INDEX messages_group_seq_idx
            ON messages (group_id, seq)",
        "CREATE TABLE events (
            id TEXT PRIMARY KEY NOT NULL,
            kind TEXT NOT NULL,
            group_id TEXT,
            payload TEXT NOT NULL,
            created_at TEXT NOT NULL
        )",
        "CREATE INDEX events_created_idx ON events (created_at)",
    ];
    for sql in stmts {
        store
            .__raw_execute_for_test(sql)
            .await
            .expect("preseed statement");
    }
}

#[tokio::test]
async fn migrate_succeeds_after_partial_schema_preseed() {
    let store = Store::connect_sqlite_memory().await.expect("connect store");
    preseed_via_store(&store).await;

    // Load-bearing: migrate() must succeed even though half the init
    // schema already exists. Without the `.if_not_exists()` adornment
    // on every index, this errors out with "index already exists".
    store
        .migrate()
        .await
        .expect("migrate after partial preseed");

    // Prove the store is actually usable end-to-end after the partial
    // migration was recovered from.
    store
        .set_state("k", "v")
        .await
        .expect("set_state post-migrate");
    assert_eq!(
        store.get_state("k").await.expect("get_state"),
        Some("v".to_owned())
    );
}
