//! Internal query implementations, organized by entity.
//!
//! Each submodule holds free functions that take a `&DatabaseConnection`
//! and public domain types; [`crate::Store`] delegates each public
//! method to one of these functions. This keeps [`crate::Store`] small
//! and each op file under the 500-line workspace cap.

pub(crate) mod events;
pub(crate) mod groups;
pub(crate) mod messages;
pub(crate) mod sessions;
pub(crate) mod state;
pub(crate) mod tasks;
