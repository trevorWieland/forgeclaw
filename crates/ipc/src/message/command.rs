//! Typed command payloads for `CommandBody` variants.
//!
//! Every struct here mirrors a row in the command table of
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Container → Host → `command`.
//!
//! This module provides the transport-level typed contract. IPC-boundary
//! authorization (main-only, own-group, capability checks) is enforced at
//! the server layer via [`AuthorizedCommand`](crate::message::authorized::AuthorizedCommand).
//! Only external ownership resolution remains deferred via [`OwnershipPending`](crate::message::authorized::OwnershipPending).

use forgeclaw_core::{GroupId, TaskId};
use serde::{Deserialize, Serialize};

/// The type of schedule for a scheduled task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ScheduleType {
    /// Cron expression schedule.
    Cron,
    /// Fixed-interval schedule.
    Interval,
    /// Single execution at a specific time.
    Once,
}

/// Tanren execution phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TanrenPhase {
    /// Execute the primary task.
    DoTask,
    /// Run gate checks (tests, lints).
    Gate,
    /// Perform a code audit (wire name: `audit-task`).
    AuditTask,
}

/// Branch strategy for self-improvement dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BranchPolicy {
    /// Create a new branch for the work.
    Create,
    /// Reuse an existing branch.
    Reuse,
}

/// Send a chat message to a group.
///
/// Authorization: main agent may target any group; non-main agents
/// may target only their own group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SendMessagePayload {
    /// Group to send the message to.
    pub target_group: GroupId,
    /// Message text.
    pub text: String,
}

/// Schedule a recurring or one-off task.
///
/// Authorization: main agent may target any group; non-main agents
/// may target only their own group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ScheduleTaskPayload {
    /// Group to schedule the task for.
    pub group: GroupId,
    /// Schedule type.
    pub schedule_type: ScheduleType,
    /// Schedule value (cron expression, duration, or timestamp).
    pub schedule_value: String,
    /// The prompt to execute on each run.
    pub prompt: String,
    /// Optional context mode override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_mode: Option<String>,
}

/// Pause a previously scheduled task.
///
/// Authorization: main agent may pause any task; non-main agents may
/// pause only their own tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PauseTaskPayload {
    /// Task to pause.
    pub task_id: TaskId,
}

/// Cancel a previously scheduled task.
///
/// Authorization: main agent may cancel any task; non-main agents may
/// cancel only their own tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CancelTaskPayload {
    /// Task to cancel.
    pub task_id: TaskId,
}

/// Typed extension envelope for group registration.
///
/// Enforces that extensions are a JSON object (not an arbitrary
/// scalar or array) and carries a schema version for forward
/// compatibility. Shape details are owned by the `router` crate;
/// this crate transports the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct GroupExtensions {
    /// Schema version for extension compatibility (e.g. `"1"`).
    pub version: String,
    /// Extension data as a JSON object.
    #[serde(flatten)]
    pub data: serde_json::Map<String, serde_json::Value>,
}

/// Register a new group.
///
/// Authorization: main agent only.
///
/// The required `name` field is typed here. Additional group
/// configuration owned by the `router` crate travels in the
/// optional [`GroupExtensions`] envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct RegisterGroupPayload {
    /// Human-readable group name.
    pub name: String,
    /// Additional group configuration, versioned and typed as a
    /// JSON object. See [`GroupExtensions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<GroupExtensions>,
}

/// Dispatch a Tanren job.
///
/// Authorization: groups with Tanren permission only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DispatchTanrenPayload {
    /// Target project name.
    pub project: String,
    /// Git branch to work on.
    pub branch: String,
    /// Tanren execution phase.
    pub phase: TanrenPhase,
    /// Prompt / instructions for the dispatch.
    pub prompt: String,
    /// Optional environment profile override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_profile: Option<String>,
}

/// Dispatch a self-improvement job.
///
/// Authorization: main agent only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DispatchSelfImprovementPayload {
    /// Human-readable objective for the improvement.
    pub objective: String,
    /// Scope constraints (e.g. which crates or subsystems).
    pub scopes: Vec<String>,
    /// Acceptance tests that must pass for the improvement to land.
    pub acceptance_tests: Vec<String>,
    /// Branch strategy for this improvement.
    pub branch_policy: BranchPolicy,
}

