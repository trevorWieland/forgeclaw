//! Persistence ops for the `messages` table.
//!
//! Cursor pagination uses the store-owned `seq` column (an integer
//! assigned by the database on insert) as the sole sort key. This
//! closes the backdating hole that a caller-controlled `(created_at,
//! id)` cursor would leave open: no caller can ever produce a `seq`
//! smaller than one already delivered.
//!
//! See [`crate::Cursor`] for the MVCC commit-order caveat that still
//! applies on PostgreSQL — store closes the backdating hole, but the
//! router is responsible for tolerating the commit-visibility gap.

use forgeclaw_core::id::{ChannelId, GroupId};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
};

use crate::entities::messages;
use crate::error::StoreError;
use crate::types::message::{Cursor, NewMessage, StoredMessage};

/// Insert a single message. The database assigns `seq` automatically.
pub(crate) async fn store_message(
    db: &DatabaseConnection,
    msg: &NewMessage,
) -> Result<(), StoreError> {
    let am = messages::ActiveModel {
        seq: NotSet,
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

/// Fetch messages strictly after `cursor.seq` for a given group,
/// ordered by `seq` ascending, limited to `limit` rows clamped against
/// [`crate::MAX_PAGE_SIZE`].
///
/// A freshly-allocated `seq` is always `>= 1`, so `Cursor::beginning()`
/// (seq = 0) returns every row in the group.
pub(crate) async fn get_messages_since(
    db: &DatabaseConnection,
    group: &GroupId,
    cursor: &Cursor,
    limit: i64,
) -> Result<Vec<StoredMessage>, StoreError> {
    let take = super::clamped_take(limit, "get_messages_since")?;
    if take == 0 {
        return Ok(Vec::new());
    }

    let rows = messages::Entity::find()
        .filter(messages::Column::GroupId.eq(group.as_ref()))
        .filter(messages::Column::Seq.gt(cursor.seq))
        .order_by_asc(messages::Column::Seq)
        .limit(take)
        .all(db)
        .await?;

    Ok(rows.into_iter().map(row_to_stored).collect())
}

fn row_to_stored(row: messages::Model) -> StoredMessage {
    StoredMessage {
        seq: row.seq,
        id: row.id,
        group_id: GroupId::from(row.group_id),
        channel_id: ChannelId::from(row.channel_id),
        sender: row.sender,
        content: row.content,
        created_at: row.created_at,
    }
}
