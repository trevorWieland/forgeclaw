//! Broadcast event bus for fire-and-observe communication.
//!
//! The [`EventBus`] is one half of Forgeclaw's communication model:
//!
//! - **Events** (this module) — broadcast to all subscribers.  Used for
//!   observations, metrics, audit logging, and decoupled reactions.
//!   Events are `Clone + Send + Sync` (but not `Serialize`, so future
//!   variants may carry non-serializable payloads like `oneshot::Sender`
//!   for request-response patterns per design principle 1).
//!
//! - **Commands** ([`crate::command`]) — point-to-point, exactly-once
//!   delivery with a typed response.  Used when the caller needs a
//!   result.  Handlers typically process a command *and* emit an event
//!   so the observation layer stays informed.
//!
//! This separation follows CQRS: the event bus is the read/observation
//! side, the command bus is the write/action side.  For request-response
//! patterns, prefer the command bus; for patterns where the response
//! should also be observable, an event variant carrying a
//! `oneshot::Sender` is also valid.

mod bus;
mod types;

pub use bus::EventBus;
pub use types::*;