/// The set of commands an agent may issue to the host.
///
/// The `command` field is the tag on the wire; each variant carries a
/// typed payload struct matching the shapes documented in
/// `docs/IPC_PROTOCOL.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "command", content = "payload", rename_all = "snake_case")]
pub enum CommandBody {
    /// Send a chat message to a group.
    SendMessage(SendMessagePayload),
    /// Schedule a recurring or one-off task.
    ScheduleTask(ScheduleTaskPayload),
    /// Pause a previously scheduled task.
    PauseTask(PauseTaskPayload),
    /// Cancel a previously scheduled task.
    CancelTask(CancelTaskPayload),
    /// Register a new group (main-only).
    RegisterGroup(RegisterGroupPayload),
    /// Dispatch a Tanren job.
    DispatchTanren(DispatchTanrenPayload),
    /// Dispatch a self-improvement job (main-only).
    DispatchSelfImprovement(DispatchSelfImprovementPayload),
}

impl CommandBody {
    /// Returns `true` if this command requires main-group privilege.
    #[must_use]
    pub fn is_privileged(&self) -> bool {
        matches!(
            self,
            Self::RegisterGroup(_) | Self::DispatchSelfImprovement(_)
        )
    }

    /// Classify into the privilege-separated type system.
    ///
    /// Used internally by the server to apply the authorization
    /// matrix before returning an [`AuthorizedCommand`].
    #[must_use]
    pub(crate) fn classify(self) -> ClassifiedCommand {
        match self {
            Self::SendMessage(p) => ClassifiedCommand::Scoped(ScopedCommand::SendMessage(p)),
            Self::ScheduleTask(p) => ClassifiedCommand::Scoped(ScopedCommand::ScheduleTask(p)),
            Self::PauseTask(p) => ClassifiedCommand::Scoped(ScopedCommand::PauseTask(p)),
            Self::CancelTask(p) => ClassifiedCommand::Scoped(ScopedCommand::CancelTask(p)),
            Self::DispatchTanren(p) => ClassifiedCommand::Scoped(ScopedCommand::DispatchTanren(p)),
            Self::RegisterGroup(p) => {
                ClassifiedCommand::Privileged(PrivilegedCommand::RegisterGroup(p))
            }
            Self::DispatchSelfImprovement(p) => {
                ClassifiedCommand::Privileged(PrivilegedCommand::DispatchSelfImprovement(p))
            }
        }
    }
}

/// Commands available to any agent (with scope checks enforced by
/// the router). These commands target a group or task that the
/// agent may or may not own — authorization is policy, not
/// transport.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ScopedCommand {
    /// Send a chat message to a group.
    SendMessage(SendMessagePayload),
    /// Schedule a recurring or one-off task.
    ScheduleTask(ScheduleTaskPayload),
    /// Pause a previously scheduled task.
    PauseTask(PauseTaskPayload),
    /// Cancel a previously scheduled task.
    CancelTask(CancelTaskPayload),
    /// Dispatch a Tanren job.
    DispatchTanren(DispatchTanrenPayload),
}

/// Commands restricted to the main agent. Attempting to issue
/// these from a non-main group is a protocol violation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PrivilegedCommand {
    /// Register a new group.
    RegisterGroup(RegisterGroupPayload),
    /// Dispatch a self-improvement job.
    DispatchSelfImprovement(DispatchSelfImprovementPayload),
}

/// A command classified by privilege level (internal).
///
/// Constructed via [`CommandBody::classify`]. Used internally by
/// [`crate::server::IpcConnectionReader::recv_command`] to apply the
/// full authorization matrix before returning an
/// [`AuthorizedCommand`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClassifiedCommand {
    /// A scoped command any agent may attempt (subject to
    /// authorization checks in the router).
    Scoped(ScopedCommand),
    /// A privileged command only the main agent may issue.
    Privileged(PrivilegedCommand),
}

