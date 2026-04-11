//! Event types — the audit log persisted to the store.
//!
//! These events are written by higher-level subsystems (router, scheduler,
//! health) as a durable trace of what flowed through the event bus.
//! Store treats payloads as opaque JSON strings; interpretation is the
//! caller's concern.

use chrono::{DateTime, Utc};
use forgeclaw_core::id::GroupId;

/// An event being written to the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    /// Event type discriminator (e.g. `"task.completed"`,
    /// `"message.received"`). Caller-defined naming convention.
    pub kind: String,
    /// Group this event relates to, if any.
    pub group_id: Option<GroupId>,
    /// Opaque JSON payload (caller-serialized).
    pub payload: String,
    /// When the event occurred (UTC).
    pub created_at: DateTime<Utc>,
}

/// An event row returned by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// Primary key (store-generated).
    pub id: String,
    /// Event type discriminator.
    pub kind: String,
    /// Group this event relates to, if any.
    pub group_id: Option<GroupId>,
    /// Opaque JSON payload.
    pub payload: String,
    /// When the event occurred.
    pub created_at: DateTime<Utc>,
}

/// Filter for [`crate::Store::list_events`].
///
/// Each field is optional and applied as an `AND` condition; an
/// all-default filter matches every event.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Match only events whose `kind` equals this value.
    pub kind: Option<String>,
    /// Match only events belonging to this group.
    pub group_id: Option<GroupId>,
    /// Match only events with `created_at > since`.
    pub since: Option<DateTime<Utc>>,
}
