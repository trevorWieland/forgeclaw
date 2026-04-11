//! Persistence ops for the `events` audit log.
//!
//! `list_events` uses SeaORM's `apply_if` pattern to compose an
//! arbitrary subset of filter columns while keeping every column
//! reference compile-time typed. There is no runtime-built SQL here.

use forgeclaw_core::id::GroupId;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, QueryTrait,
};

use crate::entities::events;
use crate::error::StoreError;
use crate::ids::generate_id;
use crate::types::event::{EventFilter, NewEvent, StoredEvent};

/// Insert a new event row into the audit log.
pub(crate) async fn record_event(
    db: &DatabaseConnection,
    event: &NewEvent,
) -> Result<(), StoreError> {
    let am = events::ActiveModel {
        id: Set(generate_id()),
        kind: Set(event.kind.clone()),
        group_id: Set(event.group_id.as_ref().map(|g| g.as_ref().to_owned())),
        payload: Set(event.payload.clone()),
        created_at: Set(event.created_at),
    };
    am.insert(db).await?;
    Ok(())
}

/// Return events matching `filter`, newest first, limited to `limit`.
///
/// Each optional field on `filter` is applied via `apply_if` so the
/// generated SQL varies with the input while every column reference
/// stays type-checked at compile time.
pub(crate) async fn list_events(
    db: &DatabaseConnection,
    filter: &EventFilter,
    limit: i64,
) -> Result<Vec<StoredEvent>, StoreError> {
    let take = super::clamped_take(limit, "list_events")?;
    if take == 0 {
        return Ok(Vec::new());
    }

    let rows = events::Entity::find()
        .apply_if(filter.kind.clone(), |q, kind| {
            q.filter(events::Column::Kind.eq(kind))
        })
        .apply_if(filter.group_id.clone(), |q, group| {
            q.filter(events::Column::GroupId.eq(group.as_ref().to_owned()))
        })
        .apply_if(filter.since, |q, since| {
            q.filter(events::Column::CreatedAt.gt(since))
        })
        .order_by_desc(events::Column::CreatedAt)
        .order_by_desc(events::Column::Id)
        .limit(take)
        .all(db)
        .await?;

    Ok(rows.into_iter().map(row_to_stored).collect())
}

fn row_to_stored(row: events::Model) -> StoredEvent {
    StoredEvent {
        id: row.id,
        kind: row.kind,
        group_id: row.group_id.map(GroupId::from),
        payload: row.payload,
        created_at: row.created_at,
    }
}
