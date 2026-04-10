//! Typed command bus for request-response communication.
//!
//! Forgeclaw uses two complementary communication patterns:
//!
//! - **[`EventBus`](crate::event::EventBus)** — broadcast, fire-and-observe.
//!   All subscribers see every event.  Used for observations, metrics,
//!   audit logging, and decoupled reactions.
//!
//! - **[`CommandBus`]** — point-to-point, request-response.  Exactly one
//!   handler processes each command and returns a typed response.  Used
//!   when the caller needs a result (e.g. "spawn a container and tell me
//!   its ID").
//!
//! This separation follows CQRS: the event bus is the read/observation
//! side, the command bus is the write/action side.  Handlers typically
//! process a command *and* emit an event so observers stay informed.
//!
//! ## The `Command` trait
//!
//! Each command type declares its response type at the type level:
//!
//! ```ignore
//! impl Command for SpawnContainer {
//!     type Response = ContainerId;
//! }
//! ```
//!
//! The [`CommandBus<C>`] enforces this contract — you cannot send a
//! `SpawnContainer` and receive a `bool`.
//!
//! ## Usage
//!
//! ```ignore
//! // Create a bus (handler holds the receiver).
//! let (bus, mut rx) = CommandBus::<SpawnContainer>::new(16);
//!
//! // Caller side: send and await.
//! let id = bus.call(SpawnContainer { group }).await?;
//!
//! // Handler side: receive, process, respond.
//! while let Some((cmd, responder)) = rx.recv().await {
//!     let result = provision_container(cmd.group).await;
//!     responder.respond(result);
//! }
//! ```

use tokio::sync::{mpsc, oneshot};

use crate::error::ErrorClass;

/// A typed command that carries its response type at the type level.
///
/// Implement this for each request-response operation in the system.
/// The [`CommandBus`] uses the associated `Response` type to enforce
/// that handlers return the correct type.
pub trait Command: Send + std::fmt::Debug + 'static {
    /// The success type returned by the handler.
    type Response: Send + 'static;
}

/// Error returned when a command cannot be delivered or processed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CommandError {
    /// The handler has been dropped (subsystem shut down).
    #[error("command handler is unavailable")]
    HandlerDropped,
    /// The handler received the command but returned an error.
    #[error(transparent)]
    Handler(ErrorClass),
}

impl CommandError {
    /// Returns `true` if the underlying error is retriable.
    pub fn is_retriable(&self) -> bool {
        match self {
            Self::HandlerDropped => false,
            Self::Handler(e) => e.is_retriable(),
        }
    }
}

/// Internal envelope carrying a command and its oneshot responder.
#[derive(Debug)]
struct Envelope<C: Command> {
    command: C,
    responder: oneshot::Sender<Result<C::Response, ErrorClass>>,
}

/// A handle used by the handler to send a response back to the caller.
///
/// Returned alongside the command from [`CommandReceiver::recv`].
/// Consuming the responder (via [`respond`](Responder::respond)) sends
/// exactly one value back through the oneshot channel.
#[derive(Debug)]
pub struct Responder<R: Send + 'static> {
    tx: oneshot::Sender<Result<R, ErrorClass>>,
}

impl<R: Send + 'static> Responder<R> {
    /// Send a response back to the caller.
    ///
    /// If the caller has been dropped, the response is silently lost
    /// (this is not an error — the caller simply no longer cares).
    pub fn respond(self, result: Result<R, ErrorClass>) {
        // Ignore the error — it means the caller dropped their receiver,
        // which is a normal cancellation path.
        let _ = self.tx.send(result);
    }
}

/// The sending half of a command bus.
///
/// Cheaply cloneable — multiple callers can share the same bus.
/// Each bus targets a single handler (the holder of the
/// [`CommandReceiver`]).
#[derive(Debug)]
pub struct CommandBus<C: Command> {
    tx: mpsc::Sender<Envelope<C>>,
}

// Manual Clone: derive(Clone) adds `C: Clone` bound, which we don't want.
impl<C: Command> Clone for CommandBus<C> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl<C: Command> CommandBus<C> {
    /// Create a new command bus with the given buffer capacity.
    ///
    /// Returns the sending half ([`CommandBus`]) and the receiving half
    /// ([`CommandReceiver`]) for the handler.  Capacity is clamped to a
    /// minimum of 1.
    pub fn new(capacity: usize) -> (Self, CommandReceiver<C>) {
        let effective = capacity.max(1);
        let (tx, rx) = mpsc::channel(effective);
        (Self { tx }, CommandReceiver { rx })
    }

    /// Send a command and await its response.
    ///
    /// This is the primary caller API.  It handles all oneshot plumbing
    /// internally — callers never touch channels directly.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::HandlerDropped`] if the handler has shut
    /// down, or [`CommandError::Handler`] if the handler returned an
    /// [`ErrorClass`].
    pub async fn call(&self, command: C) -> Result<C::Response, CommandError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        let envelope = Envelope {
            command,
            responder: resp_tx,
        };
        self.tx
            .send(envelope)
            .await
            .map_err(|_| CommandError::HandlerDropped)?;
        resp_rx
            .await
            .map_err(|_| CommandError::HandlerDropped)?
            .map_err(CommandError::Handler)
    }
}

/// The receiving half of a command bus, held by the handler subsystem.
///
/// Each call to [`recv`](CommandReceiver::recv) yields a command and a
/// [`Responder`] that the handler uses to send a typed reply.
#[derive(Debug)]
pub struct CommandReceiver<C: Command> {
    rx: mpsc::Receiver<Envelope<C>>,
}

impl<C: Command> CommandReceiver<C> {
    /// Receive the next command, or `None` if all senders are dropped.
    pub async fn recv(&mut self) -> Option<(C, Responder<C::Response>)> {
        let envelope = self.rx.recv().await?;
        let responder = Responder {
            tx: envelope.responder,
        };
        Some((envelope.command, responder))
    }
}

pub mod types;

#[cfg(test)]
mod tests;
