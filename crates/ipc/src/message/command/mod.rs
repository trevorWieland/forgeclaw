//! Typed command payloads for `CommandBody` variants.

use forgeclaw_core::{GroupId, TaskId};
use serde::{Deserialize, Serialize};

use super::collections::SelfImprovementListItems;
use super::semantic::{IdentifierText, MessageText, PromptText, ScheduleValueText};

mod extensions;

pub use extensions::{
    GroupExtensions, GroupExtensionsError, GroupExtensionsVersion, GroupExtensionsVersionError,
};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct SendMessagePayload {
    /// Group to send the message to.
    pub target_group: GroupId,
    /// Message text.
    pub text: MessageText,
}

/// Schedule a recurring or one-off task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ScheduleTaskPayload {
    /// Group to schedule the task for.
    pub group: GroupId,
    /// Schedule type.
    pub schedule_type: ScheduleType,
    /// Schedule value (cron expression, duration, or timestamp).
    pub schedule_value: ScheduleValueText,
    /// The prompt to execute on each run.
    pub prompt: PromptText,
    /// Optional context mode override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_mode: Option<IdentifierText>,
}

/// Pause a previously scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PauseTaskPayload {
    /// Task to pause.
    pub task_id: TaskId,
}

/// Cancel a previously scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CancelTaskPayload {
    /// Task to cancel.
    pub task_id: TaskId,
}

/// Register a new group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct RegisterGroupPayload {
    /// Human-readable group name.
    pub name: IdentifierText,
    /// Additional group configuration, versioned and typed as a
    /// JSON object. See [`GroupExtensions`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(
        feature = "json-schema",
        schemars(schema_with = "extensions::group_extensions_schema")
    )]
    pub extensions: Option<GroupExtensions>,
}

/// Dispatch a Tanren job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DispatchTanrenPayload {
    /// Target project name.
    pub project: IdentifierText,
    /// Git branch to work on.
    pub branch: IdentifierText,
    /// Tanren execution phase.
    pub phase: TanrenPhase,
    /// Prompt / instructions for the dispatch.
    pub prompt: PromptText,
    /// Optional environment profile override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_profile: Option<IdentifierText>,
}

/// Dispatch a self-improvement job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct DispatchSelfImprovementPayload {
    /// Human-readable objective for the improvement.
    pub objective: PromptText,
    /// Scope constraints (e.g. which crates or subsystems).
    pub scopes: SelfImprovementListItems,
    /// Acceptance tests that must pass for the improvement to land.
    pub acceptance_tests: SelfImprovementListItems,
    /// Branch strategy for this improvement.
    pub branch_policy: BranchPolicy,
}

/// The set of commands an agent may issue to the host.
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

/// Commands available to any agent (with scope checks enforced by the router).
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

/// Commands restricted to the main agent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PrivilegedCommand {
    /// Register a new group.
    RegisterGroup(RegisterGroupPayload),
    /// Dispatch a self-improvement job.
    DispatchSelfImprovement(DispatchSelfImprovementPayload),
}

/// A command classified by privilege level (internal).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClassifiedCommand {
    /// A scoped command any agent may attempt.
    Scoped(ScopedCommand),
    /// A privileged command only the main agent may issue.
    Privileged(PrivilegedCommand),
}

/// `command` — IPC command from the agent to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct CommandPayload {
    /// The command discriminator and its associated payload.
    #[serde(flatten)]
    pub body: CommandBody,
}

#[cfg(test)]
#[path = "../command_tests.rs"]
mod tests;
