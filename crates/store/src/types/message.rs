//! Message types — the channel messages persisted to the store.

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use serde::{Deserialize, Serialize};

/// A message being written to the store.
///
/// The caller generates `id` (typically via [`crate::generate_id`]) as
/// a **correlation key**, not a sort key. The store assigns a
/// monotonic `seq` on insert; paging is strictly by that `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// Caller-generated correlation id (UUIDv7). Unique but not used
    /// to order results.
    pub id: String,
    /// Group this message belongs to.
    pub group_id: GroupId,
    /// Channel the message was received on.
    pub channel_id: ChannelId,
    /// Opaque sender identifier (empty string for system messages).
    pub sender: String,
    /// Message body.
    pub content: String,
    /// Event timestamp in UTC. This is caller-supplied and may be
    /// backdated — it is stored for diagnostics and UI purposes and
    /// does **not** drive pagination order.
    pub created_at: DateTime<Utc>,
}

/// A message row returned by the store.
///
/// `seq` is the store-assigned monotonic ordinal. Downstream code
/// builds new cursors by reading `seq` off the most-recently-seen
/// message and passing it in [`Cursor::after`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Store-owned monotonic ordinal. Use this to build a
    /// follow-up [`Cursor`].
    pub seq: i64,
    /// Caller-supplied correlation id.
    pub id: String,
    /// Group this message belongs to.
    pub group_id: GroupId,
    /// Channel the message was received on.
    pub channel_id: ChannelId,
    /// Opaque sender identifier.
    pub sender: String,
    /// Message body.
    pub content: String,
    /// Event timestamp in UTC (caller-supplied).
    pub created_at: DateTime<Utc>,
}

/// Exclusive cursor for [`crate::Store::get_messages_since`].
///
/// The cursor wraps a single store-owned monotonic `seq` and is
/// strictly greater-than: `get_messages_since(group, cursor, n)`
/// returns rows with `seq > cursor.seq`. Because `seq` is assigned by
/// the database at insert time, no caller can produce a row with a
/// smaller `seq` than one already visible — this closes the classic
/// "backdated insert skipped by cursor" hole.
///
/// A fresh caller starts from [`Cursor::beginning`].
///
/// # Known limitation: PostgreSQL MVCC commit ordering
///
/// On PostgreSQL a sequence value (`BIGSERIAL`) is reserved at insert
/// time but becomes visible to readers only at commit time. Two
/// concurrent transactions that acquire seqs `5` and `6` in that order
/// can still commit in `6, 5` order, so a reader may see row `6`
/// before `5` is visible. If the reader advances its cursor past `5`,
/// it will skip that row once it finally commits. This is inherent to
/// any sequence-based cursor on an MVCC database and is **not** solved
/// here.
///
/// Mitigations live at the router layer: either (a) only advance past
/// rows older than a short "lag window" so in-flight transactions
/// have time to commit, or (b) inspect `pg_snapshot_xmin` on Postgres
/// to wait for the oldest in-flight write. Neither is needed on
/// SQLite, which does not expose the same MVCC gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Exclusive lower bound on `seq`. Readers get back rows where
    /// the row's `seq` is strictly greater than this value.
    pub seq: i64,
}

impl Cursor {
    /// Return a cursor that sorts before every possible row. The
    /// first `seq` assigned by either backend is `>= 1`, so `seq = 0`
    /// is a safe "before anything" sentinel.
    #[must_use]
    pub const fn beginning() -> Self {
        Self { seq: 0 }
    }

    /// Build a cursor that points at the end of a returned batch, so
    /// the next call resumes immediately after the last row.
    #[must_use]
    pub const fn after(seq: i64) -> Self {
        Self { seq }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::beginning()
    }
}
