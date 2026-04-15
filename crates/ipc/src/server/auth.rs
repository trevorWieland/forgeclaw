//! Shared command authorization logic.
//!
//! Extracted so both [`super::IpcConnection`] and
//! [`super::IpcConnectionReader`] can enforce the same authorization
//! matrix without duplicating the match arms.

use forgeclaw_core::GroupId;

use crate::error::{IpcError, ProtocolError};
use crate::message::authorized::{
    AuthorizedCommand, OwnershipPending, PrivilegedAuthorizedCommand, ScopedAuthorizedCommand,
};
use crate::message::command::{ClassifiedCommand, PrivilegedCommand, ScopedCommand};
use crate::peer_cred::SessionIdentity;

/// Apply the IPC protocol's command authorization table.
///
/// Enforces:
/// - `register_group`, `dispatch_self_improvement`: main only.
/// - `send_message`, `schedule_task`: own-group for non-main.
/// - `dispatch_tanren`: requires tanren capability.
/// - `pause_task`, `cancel_task`: wrapped in [`OwnershipPending`]
///   so the caller must verify task ownership.
///
/// Returns [`ProtocolError::Unauthorized`] for rejected commands.
pub(crate) fn authorize_command(
    classified: ClassifiedCommand,
    identity: &SessionIdentity,
) -> Result<AuthorizedCommand, IpcError> {
    let group = identity.group();
    let is_main = group.is_main;
    let group_id = group.id.clone();
    let has_tanren = group.capabilities.tanren;

    authorize_classified(classified, &group_id, is_main, has_tanren)
}

fn authorize_classified(
    classified: ClassifiedCommand,
    group_id: &GroupId,
    is_main: bool,
    has_tanren: bool,
) -> Result<AuthorizedCommand, IpcError> {
    match classified {
        ClassifiedCommand::Privileged(PrivilegedCommand::RegisterGroup(p)) => {
            if !is_main {
                return Err(IpcError::Protocol(ProtocolError::Unauthorized {
                    command: "register_group",
                    reason: "requires main-group privilege",
                }));
            }
            Ok(AuthorizedCommand::Privileged(
                PrivilegedAuthorizedCommand::RegisterGroup(p),
            ))
        }
        ClassifiedCommand::Privileged(PrivilegedCommand::DispatchSelfImprovement(p)) => {
            if !is_main {
                return Err(IpcError::Protocol(ProtocolError::Unauthorized {
                    command: "dispatch_self_improvement",
                    reason: "requires main-group privilege",
                }));
            }
            Ok(AuthorizedCommand::Privileged(
                PrivilegedAuthorizedCommand::DispatchSelfImprovement(p),
            ))
        }
        ClassifiedCommand::Scoped(ScopedCommand::SendMessage(p)) => {
            if !is_main && p.target_group != *group_id {
                return Err(IpcError::Protocol(ProtocolError::Unauthorized {
                    command: "send_message",
                    reason: "cross-group target",
                }));
            }
            Ok(AuthorizedCommand::Scoped(
                ScopedAuthorizedCommand::SendMessage(p),
            ))
        }
        ClassifiedCommand::Scoped(ScopedCommand::ScheduleTask(p)) => {
            if !is_main && p.group != *group_id {
                return Err(IpcError::Protocol(ProtocolError::Unauthorized {
                    command: "schedule_task",
                    reason: "cross-group target",
                }));
            }
            Ok(AuthorizedCommand::Scoped(
                ScopedAuthorizedCommand::ScheduleTask(p),
            ))
        }
        ClassifiedCommand::Scoped(ScopedCommand::DispatchTanren(p)) => {
            if !has_tanren {
                return Err(IpcError::Protocol(ProtocolError::Unauthorized {
                    command: "dispatch_tanren",
                    reason: "missing tanren capability",
                }));
            }
            Ok(AuthorizedCommand::Scoped(
                ScopedAuthorizedCommand::DispatchTanren(p),
            ))
        }
        ClassifiedCommand::Scoped(ScopedCommand::PauseTask(p)) => Ok(AuthorizedCommand::Scoped(
            ScopedAuthorizedCommand::PauseTask(OwnershipPending::new(p, group_id.clone(), is_main)),
        )),
        ClassifiedCommand::Scoped(ScopedCommand::CancelTask(p)) => Ok(AuthorizedCommand::Scoped(
            ScopedAuthorizedCommand::CancelTask(OwnershipPending::new(
                p,
                group_id.clone(),
                is_main,
            )),
        )),
    }
}
