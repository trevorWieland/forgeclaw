//! Message types — the channel messages persisted to the store.

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{ChannelId, GroupId};
use serde::{Deserialize, Serialize};

/// A message being written to the store.
///
/// Caller generates the `id` (typically via [`crate::generate_id`]) so
/// that the same identifier can be used as a correlation key on the
/// event bus before the row is durable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMessage {
    /// Primary key, caller-generated.
    pub id: String,
    /// Group this message belongs to.
    pub group_id: GroupId,
    /// Channel the message was received on.
    pub channel_id: ChannelId,
    /// Opaque sender identifier (empty string for system messages).
    pub sender: String,
    /// Message body.
    pub content: String,
    /// Ingest timestamp in UTC.
    pub created_at: DateTime<Utc>,
}

/// A message row returned by the store.
///
/// Structurally identical to [`NewMessage`] today; kept as a distinct
/// type so future DB-managed columns (tenant id, row version) can be
/// added without breaking callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Primary key.
    pub id: String,
    /// Group this message belongs to.
    pub group_id: GroupId,
    /// Channel the message was received on.
    pub channel_id: ChannelId,
    /// Opaque sender identifier.
    pub sender: String,
    /// Message body.
    pub content: String,
    /// Ingest timestamp in UTC.
    pub created_at: DateTime<Utc>,
}

/// Exclusive cursor for [`crate::Store::get_messages_since`].
///
/// The cursor points at the last message already delivered to the caller
/// and is **strictly greater-than** on the composite key
/// `(created_at, id)`. This prevents the two classical cursor failure
/// modes: a plain timestamp cursor would skip or double-deliver messages
/// ingested in the same millisecond, and a plain id cursor cannot be
/// compared across restarts. The composite form is correct under
/// concurrent writes.
///
/// A fresh caller starts from [`Cursor::beginning`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Timestamp component of the composite key.
    pub timestamp: DateTime<Utc>,
    /// Message id component of the composite key.
    pub message_id: String,
}

impl Cursor {
    /// Return a cursor that sorts before every possible message.
    ///
    /// Uses the Unix epoch rather than `DateTime::<Utc>::MIN_UTC` because
    /// the latter serializes on SQLite as a negative-year RFC-3339 string
    /// that some drivers refuse to decode round-trip.
    #[must_use]
    pub fn beginning() -> Self {
        Self {
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap_or_default(),
            message_id: String::new(),
        }
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::beginning()
    }
}
