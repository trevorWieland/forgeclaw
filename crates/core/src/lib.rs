//! Shared types, event bus, error taxonomy, and configuration loading.
//!
//! This is the foundation crate for Forgeclaw. It has zero dependencies on
//! other workspace crates — every other crate depends on it.

pub mod config;
pub mod error;
pub mod event;
pub mod id;

pub use config::ForgeclawConfig;
pub use error::ErrorClass;
pub use event::{Event, EventBus};
pub use id::{ChannelId, ContainerId, DispatchId, GroupId, JobId, PoolId, ProviderId, TaskId};
