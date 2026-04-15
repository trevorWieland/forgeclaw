//! Cursor pagination edge cases for `get_messages_since`.
//!
//! The cursor is a single store-owned `seq` ordinal, so the edge
//! cases are: empty table, cursor at beginning, cursor past end,
//! exact-match exclusion, LIMIT boundary with resume, multi-group
//! isolation, and the invariant that a row with a backdated
//! `created_at` cannot be skipped — that is the whole point of moving
//! the cursor off caller-supplied fields.

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
            channel_id: ChannelId::new("ch").expect("valid channel id"),
            sender: "u".into(),
            content: "c".into(),
            created_at: at,
        })
        .await
        .expect("store_message");
}

fn cursor_from(msg: &StoredMessage) -> Cursor {
    Cursor::after(msg.seq)
}

#[tokio::test]
async fn empty_table_returns_empty_vec() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert!(got.is_empty());
}

#[tokio::test]
async fn single_message_with_beginning_cursor_returns_it() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    insert(&store, &group, "m-1", epoch_plus(1000)).await;

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m-1");
    assert!(got[0].seq >= 1, "first seq should be >= 1");
}

#[tokio::test]
async fn cursor_past_last_message_returns_empty() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    insert(&store, &group, "m-1", epoch_plus(1000)).await;

    let cursor = Cursor::after(i64::MAX - 1);
    let got = store
        .get_messages_since(&group, &cursor, 100)
        .await
        .expect("query");
    assert!(got.is_empty());
}

#[tokio::test]
async fn ordering_is_by_insert_seq_not_created_at() {
    // Insert in a funky order with backdated timestamps — the cursor
    // must return them in insert order (the order the DB assigned
    // `seq`), not in `created_at` order. This is the behavioral core
    // of the B1 fix: caller-controlled `created_at` cannot affect
    // pagination order.
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");

    insert(&store, &group, "m-c", epoch_plus(3000)).await;
    insert(&store, &group, "m-a", epoch_plus(1000)).await; // backdated
    insert(&store, &group, "m-b", epoch_plus(2000)).await; // backdated

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query");
    assert_eq!(got.len(), 3);
    let ids: Vec<&str> = got.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["m-c", "m-a", "m-b"],
        "pagination must return rows in insert (seq) order, not created_at order"
    );
    // And the seqs must be strictly monotonic.
    assert!(got[0].seq < got[1].seq);
    assert!(got[1].seq < got[2].seq);
}

#[tokio::test]
async fn backdated_insert_is_not_skipped_when_cursor_is_advanced() {
    // Regression for the Lane 0.3 audit blocker B1. A reader
    // checkpoints after reading m-1. A second writer then inserts
    // m-2 with a `created_at` far in the past. With the old
    // (created_at, id) cursor, the reader would silently skip m-2
    // because its composite key sorted below the checkpoint. With
    // seq-based cursor, m-2 gets a fresh seq after the checkpoint
    // and is visible to the reader.
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");

    insert(&store, &group, "m-1", epoch_plus(5_000_000_000)).await;
    let page1 = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("page 1");
    assert_eq!(page1.len(), 1);
    let checkpoint = cursor_from(&page1[0]);

    // Concurrent writer: insert a message with a much earlier
    // created_at.
    insert(&store, &group, "m-2", epoch_plus(1)).await;

    let page2 = store
        .get_messages_since(&group, &checkpoint, 100)
        .await
        .expect("page 2");
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].id, "m-2", "backdated insert must not be skipped");
    assert!(
        page2[0].seq > page1[0].seq,
        "backdated row must still get a seq greater than earlier rows"
    );
}

#[tokio::test]
async fn cursor_excludes_message_at_exact_seq() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    for i in 0..3 {
        insert(&store, &group, &format!("m-{i}"), epoch_plus(4000 + i)).await;
    }

    let all = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("query all");
    assert_eq!(all.len(), 3);

    // Cursor positioned at m-1's seq returns only m-2.
    let after_m1 = store
        .get_messages_since(&group, &cursor_from(&all[1]), 100)
        .await
        .expect("query after m-1");
    assert_eq!(after_m1.len(), 1);
    assert_eq!(after_m1[0].id, "m-2");
}

#[tokio::test]
async fn limit_boundary_paginates_correctly() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
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
    let a = GroupId::new("a").expect("valid group id");
    let b = GroupId::new("b").expect("valid group id");

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
async fn negative_limit_is_rejected() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    insert(&store, &group, "m-1", epoch_plus(7500)).await;

    let err = store
        .get_messages_since(&group, &Cursor::beginning(), -1)
        .await
        .expect_err("negative limit must error");
    assert!(
        matches!(err, forgeclaw_store::StoreError::InvalidLimit { .. }),
        "expected InvalidLimit, got {err:?}"
    );
}

#[tokio::test]
async fn huge_limit_is_clamped_to_max_page_size() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    for i in 0..5_i64 {
        insert(&store, &group, &format!("m-{i}"), epoch_plus(8000 + i)).await;
    }
    let got = store
        .get_messages_since(&group, &Cursor::beginning(), i64::MAX)
        .await
        .expect("huge limit must succeed (clamped)");
    assert_eq!(got.len(), 5);
    let rows = i64::try_from(got.len()).expect("row count fits in i64");
    assert!(rows <= forgeclaw_store::MAX_PAGE_SIZE);
}

#[tokio::test]
async fn zero_limit_returns_empty() {
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    insert(&store, &group, "m-1", epoch_plus(7000)).await;

    let got = store
        .get_messages_since(&group, &Cursor::beginning(), 0)
        .await
        .expect("zero limit");
    assert!(got.is_empty());
}

#[tokio::test]
async fn duplicate_message_id_is_integrity_error() {
    // `id` is now a UNIQUE secondary key. Inserting two messages
    // with the same id must fail loudly so callers can dedupe
    // upstream. Classification goes to `Fatal` via
    // `DatabaseCategory::Integrity` / `Other`.
    let store = fresh_store().await;
    let group = GroupId::new("g").expect("valid group id");
    insert(&store, &group, "m-dup", epoch_plus(9000)).await;

    let err = store
        .store_message(&NewMessage {
            id: "m-dup".to_owned(),
            group_id: group,
            channel_id: ChannelId::new("ch").expect("valid channel id"),
            sender: "u".into(),
            content: "c".into(),
            created_at: epoch_plus(9001),
        })
        .await
        .expect_err("duplicate id must error");
    assert!(
        matches!(err, forgeclaw_store::StoreError::Database { .. }),
        "expected Database, got {err:?}"
    );
}
