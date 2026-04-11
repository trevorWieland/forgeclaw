//! Cursor pagination edge cases for `get_messages_since`.
//!
//! Exercises the seven correctness-sensitive scenarios in the plan:
//! empty table, cursor at beginning, cursor past end, ties on
//! timestamp, exact-match exclusion, LIMIT boundary, and multi-group
//! isolation.

use chrono::{DateTime, TimeZone, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use forgeclaw_store::{Cursor, NewMessage, Store, StoredMessage};

fn epoch_plus(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .expect("valid UTC timestamp")
}

async fn fresh_store() -> Store {
    let store = Store::connect_sqlite_memory()
        .await
        .expect("connect sqlite memory");
    store.migrate().await.expect("migrate");
    store
}

async fn insert(store: &Store, group: &GroupId, id: &str, at: DateTime<Utc>) {
    store
        .store_message(&NewMessage {
            id: id.to_owned(),
            group_id: group.clone(),
            channel_id: ChannelId::from("ch"),
            sender: "u".into(),
            content: "c".into(),
            created_at: at,
        })
        .await
        .expect("store_message");
}

fn cursor_from(msg: &StoredMessage) -> Cursor {
    Cursor {
        timestamp: msg.created_at,
        message_id: msg.id.clone(),
    }
}

#[tokio::test]
async fn empty_table_returns_empty_vec() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert!(got.is_empty());
}

#[tokio::test]
async fn single_message_with_beginning_cursor_returns_it() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    insert(&store, &group, "m-1", epoch_plus(1000)).await;

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m-1");
}

#[tokio::test]
async fn cursor_past_last_message_returns_empty() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    insert(&store, &group, "m-1", epoch_plus(1000)).await;

    let cursor = Cursor {
        timestamp: epoch_plus(2000),
        message_id: String::new(),
    };
    let got = store
        .get_messages_since(&group, &cursor, 100)
        .await
        .expect("query");
    assert!(got.is_empty());
}

#[tokio::test]
async fn identical_timestamps_ordered_deterministically_by_id() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    let at = epoch_plus(3000);

    insert(&store, &group, "m-c", at).await;
    insert(&store, &group, "m-a", at).await;
    insert(&store, &group, "m-b", at).await;

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert_eq!(got.len(), 3);
    let ids: Vec<&str> = got.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["m-a", "m-b", "m-c"]);
}

#[tokio::test]
async fn cursor_excludes_message_at_exact_composite_key() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    let at = epoch_plus(4000);
    insert(&store, &group, "m-1", at).await;
    insert(&store, &group, "m-2", at).await;
    insert(&store, &group, "m-3", at).await;

    let all = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query all");
    assert_eq!(all.len(), 3);

    // Cursor positioned at m-2 should return only m-3 (m-1, m-2 excluded).
    let after_m2 = store
        .get_messages_since(&group, &cursor_from(&all[1]), 100)
        .await
        .expect("query after m-2");
    assert_eq!(after_m2.len(), 1);
    assert_eq!(after_m2[0].id, "m-3");
}

#[tokio::test]
async fn limit_boundary_paginates_correctly() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    for i in 0..5 {
        insert(&store, &group, &format!("m-{i}"), epoch_plus(5000 + i)).await;
    }

    // First page: limit 3 returns the first 3 messages.
    let page1 = store
        .get_messages_since(&group, &Cursor::beginning(), 3)
        .await
        .expect("page 1");
    assert_eq!(page1.len(), 3);
    assert_eq!(page1[0].id, "m-0");
    assert_eq!(page1[2].id, "m-2");

    // Second page: cursor = last of page1, limit 3 returns the rest (2).
    let page2 = store
        .get_messages_since(&group, &cursor_from(&page1[2]), 3)
        .await
        .expect("page 2");
    assert_eq!(page2.len(), 2);
    assert_eq!(page2[0].id, "m-3");
    assert_eq!(page2[1].id, "m-4");

    // Third page: cursor = last of page2, returns empty.
    let page3 = store
        .get_messages_since(&group, &cursor_from(&page2[1]), 3)
        .await
        .expect("page 3");
    assert!(page3.is_empty());
}

#[tokio::test]
async fn multi_group_isolation() {
    let store = fresh_store().await;
    let a = GroupId::from("a");
    let b = GroupId::from("b");

    insert(&store, &a, "a-1", epoch_plus(6000)).await;
    insert(&store, &a, "a-2", epoch_plus(6001)).await;
    insert(&store, &b, "b-1", epoch_plus(6000)).await;

    let got_a = store
        .get_messages_since(&a, &Cursor::beginning(), 100)
        .await
        .expect("group a");
    assert_eq!(got_a.len(), 2);
    assert!(got_a.iter().all(|m| m.group_id.as_ref() == "a"));

    let got_b = store
        .get_messages_since(&b, &Cursor::beginning(), 100)
        .await
        .expect("group b");
    assert_eq!(got_b.len(), 1);
    assert_eq!(got_b[0].id, "b-1");
}

#[tokio::test]
async fn zero_limit_returns_empty() {
    let store = fresh_store().await;
    let group = GroupId::from("g");
    insert(&store, &group, "m-1", epoch_plus(7000)).await;

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 0)
        .await
        .expect("zero limit");
    assert!(got.is_empty());
}
