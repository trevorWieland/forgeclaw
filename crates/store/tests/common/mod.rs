//! Shared integration-test harness.
//!
//! Every scenario in this module takes a `&Store` and runs identically
//! against SQLite (via the `round_trip.rs` suite) and PostgreSQL (via
//! `postgres_parity.rs`). Keeping the scenarios here guarantees the
//! two backends are exercised against byte-identical Rust code, so
//! there is no way a SQLite-only test suite can report green while
//! the Postgres backend silently diverges.
//!
//! Tests that cover edges specific to one backend (in-memory pool,
//! migration restart) live in their own test files and do not use
//! this harness.
//!
//! Every scenario in this file must be referenced by *every* importer
//! so that `dead_code` does not fire in any test binary. The two
//! current importers — `round_trip.rs` and `postgres_parity.rs` —
//! both call every scenario.

use chrono::{DateTime, TimeZone, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use forgeclaw_store::{
    Cursor, EventFilter, NewEvent, NewMessage, NewTask, RegisteredGroup, RunOutcome, ScheduleKind,
    Store, StoreError, TaskRunResult, TaskStatus,
};

pub(crate) fn epoch_plus(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .expect("valid UTC timestamp")
}

// ---------------------------------------------------------------
// Scenarios that run against any connected+migrated Store
// ---------------------------------------------------------------

pub(crate) async fn scenario_messages_round_trip(store: &Store) {
    let group = GroupId::new("group-a").expect("valid group id");
    let channel = ChannelId::new("discord").expect("valid channel id");
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
    assert_eq!(messages[2].id, "msg-002");
    assert!(messages[0].seq >= 1);
    assert!(messages[0].seq < messages[1].seq);
    assert!(messages[1].seq < messages[2].seq);
}

pub(crate) async fn scenario_groups_upsert_preserves_created_at(store: &Store) {
    let id = GroupId::new("group-b").expect("valid group id");
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
            created_at: later,
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

pub(crate) async fn scenario_get_group_missing_returns_none(store: &Store) {
    let id = GroupId::new("nope-no-such-group").expect("valid group id");
    let got = store.get_group(&id).await.expect("get_group");
    assert!(got.is_none());
}

pub(crate) async fn scenario_state_round_trip(store: &Store) {
    assert_eq!(store.get_state("common-k").await.expect("get"), None);
    store.set_state("common-k", "v1").await.expect("set v1");
    assert_eq!(
        store.get_state("common-k").await.expect("get"),
        Some("v1".to_owned())
    );
    store.set_state("common-k", "v2").await.expect("set v2");
    assert_eq!(
        store.get_state("common-k").await.expect("get"),
        Some("v2".to_owned())
    );
}

pub(crate) async fn scenario_sessions_round_trip(store: &Store) {
    let a = GroupId::new("sess-a").expect("valid group id");
    let b = GroupId::new("sess-b").expect("valid group id");
    assert_eq!(store.get_session(&a).await.expect("get a miss"), None);

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

pub(crate) async fn scenario_get_session_missing_returns_none(store: &Store) {
    let id = GroupId::new("nope-no-session").expect("valid group id");
    assert_eq!(store.get_session(&id).await.expect("get_session"), None);
}

pub(crate) async fn scenario_tasks_due_and_update(store: &Store) {
    let group = GroupId::new("group-t").expect("valid group id");
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

    let _future_id = store
        .create_task(&NewTask {
            group_id: group,
            prompt: "later".into(),
            schedule_kind: ScheduleKind::Cron,
            schedule_value: "0 0 * * *".into(),
            status: TaskStatus::Active,
            next_run: Some(not_yet),
        })
        .await
        .expect("create future task");

    let now = due + chrono::Duration::seconds(1);
    let tasks = store.get_due_tasks(now, 100).await.expect("get_due_tasks");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, due_id);

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

    let tasks = store.get_due_tasks(now, 100).await.expect("get_due_tasks");
    assert_eq!(tasks.len(), 0);
}

pub(crate) async fn scenario_update_missing_task_is_not_found(store: &Store) {
    use forgeclaw_core::id::TaskId;
    let fake = TaskId::new("no-such-task-id").expect("valid task id");
    let result = TaskRunResult {
        ran_at: epoch_plus(1_900_000_000),
        outcome: RunOutcome::Success { detail: None },
        next_run: None,
        status: TaskStatus::Completed,
    };
    let err = store
        .update_task_after_run(&fake, &result)
        .await
        .expect_err("must fail on missing task");
    assert!(
        matches!(err, StoreError::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

pub(crate) async fn scenario_events_filters(store: &Store) {
    let group = GroupId::new("group-e").expect("valid group id");
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

    // No filter
    let all = store
        .list_events(&EventFilter::default(), 100)
        .await
        .expect("list all");
    assert_eq!(all.len(), 5);

    // Filter by kind
    let completed = store
        .list_events(
            &EventFilter {
                kind: Some("task.completed".into()),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list kind");
    assert_eq!(completed.len(), 3);

    // Filter by group
    let by_group = store
        .list_events(
            &EventFilter {
                group_id: Some(group.clone()),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list group");
    assert_eq!(by_group.len(), 5);

    // Filter by since (strict >)
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

    // Limit
    let limited = store
        .list_events(&EventFilter::default(), 2)
        .await
        .expect("list limit");
    assert_eq!(limited.len(), 2);

    // A filter that matches nothing
    let nothing = store
        .list_events(
            &EventFilter {
                kind: Some("no.such.kind".into()),
                ..Default::default()
            },
            100,
        )
        .await
        .expect("list miss");
    assert!(nothing.is_empty());
}

pub(crate) async fn scenario_event_with_no_group(store: &Store) {
    let ts = epoch_plus(1_800_000_000);
    store
        .record_event(&NewEvent {
            kind: "system.boot".into(),
            group_id: None,
            payload: r#"{"ok":true}"#.into(),
            created_at: ts,
        })
        .await
        .expect("record_event");

    let rows = store
        .list_events(
            &EventFilter {
                kind: Some("system.boot".into()),
                ..Default::default()
            },
            10,
        )
        .await
        .expect("list system.boot");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].group_id.is_none());
}
