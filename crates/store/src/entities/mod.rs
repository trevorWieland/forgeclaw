//! Private SeaORM entities backing the six store tables.
//!
//! These types are `pub(crate)` — they are implementation details of
//! the `ops` modules and never leak out of the crate. Downstream callers
//! see only the plain [`crate::types`] structs.

pub(crate) mod events;
pub(crate) mod groups;
pub(crate) mod messages;
pub(crate) mod sessions;
pub(crate) mod state;
pub(crate) mod tasks;
