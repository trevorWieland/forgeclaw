//! Persistence ops for the `tasks` table.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{GroupId, TaskId};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
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

/// Return every task whose `next_run <= now` with status `active`,
/// ordered by `next_run` ascending.
pub(crate) async fn get_due_tasks(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
) -> Result<Vec<StoredTask>, StoreError> {
    let rows = tasks::Entity::find()
        .filter(tasks::Column::Status.eq(TaskStatus::Active.as_str()))
        .filter(tasks::Column::NextRun.is_not_null())
        .filter(tasks::Column::NextRun.lte(now))
        .order_by_asc(tasks::Column::NextRun)
        .all(db)
        .await?;

    rows.into_iter().map(row_to_stored).collect()
}

/// Record the outcome of a task run and update scheduling fields.
pub(crate) async fn update_task_after_run(
    db: &DatabaseConnection,
    id: &TaskId,
    result: &TaskRunResult,
) -> Result<(), StoreError> {
    let encoded = serde_json::to_string(result)?;
    let am = tasks::ActiveModel {
        id: Set(id.as_ref().to_owned()),
        status: Set(result.status.as_str().to_owned()),
        next_run: Set(result.next_run),
        last_result: Set(Some(encoded)),
        updated_at: Set(result.ran_at),
        ..Default::default()
    };
    am.update(db).await?;
    Ok(())
}

fn row_to_stored(row: tasks::Model) -> Result<StoredTask, StoreError> {
    let schedule_kind =
        ScheduleKind::from_str(&row.schedule_kind).map_err(|e| StoreError::InvalidCursor {
            reason: format!("unknown schedule_kind in db: {e}"),
        })?;
    let status = TaskStatus::from_str(&row.status).map_err(|e| StoreError::InvalidCursor {
        reason: format!("unknown task status in db: {e}"),
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
