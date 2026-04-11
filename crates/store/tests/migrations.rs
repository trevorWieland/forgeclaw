//! Migration idempotency and schema-smoke tests.

use forgeclaw_store::Store;

#[tokio::test]
async fn migrate_is_idempotent_on_sqlite_memory() {
    let store = Store::connect_sqlite_memory()
        .await
        .expect("connect sqlite memory");
    store.migrate().await.expect("first migration run");
    // Second call must succeed without errors — SeaORM tracks applied
    // migrations internally.
    store.migrate().await.expect("second migration run");
}

#[tokio::test]
async fn migrated_schema_supports_state_roundtrip() {
    let store = Store::connect_sqlite_memory()
        .await
        .expect("connect sqlite memory");
    store.migrate().await.expect("migrate");

    // A trivial round-trip proves the `state` table exists and works.
    store.set_state("boot", "hello").await.expect("set_state");
    let got = store.get_state("boot").await.expect("get_state");
    assert_eq!(got.as_deref(), Some("hello"));
}
