//! Schema drift regression — the entity definitions and the
//! migration DDL must agree on every column name. Running under
//! SQLite here; the Postgres parity job runs the same check against
//! real Postgres via the `postgres-tests` feature.

use forgeclaw_store::{Store, schema_check};

#[tokio::test]
async fn sqlite_schema_matches_entities() {
    let store = Store::connect_sqlite_memory()
        .await
        .expect("connect sqlite memory");
    store.migrate().await.expect("migrate");

    schema_check::__check_for_test(&store)
        .await
        .expect("schema drift check must pass on a freshly-migrated DB");
}
