//! Shared types, event bus, command bus, error taxonomy, and configuration.
//!
//! This is the foundation crate for Forgeclaw. It has zero dependencies on
//! other workspace crates — every other crate depends on it.
//!
//! ## Communication model
//!
//! Forgeclaw uses two complementary patterns (CQRS, per design principle 1):
//!
//! - **[`EventBus`]** — broadcast, fire-and-observe.  All subscribers see
//!   every event.  For metrics, logging, and decoupled reactions.  Event
//!   payloads are not required to be `Serialize`, so variants may carry
//!   `oneshot::Sender` for request-response when the response should
//!   also be observable.
//!
//! - **[`CommandBus`]** — point-to-point, request-response.  Exactly one
//!   handler processes each command and returns a typed result.  For
//!   actions that need a reply.  Handlers typically process a command
//!   *and* emit an event so the observation layer stays informed.

pub mod command;
pub mod config;
pub mod error;
pub mod event;
pub mod id;

pub use command::{Command, CommandBus, CommandError, CommandReceiver, Responder};
pub use config::ForgeclawConfig;
pub use error::ErrorClass;
pub use event::{Event, EventBus};
pub use id::{
    ChannelId, ContainerId, DispatchId, GroupId, IdError, JobId, PoolId, ProviderId, TaskId,
};
