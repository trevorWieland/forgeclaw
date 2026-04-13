//! Typed command payloads for `CommandBody` variants.
//!
//! Every struct here mirrors a row in the command table of
//! [`docs/IPC_PROTOCOL.md`](../../../../docs/IPC_PROTOCOL.md)
//! §Container → Host → `command`.
//!
//! Authorization and validation (e.g. "main-only", "own group only")
//! are policy — they live in later crates (`router`, `scheduler`,
//! `tanren`), not here. This crate provides the compile-time typed
//! contract; callers enforce invariants at runtime.

use forgeclaw_core::{GroupId, TaskId};
use serde::{Deserialize, Serialize};

/// The type of schedule for a scheduled task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[serde(rename_all = "kebab-case")]
pub enum TanrenPhase {
    /// Execute the primary task.
    DoTask,
    /// Run gate checks (tests, lints).
    Gate,
    /// Perform a code audit.
    Audit,
}

/// Branch strategy for self-improvement dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct PauseTaskPayload {
    /// Task to pause.
    pub task_id: TaskId,
}

/// Cancel a previously scheduled task.
///
/// Authorization: main agent may cancel any task; non-main agents may
/// cancel only their own tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelTaskPayload {
    /// Task to cancel.
    pub task_id: TaskId,
}

/// Register a new group.
///
/// Authorization: main agent only.
///
/// The group spec is kept as opaque JSON because the full group
/// configuration shape is owned by the `router` crate — this crate
/// only transports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterGroupPayload {
    /// Group configuration. Shape is defined by the `router` crate;
    /// this crate transports it as an opaque JSON value.
    pub group_spec: serde_json::Value,
}

/// Dispatch a Tanren job.
///
/// Authorization: groups with Tanren permission only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct DispatchSelfImprovementPayload {
    /// Human-readable objective for the improvement.
    pub objective: String,
    /// Scope constraint (e.g. which crate or subsystem).
    pub scope: String,
    /// Acceptance tests that must pass for the improvement to land.
    pub acceptance_tests: String,
    /// Branch strategy for this improvement.
    pub branch_policy: BranchPolicy,
}

/// The set of commands an agent may issue to the host.
///
/// The `command` field is the tag on the wire; each variant carries a
/// typed payload struct matching the shapes documented in
/// `docs/IPC_PROTOCOL.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// `command` — IPC command from the agent to the host.
///
/// Wraps [`CommandBody`] with `#[serde(flatten)]` so the serialized
/// JSON contains `command` and `payload` fields at the top level
/// alongside the `type` discriminator added by
/// [`crate::message::ContainerToHost`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        let spec = json!({"name": "New Group", "trigger": "@bot"});
        let cmd = CommandPayload {
            body: CommandBody::RegisterGroup(RegisterGroupPayload {
                group_spec: spec.clone(),
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "register_group");
        assert_eq!(json["payload"]["group_spec"], spec);
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
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
                scope: "crates/ipc".to_owned(),
                acceptance_tests: "cargo nextest run -p forgeclaw-ipc".to_owned(),
                branch_policy: BranchPolicy::Create,
            }),
        };
        let json = serde_json::to_value(&cmd).expect("serialize");
        assert_eq!(json["command"], "dispatch_self_improvement");
        let back: CommandPayload = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, cmd);
    }
}
