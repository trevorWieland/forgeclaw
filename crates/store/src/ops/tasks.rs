//! Persistence ops for the `tasks` table.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{GroupId, TaskId};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::entities::tasks;
use crate::error::StoreError;
use crate::ids::generate_id;
use crate::types::task::{NewTask, ScheduleKind, StoredTask, TaskRunResult, TaskStatus};

/// Insert a new scheduled task; store generates its [`TaskId`].
pub(crate) async fn create_task(
    db: &DatabaseConnection,
    task: &NewTask,
) -> Result<TaskId, StoreError> {
    let id = generate_id();
    let now = Utc::now();
    let am = tasks::ActiveModel {
        id: Set(id.clone()),
        group_id: Set(task.group_id.as_ref().to_owned()),
        prompt: Set(task.prompt.clone()),
        schedule_kind: Set(task.schedule_kind.as_str().to_owned()),
        schedule_value: Set(task.schedule_value.clone()),
        status: Set(task.status.as_str().to_owned()),
        next_run: Set(task.next_run),
        last_result: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    Ok(TaskId::from(id))
}

/// Return up to `limit` tasks whose `next_run <= now` with status
/// `active`, ordered by `(next_run ASC, id ASC)`.
///
/// The secondary sort on `id` makes result ordering deterministic
/// across backends when multiple tasks share the same `next_run`.
pub(crate) async fn get_due_tasks(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<StoredTask>, StoreError> {
    let take = super::clamped_take(limit, "get_due_tasks")?;
    if take == 0 {
        return Ok(Vec::new());
    }

    let rows = tasks::Entity::find()
        .filter(tasks::Column::Status.eq(TaskStatus::Active.as_str()))
        .filter(tasks::Column::NextRun.is_not_null())
        .filter(tasks::Column::NextRun.lte(now))
        .order_by_asc(tasks::Column::NextRun)
        .order_by_asc(tasks::Column::Id)
        .limit(take)
        .all(db)
        .await?;

    rows.into_iter().map(row_to_stored).collect()
}

/// Record the outcome of a task run and update scheduling fields.
///
/// Returns [`StoreError::NotFound`] if no task with the given id
/// exists. Every other failure surfaces as [`StoreError::Database`]
/// with a category hint.
pub(crate) async fn update_task_after_run(
    db: &DatabaseConnection,
    id: &TaskId,
    result: &TaskRunResult,
) -> Result<(), StoreError> {
    // Load-then-update, so "missing row" becomes NotFound rather than
    // SeaORM's generic `RecordNotUpdated` (which would classify as an
    // integrity error).
    let existing = tasks::Entity::find_by_id(id.as_ref().to_owned())
        .one(db)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "task".to_owned(),
        })?;

    let encoded = serde_json::to_string(result)?;
    let mut am: tasks::ActiveModel = existing.into();
    am.status = Set(result.status.as_str().to_owned());
    am.next_run = Set(result.next_run);
    am.last_result = Set(Some(encoded));
    am.updated_at = Set(result.ran_at);
    am.update(db).await?;
    Ok(())
}

fn row_to_stored(row: tasks::Model) -> Result<StoredTask, StoreError> {
    let schedule_kind =
        ScheduleKind::from_str(&row.schedule_kind).map_err(|e| StoreError::SchemaDrift {
            table: Some("tasks".to_owned()),
            column: Some("schedule_kind".to_owned()),
            reason: e.to_string(),
        })?;
    let status = TaskStatus::from_str(&row.status).map_err(|e| StoreError::SchemaDrift {
        table: Some("tasks".to_owned()),
        column: Some("status".to_owned()),
        reason: e.to_string(),
    })?;
    Ok(StoredTask {
        id: TaskId::from(row.id),
        group_id: GroupId::from(row.group_id),
        prompt: row.prompt,
        schedule_kind,
        schedule_value: row.schedule_value,
        status,
        next_run: row.next_run,
        last_result: row.last_result,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
