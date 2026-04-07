//! Event bus and typed event definitions.
//!
//! The event bus is the central communication mechanism in Forgeclaw.
//! Subsystems emit events and other subsystems subscribe to observe them,
//! enabling fire-and-observe semantics without coupling.

mod bus;
mod types;

pub use bus::EventBus;
pub use types::*;