/// `command` — IPC command from the agent to the host.
///
/// Wraps [`CommandBody`] with `#[serde(flatten)]` so the serialized
/// JSON contains `command` and `payload` fields at the top level
/// alongside the `type` discriminator added by
/// [`crate::message::ContainerToHost`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CommandPayload {
    /// The command discriminator and its associated payload.
    #[serde(flatten)]
    pub body: CommandBody,
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgeclaw_core::{GroupId, TaskId};
    use serde_json::json;

    #[test]
    fn send_message_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::from("group-main"),
                text: "Task completed successfully.".to_owned(),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "send_message");
        assert_eq!(json["payload"]["target_group"], "group-main");
        assert_eq!(json["payload"]["text"], "Task completed successfully.");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn schedule_task_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::ScheduleTask(ScheduleTaskPayload {
                group: GroupId::from("group-main"),
                schedule_type: ScheduleType::Cron,
                schedule_value: "0 9 * * *".to_owned(),
                prompt: "Check status".to_owned(),
                context_mode: Some("full".to_owned()),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "schedule_task");
        assert_eq!(json["payload"]["schedule_type"], "cron");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn schedule_task_omits_context_mode_when_none() {
        let cmd = CommandPayload {
            body: CommandBody::ScheduleTask(ScheduleTaskPayload {
                group: GroupId::from("g"),
                schedule_type: ScheduleType::Once,
                schedule_value: "2026-04-12T00:00:00Z".to_owned(),
                prompt: "p".to_owned(),
                context_mode: None,
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        let payload = json["payload"].as_object().expect("payload object");
        assert!(!payload.contains_key("context_mode"));
    }

    #[test]
    fn pause_task_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::PauseTask(PauseTaskPayload {
                task_id: TaskId::from("task-1"),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "pause_task");
        assert_eq!(json["payload"]["task_id"], "task-1");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn cancel_task_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::CancelTask(CancelTaskPayload {
                task_id: TaskId::from("task-2"),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "cancel_task");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn register_group_roundtrip() {
        let ext = GroupExtensions {
            version: "1".to_owned(),
            data: {
                let mut m = serde_json::Map::new();
                m.insert("trigger".to_owned(), json!("@bot"));
                m
            },
        };
        let cmd = CommandPayload {
            body: CommandBody::RegisterGroup(RegisterGroupPayload {
                name: "New Group".to_owned(),
                extensions: Some(ext),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "register_group");
        assert_eq!(json["payload"]["name"], "New Group");
        assert_eq!(json["payload"]["extensions"]["version"], "1");
        assert_eq!(json["payload"]["extensions"]["trigger"], "@bot");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn register_group_omits_extensions_when_none() {
        let cmd = CommandPayload {
            body: CommandBody::RegisterGroup(RegisterGroupPayload {
                name: "Minimal Group".to_owned(),
                extensions: None,
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        let payload = json["payload"].as_object().expect("payload object");
        assert!(!payload.contains_key("extensions"));
    }

    #[test]
    fn dispatch_tanren_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::DispatchTanren(DispatchTanrenPayload {
                project: "forgeclaw".to_owned(),
                branch: "main".to_owned(),
                phase: TanrenPhase::DoTask,
                prompt: "Implement feature X".to_owned(),
                environment_profile: Some("large-vm".to_owned()),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "dispatch_tanren");
        assert_eq!(json["payload"]["phase"], "do-task");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn dispatch_self_improvement_roundtrip() {
        let cmd = CommandPayload {
            body: CommandBody::DispatchSelfImprovement(DispatchSelfImprovementPayload {
                objective: "Add retry logic".to_owned(),
                scopes: vec!["crates/ipc".to_owned()],
                acceptance_tests: vec!["cargo nextest run -p forgeclaw-ipc".to_owned()],
                branch_policy: BranchPolicy::Create,
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "dispatch_self_improvement");
        assert!(json["payload"]["scopes"].is_array());
        assert!(json["payload"]["acceptance_tests"].is_array());
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }

    #[test]
    fn classify_returns_scoped_for_group_commands() {
        let scoped_commands = [
            CommandBody::SendMessage(SendMessagePayload {
                target_group: GroupId::from("g"),
                text: "t".to_owned(),
            }),
            CommandBody::ScheduleTask(ScheduleTaskPayload {
                group: GroupId::from("g"),
                schedule_type: ScheduleType::Once,
                schedule_value: "v".to_owned(),
                prompt: "p".to_owned(),
                context_mode: None,
            }),
            CommandBody::PauseTask(PauseTaskPayload {
                task_id: TaskId::from("t"),
            }),
            CommandBody::CancelTask(CancelTaskPayload {
                task_id: TaskId::from("t"),
            }),
            CommandBody::DispatchTanren(DispatchTanrenPayload {
                project: "p".to_owned(),
                branch: "b".to_owned(),
                phase: TanrenPhase::DoTask,
                prompt: "pr".to_owned(),
                environment_profile: None,
            }),
        ];
        for cmd in scoped_commands {
            assert!(!cmd.is_privileged());
            assert!(matches!(cmd.classify(), ClassifiedCommand::Scoped(_)));
        }
    }

    #[test]
    fn classify_returns_privileged_for_main_commands() {
        let privileged_commands = [
            CommandBody::RegisterGroup(RegisterGroupPayload {
                name: "g".to_owned(),
                extensions: None,
            }),
            CommandBody::DispatchSelfImprovement(DispatchSelfImprovementPayload {
                objective: "o".to_owned(),
                scopes: vec!["s".to_owned()],
                acceptance_tests: vec!["t".to_owned()],
                branch_policy: BranchPolicy::Create,
            }),
        ];
        for cmd in privileged_commands {
            assert!(cmd.is_privileged());
            assert!(matches!(cmd.classify(), ClassifiedCommand::Privileged(_)));
        }
    }
}
