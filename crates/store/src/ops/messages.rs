//! Persistence ops for the `messages` table.

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};

use crate::entities::messages;
use crate::error::StoreError;
use crate::types::message::{Cursor, NewMessage, StoredMessage};

/// Insert a single message.
pub(crate) async fn store_message(
    db: &DatabaseConnection,
    msg: &NewMessage,
) -> Result<(), StoreError> {
    let am = messages::ActiveModel {
        id: Set(msg.id.clone()),
        group_id: Set(msg.group_id.as_ref().to_owned()),
        channel_id: Set(msg.channel_id.as_ref().to_owned()),
        sender: Set(msg.sender.clone()),
        content: Set(msg.content.clone()),
        created_at: Set(msg.created_at),
    };
    am.insert(db).await?;
    Ok(())
}

/// Fetch messages strictly after `cursor` for a given group, ordered
/// by `(created_at, id)` ascending, limited to `limit` rows.
///
/// The cursor is exclusive on the composite `(created_at, id)` key, so
/// messages ingested concurrently at the same millisecond are returned
/// deterministically by id and never skipped or double-delivered.
pub(crate) async fn get_messages_since(
    db: &DatabaseConnection,
    group: &GroupId,
    cursor: &Cursor,
    limit: i64,
) -> Result<Vec<StoredMessage>, StoreError> {
    if limit <= 0 {
        return Ok(Vec::new());
    }
    let take = u64::try_from(limit).map_err(|_| StoreError::InvalidCursor {
        reason: format!("limit {limit} cannot be converted to u64"),
    })?;

    // Composite-key strict greater-than: (created_at > ts) OR
    // (created_at = ts AND id > mid). Expressed via SeaORM's typed
    // Column API so a wrong column name or operand type fails at
    // compile time.
    let ts = cursor.timestamp;
    let mid = cursor.message_id.clone();

    let rows = messages::Entity::find()
        .filter(messages::Column::GroupId.eq(group.as_ref()))
        .filter(
            messages::Column::CreatedAt
                .gt(ts)
                .or(messages::Column::CreatedAt
                    .eq(ts)
                    .and(messages::Column::Id.gt(mid))),
        )
        .order_by_asc(messages::Column::CreatedAt)
        .order_by_asc(messages::Column::Id)
        .limit(take)
        .all(db)
        .await?;

    Ok(rows.into_iter().map(row_to_stored).collect())
}

fn row_to_stored(row: messages::Model) -> StoredMessage {
    StoredMessage {
        id: row.id,
        group_id: GroupId::from(row.group_id),
        channel_id: ChannelId::from(row.channel_id),
        sender: row.sender,
        content: row.content,
        created_at: coerce_utc(row.created_at),
    }
}

/// Identity on `DateTime<Utc>` — exists so the row → domain conversion
/// has a single chokepoint if we ever need to adjust timezone handling
/// for a particular backend.
fn coerce_utc(ts: DateTime<Utc>) -> DateTime<Utc> {
    ts
}
