//! Write → read → assert round-trips for every entity.
//!
//! All tests run against an in-memory SQLite database so they need no
//! external services. The same suite is exercised against PostgreSQL
//! under the `postgres-tests` feature.

use chrono::{DateTime, TimeZone, Utc};
use forgeclaw_core::id::{ChannelId, GroupId, TaskId};
use forgeclaw_store::{
    Cursor, EventFilter, NewEvent, NewMessage, NewTask, RegisteredGroup, RunOutcome, ScheduleKind,
    Store, TaskRunResult, TaskStatus, generate_id,
};

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

#[tokio::test]
async fn messages_round_trip() {
    let store = fresh_store().await;
    let group = GroupId::from("group-a");
    let channel = ChannelId::from("discord");

    let base = epoch_plus(1_000_000_000);
    for i in 0..3 {
        store
            .store_message(&NewMessage {
                id: format!("msg-{i:03}"),
                group_id: group.clone(),
                channel_id: channel.clone(),
                sender: "alice".into(),
                content: format!("hello {i}"),
                created_at: base + chrono::Duration::seconds(i64::from(i)),
            })
            .await
            .expect("store_message");
    }

    let messages = store
        .get_messages_since(&group, &Cursor::beginning(), 100)
        .await
        .expect("get_messages_since");

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].id, "msg-000");
    assert_eq!(messages[1].id, "msg-001");
    assert_eq!(messages[2].id, "msg-002");
    assert_eq!(messages[0].content, "hello 0");
    assert_eq!(messages[0].group_id.as_ref(), "group-a");
    assert_eq!(messages[0].channel_id.as_ref(), "discord");
}

#[tokio::test]
async fn groups_upsert_preserves_created_at() {
    let store = fresh_store().await;
    let id = GroupId::from("group-b");

    let created = epoch_plus(1_600_000_000);
    store
        .upsert_group(&RegisteredGroup {
            id: id.clone(),
            display_name: "Alpha".into(),
            config_json: r#"{"ver":1}"#.into(),
            active: true,
            created_at: created,
            updated_at: created,
        })
        .await
        .expect("first upsert");

    let later = epoch_plus(1_700_000_000);
    store
        .upsert_group(&RegisteredGroup {
            id: id.clone(),
            display_name: "Beta".into(),
            config_json: r#"{"ver":2}"#.into(),
            active: false,
            created_at: later, // should be ignored; created_at preserved
            updated_at: later,
        })
        .await
        .expect("second upsert");

    let got = store
        .get_group(&id)
        .await
        .expect("get_group")
        .expect("group exists");
    assert_eq!(got.display_name, "Beta");
    assert!(!got.active);
    assert_eq!(got.config_json, r#"{"ver":2}"#);
    assert_eq!(got.created_at, created, "created_at must be preserved");
    assert_eq!(got.updated_at, later);
}

#[tokio::test]
async fn state_round_trip_and_overwrite() {
    let store = fresh_store().await;
    assert_eq!(store.get_state("k").await.expect("get"), None);

    store.set_state("k", "v1").await.expect("set v1");
    assert_eq!(
        store.get_state("k").await.expect("get"),
        Some("v1".to_owned())
    );

    store.set_state("k", "v2").await.expect("set v2");
    assert_eq!(
        store.get_state("k").await.expect("get"),
        Some("v2".to_owned())
    );
}

#[tokio::test]
async fn sessions_round_trip_per_group() {
    let store = fresh_store().await;
    let a = GroupId::from("a");
    let b = GroupId::from("b");

    store.set_session(&a, "sess-1").await.expect("set a");
    store.set_session(&b, "sess-2").await.expect("set b");

    assert_eq!(
        store.get_session(&a).await.expect("get a"),
        Some("sess-1".to_owned())
    );
    assert_eq!(
        store.get_session(&b).await.expect("get b"),
        Some("sess-2".to_owned())
    );

    store.set_session(&a, "sess-3").await.expect("overwrite");
    assert_eq!(
        store.get_session(&a).await.expect("get a again"),
        Some("sess-3".to_owned())
    );
}

#[tokio::test]
async fn tasks_due_and_update_after_run() {
    let store = fresh_store().await;
    let group = GroupId::from("group-t");

    let due = epoch_plus(1_500_000_000);
    let not_yet = epoch_plus(2_000_000_000);

    let due_id = store
        .create_task(&NewTask {
            group_id: group.clone(),
            prompt: "run this".into(),
            schedule_kind: ScheduleKind::Once,
            schedule_value: "2026-04-11T00:00:00Z".into(),
            status: TaskStatus::Active,
            next_run: Some(due),
        })
        .await
        .expect("create due task");

    let _future_id: TaskId = store
        .create_task(&NewTask {
            group_id: group.clone(),
            prompt: "later".into(),
            schedule_kind: ScheduleKind::Cron,
            schedule_value: "0 0 * * *".into(),
            status: TaskStatus::Active,
            next_run: Some(not_yet),
        })
        .await
        .expect("create future task");

    // now = due + 1s — only the first task should be returned.
    let now = due + chrono::Duration::seconds(1);
    let tasks = store.get_due_tasks(now).await.expect("get_due_tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, due_id);
    assert_eq!(tasks[0].prompt, "run this");

    // Record a run, marking the task completed.
    let result = TaskRunResult {
        ran_at: now,
        outcome: RunOutcome::Success {
            detail: Some("ok".into()),
        },
        next_run: None,
        status: TaskStatus::Completed,
    };
    store
        .update_task_after_run(&due_id, &result)
        .await
        .expect("update_task_after_run");

    // Completed tasks should no longer be "due".
    let tasks = store.get_due_tasks(now).await.expect("get_due_tasks");
    assert_eq!(tasks.len(), 0);
}

#[tokio::test]
async fn events_record_and_list_with_filters() {
    let store = fresh_store().await;
    let group = GroupId::from("group-e");

    let base = epoch_plus(1_600_000_000);
    for i in 0..5 {
        store
            .record_event(&NewEvent {
                kind: if i % 2 == 0 {
                    "task.completed".into()
                } else {
                    "message.received".into()
                },
                group_id: Some(group.clone()),
                payload: format!(r#"{{"i":{i}}}"#),
                created_at: base + chrono::Duration::seconds(i64::from(i)),
            })
            .await
            .expect("record_event");
    }

    // No filter: all 5, newest first.
    let all = store
        .list_events(&EventFilter::default(), 100)
        .await
        .expect("list all");
    assert_eq!(all.len(), 5);
    assert!(all[0].created_at >= all[4].created_at);

    // Filter by kind.
    let completed = store
        .list_events(
            &EventFilter {
                kind: Some("task.completed".into()),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list filtered");
    assert_eq!(completed.len(), 3);
    for ev in &completed {
        assert_eq!(ev.kind, "task.completed");
    }

    // Filter by group (matches all).
    let by_group = store
        .list_events(
            &EventFilter {
                group_id: Some(group.clone()),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list by group");
    assert_eq!(by_group.len(), 5);

    // Filter by since (strict >). `base + 2s` excludes indices 0, 1, 2
    // and keeps 3, 4.
    let since_2 = store
        .list_events(
            &EventFilter {
                since: Some(base + chrono::Duration::seconds(2)),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list since");
    assert_eq!(since_2.len(), 2);

    // Limit.
    let limited = store
        .list_events(&EventFilter::default(), 2)
        .await
        .expect("limit");
    assert_eq!(limited.len(), 2);

    // Use the ULID helper too, for coverage.
    let _ = generate_id();
}
