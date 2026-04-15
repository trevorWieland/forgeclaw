//! IPC-layer authorized command types.
//!
//! These types are returned by `recv_command` on both
//! [`crate::server::IpcConnection`] and
//! [`crate::server::IpcConnectionReader`] after the full
//! authorization matrix has been applied. They replace the internal
//! `ClassifiedCommand` as the public command API.

use forgeclaw_core::GroupId;

use crate::error::{IpcError, ProtocolError};

use super::command::{
    CancelTaskPayload, DispatchSelfImprovementPayload, DispatchTanrenPayload, PauseTaskPayload,
    RegisterGroupPayload, ScheduleTaskPayload, SendMessagePayload,
};

/// A command that has passed IPC-layer authorization.
///
/// Commands are separated into explicit scoped vs privileged
/// categories so downstream handlers can enforce category-specific
/// APIs at compile time.
#[derive(Debug)]
pub enum AuthorizedCommand {
    /// Command available to all groups (subject to scope checks).
    Scoped(ScopedAuthorizedCommand),
    /// Command restricted to the main group.
    Privileged(PrivilegedAuthorizedCommand),
}

impl AuthorizedCommand {
    /// Return the scoped command payload, if this command is scoped.
    #[must_use]
    pub fn into_scoped(self) -> Option<ScopedAuthorizedCommand> {
        match self {
            Self::Scoped(cmd) => Some(cmd),
            Self::Privileged(_) => None,
        }
    }

    /// Return the privileged command payload, if this command is privileged.
    #[must_use]
    pub fn into_privileged(self) -> Option<PrivilegedAuthorizedCommand> {
        match self {
            Self::Privileged(cmd) => Some(cmd),
            Self::Scoped(_) => None,
        }
    }

    /// Borrow the scoped command payload, if this command is scoped.
    #[must_use]
    pub fn as_scoped(&self) -> Option<&ScopedAuthorizedCommand> {
        match self {
            Self::Scoped(cmd) => Some(cmd),
            Self::Privileged(_) => None,
        }
    }

    /// Borrow the privileged command payload, if this command is privileged.
    #[must_use]
    pub fn as_privileged(&self) -> Option<&PrivilegedAuthorizedCommand> {
        match self {
            Self::Privileged(cmd) => Some(cmd),
            Self::Scoped(_) => None,
        }
    }
}

impl From<ScopedAuthorizedCommand> for AuthorizedCommand {
    fn from(value: ScopedAuthorizedCommand) -> Self {
        Self::Scoped(value)
    }
}

impl From<PrivilegedAuthorizedCommand> for AuthorizedCommand {
    fn from(value: PrivilegedAuthorizedCommand) -> Self {
        Self::Privileged(value)
    }
}

/// Public command category for non-main-safe commands.
#[derive(Debug)]
pub enum ScopedAuthorizedCommand {
    /// Send a chat message (group scope verified).
    SendMessage(SendMessagePayload),
    /// Schedule a task (group scope verified).
    ScheduleTask(ScheduleTaskPayload),
    /// Dispatch a Tanren job (capability verified).
    DispatchTanren(DispatchTanrenPayload),
    /// Pause a task (ownership must be verified by caller).
    PauseTask(OwnershipPending<PauseTaskPayload>),
    /// Cancel a task (ownership must be verified by caller).
    CancelTask(OwnershipPending<CancelTaskPayload>),
}

/// Public command category for main-only commands.
#[derive(Debug)]
pub enum PrivilegedAuthorizedCommand {
    /// Register a new group (main-only, verified).
    RegisterGroup(RegisterGroupPayload),
    /// Dispatch a self-improvement job (main-only, verified).
    DispatchSelfImprovement(DispatchSelfImprovementPayload),
}

/// Alias matching the architecture docs' non-main command category name.
pub type GroupCommand = ScopedAuthorizedCommand;

/// Alias matching the architecture docs' main-only command category name.
pub type MainGroupCommand = PrivilegedAuthorizedCommand;

/// Wrapper requiring callers to verify resource ownership before
/// accessing the payload.
///
/// The IPC layer confirms the command class is allowed for this
/// session type, but cannot resolve task-to-group ownership without
/// an external store. Callers must call [`verify`](Self::verify)
/// with the owning group of the target resource.
#[derive(Debug)]
pub struct OwnershipPending<T> {
    payload: T,
    session_group: GroupId,
    is_main: bool,
}

impl<T> OwnershipPending<T> {
    /// Create a new pending-ownership wrapper.
    pub(crate) fn new(payload: T, session_group: GroupId, is_main: bool) -> Self {
        Self {
            payload,
            session_group,
            is_main,
        }
    }

    /// Verify the task belongs to `owning_group`. Main sessions
    /// always pass.
    pub fn verify(self, owning_group: &GroupId) -> Result<T, IpcError> {
        if self.is_main || self.session_group == *owning_group {
            Ok(self.payload)
        } else {
            Err(IpcError::Protocol(ProtocolError::Unauthorized {
                command: "task_operation",
                reason: "task not owned by session group",
            }))
        }
    }

    /// The group identity of the session that issued this command.
    #[must_use]
    pub fn session_group(&self) -> &GroupId {
        &self.session_group
    }

    /// Whether the issuing session is the main group (always passes
    /// ownership checks).
    #[must_use]
    pub fn is_main(&self) -> bool {
        self.is_main
    }
}

#[cfg(test)]
mod tests {
    use forgeclaw_core::GroupId;

    use super::{AuthorizedCommand, PrivilegedAuthorizedCommand, ScopedAuthorizedCommand};
    use crate::message::command::{RegisterGroupPayload, SendMessagePayload};

    #[test]
    fn authorized_command_accessors_match_category() {
        let scoped =
            AuthorizedCommand::from(ScopedAuthorizedCommand::SendMessage(SendMessagePayload {
                target_group: GroupId::from("group-a"),
                text: "hello".parse().expect("valid text"),
            }));
        assert!(scoped.as_scoped().is_some());
        assert!(scoped.as_privileged().is_none());

        let privileged = AuthorizedCommand::from(PrivilegedAuthorizedCommand::RegisterGroup(
            RegisterGroupPayload {
                name: "new".parse().expect("valid name"),
                extensions: None,
            },
        ));
        assert!(privileged.as_privileged().is_some());
        assert!(privileged.as_scoped().is_none());
    }

    #[test]
    fn into_category_returns_only_matching_variant() {
        let scoped =
            AuthorizedCommand::from(ScopedAuthorizedCommand::SendMessage(SendMessagePayload {
                target_group: GroupId::from("group-a"),
                text: "hello".parse().expect("valid text"),
            }));
        assert!(scoped.into_scoped().is_some());
        let scoped =
            AuthorizedCommand::from(ScopedAuthorizedCommand::SendMessage(SendMessagePayload {
                target_group: GroupId::from("group-a"),
                text: "hello".parse().expect("valid text"),
            }));
        assert!(scoped.into_privileged().is_none());

        let privileged = AuthorizedCommand::from(PrivilegedAuthorizedCommand::RegisterGroup(
            RegisterGroupPayload {
                name: "new".parse().expect("valid name"),
                extensions: None,
            },
        ));
        assert!(privileged.into_privileged().is_some());
        let privileged = AuthorizedCommand::from(PrivilegedAuthorizedCommand::RegisterGroup(
            RegisterGroupPayload {
                name: "new".parse().expect("valid name"),
                extensions: None,
            },
        ));
        assert!(privileged.into_scoped().is_none());
    }
}
