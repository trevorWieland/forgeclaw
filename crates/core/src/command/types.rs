//! Concrete command types used across Forgeclaw subsystems.
//!
//! Each struct implements [`Command`] with a specific `Response` type,
//! enforcing the request-response contract at the type level.

use super::Command;
use crate::id::{ContainerId, GroupId};

/// Request to spawn a container for a group.
///
/// Handled by the container manager.  The response is the newly
/// provisioned [`ContainerId`].
#[derive(Debug)]
pub struct SpawnContainer {
    /// The group that needs a container.
    pub group: GroupId,
}

impl Command for SpawnContainer {
    type Response = ContainerId;
}

/// Request to check the health of a specific component.
///
/// Handled by the health subsystem.  The response is `true` if the
/// component is healthy.
#[derive(Debug)]
pub struct HealthCheck {
    /// The component to check (e.g. "database", "provider:anthropic").
    pub component: String,
}

impl Command for HealthCheck {
    type Response = bool;
}
