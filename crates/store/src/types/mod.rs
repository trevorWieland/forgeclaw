//! Public domain types for the `store` crate.
//!
//! These types form the API surface that downstream crates consume.
//! They are deliberately backend-agnostic: no `sea_orm::*` items leak
//! through, and persistence concerns (row IDs, timestamps) are hidden
//! behind plain Rust fields.

pub mod event;
pub mod group;
pub mod message;
pub mod task;
