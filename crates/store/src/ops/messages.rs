//! Persistence ops for the `messages` table.
//!
//! Cursor pagination uses the store-owned `seq` column (an integer
//! assigned by the database on insert) as the sole sort key. On
//! SQLite the database-level single-writer lock guarantees that
//! seq allocation order equals commit order. On PostgreSQL we
//! achieve the same per stream by wrapping every insert in a
//! transaction that first acquires a transaction-scoped advisory
//! lock keyed on the target `group_id`:
//!
//! ```text
//! BEGIN;
//!   SELECT pg_advisory_xact_lock(
//!       hashtextextended('forgeclaw_store_messages:' || $1, 0)
//!   );                                        -- $1 = group_id
//!   INSERT INTO messages (...) VALUES (...);  -- seq assigned here
//! COMMIT;                                     -- lock released
//! ```
//!
//! Scoping the lock by `group_id` means concurrent inserts to
//! *different* groups do not contend with each other, so one busy
//! chat cannot head-of-line block any other group's persistence.
//! Within a single group, writers still serialize on the group's
//! key, so `seq` values are allocated in strict commit order for
//! that stream. Since `get_messages_since` always filters by
//! `group_id`, a reader that observes `seq = N` in group `G` is
//! guaranteed every earlier `seq` in group `G` is also visible —
//! no MVCC commit-order gap is possible on any single stream, and
//! callers do not need to tolerate one.

use forgeclaw_core::id::{ChannelId, GroupId};
use sea_orm::{
    ActiveModelTrait,
    ActiveValue::{NotSet, Set},
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Statement, TransactionTrait,
};

use crate::entities::messages;
use crate::error::StoreError;
use crate::types::message::{Cursor, NewMessage, StoredMessage};

/// Parameterized advisory-lock SQL. `$1` is the `group_id` string
/// prefixed with a namespace so this crate's locks can't collide
/// with future crates' advisory locks that hash different strings.
const ADVISORY_LOCK_SQL: &str =
    "SELECT pg_advisory_xact_lock(hashtextextended('forgeclaw_store_messages:' || $1, 0))";

/// Insert a single message. The database assigns `seq` automatically.
///
/// On PostgreSQL the insert runs inside a transaction that first
/// acquires a per-group transaction-scoped advisory lock, so `seq`
/// allocation order matches commit order *within that group*.
/// Concurrent inserts to other groups do not contend with this
/// lock. On SQLite the database-level writer lock already gives
/// the same guarantee, so the advisory lock is skipped.
pub(crate) async fn store_message(
    db: &DatabaseConnection,
    msg: &NewMessage,
) -> Result<(), StoreError> {
    let txn = db.begin().await?;
    let backend = txn.get_database_backend();

    if matches!(backend, DbBackend::Postgres) {
        let lock_stmt = Statement::from_sql_and_values(
            backend,
            ADVISORY_LOCK_SQL,
            [msg.group_id.as_ref().into()],
        );
        txn.execute(lock_stmt).await?;
    }

    let am = messages::ActiveModel {
        seq: NotSet,
        id: Set(msg.id.clone()),
        group_id: Set(msg.group_id.as_ref().to_owned()),
        channel_id: Set(msg.channel_id.as_ref().to_owned()),
        sender: Set(msg.sender.clone()),
        content: Set(msg.content.clone()),
        created_at: Set(msg.created_at),
    };
    am.insert(&txn).await?;

    txn.commit().await?;
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
