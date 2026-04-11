//! Group types — registered groups (logical agent contexts).

use chrono::{DateTime, Utc};
use forgeclaw_core::id::GroupId;

/// A registered group row.
///
/// `config_json` is an opaque JSON snapshot owned by the caller. Store
/// makes no attempt to parse or validate it — persistence is not the
/// config-schema's concern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredGroup {
    /// Group identifier.
    pub id: GroupId,
    /// Human-readable display name.
    pub display_name: String,
    /// Opaque JSON snapshot of the group's config (caller-serialized).
    pub config_json: String,
    /// Whether the group is currently active.
    pub active: bool,
    /// When the group was first registered (immutable across upserts).
    pub created_at: DateTime<Utc>,
    /// When the group was last updated.
    pub updated_at: DateTime<Utc>,
}
