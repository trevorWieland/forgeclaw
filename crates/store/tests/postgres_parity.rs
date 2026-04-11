//! PostgreSQL parity tests.
//!
//! Gated behind the `postgres-tests` Cargo feature AND a runtime
//! environment variable so CI (with neither) runs zero Postgres work
//! and local developers (with a real database) get full parity checks
//! using identical Rust code on both backends.
//!
//! Usage:
//! ```sh
//! export FORGECLAW_TEST_POSTGRES_URL=postgres://postgres:postgres@localhost/forgeclaw_test
//! cargo nextest run -p forgeclaw-store --features postgres-tests
//! ```
//!
//! If either the feature or the env var is absent, every test returns
//! early with a tracing warning.

#![cfg(feature = "postgres-tests")]

use chrono::{DateTime, TimeZone, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use forgeclaw_store::{Cursor, NewMessage, Store};

const ENV_VAR: &str = "FORGECLAW_TEST_POSTGRES_URL";

fn epoch_plus(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .expect("valid UTC timestamp")
}

async fn maybe_connect() -> Option<Store> {
    let url = std::env::var(ENV_VAR).ok()?;
    let store = Store::connect(&url)
        .await
        .expect("connect postgres test database");
    store.migrate().await.expect("migrate postgres");
    Some(store)
}

#[tokio::test]
async fn postgres_messages_round_trip() {
    let Some(store) = maybe_connect().await else {
        tracing::warn!(env = ENV_VAR, "skipping — Postgres URL not set");
        return;
    };

    let group = GroupId::from("pg-round-trip");
    let channel = ChannelId::from("pg-ch");
    let base = epoch_plus(1_800_000_000);

    for i in 0..3 {
        store
            .store_message(&NewMessage {
                id: format!("pg-msg-{i:03}"),
                group_id: group.clone(),
                channel_id: channel.clone(),
                sender: "alice".into(),
                content: format!("hi {i}"),
                created_at: base + chrono::Duration::seconds(i64::from(i)),
            })
            .await
            .expect("store_message");
    }

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("get_messages_since");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].id, "pg-msg-000");
    assert_eq!(got[2].content, "hi 2");
}

#[tokio::test]
async fn postgres_state_round_trip() {
    let Some(store) = maybe_connect().await else {
        tracing::warn!(env = ENV_VAR, "skipping — Postgres URL not set");
        return;
    };

    store.set_state("pg-key", "pg-v1").await.expect("set pg-v1");
    assert_eq!(
        store.get_state("pg-key").await.expect("get"),
        Some("pg-v1".to_owned())
    );

    store.set_state("pg-key", "pg-v2").await.expect("set pg-v2");
    assert_eq!(
        store.get_state("pg-key").await.expect("get"),
        Some("pg-v2".to_owned())
    );
}
