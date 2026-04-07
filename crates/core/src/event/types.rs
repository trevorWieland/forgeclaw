//! Typed event definitions for the event bus.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{ChannelId, ContainerId, DispatchId, GroupId, ProviderId, TaskId};

/// A system event emitted on the event bus.
///
/// Each variant wraps a specific event struct carrying the relevant payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// A message was received from a channel.
    Message(MessageEvent),
    /// A container changed state.
    Container(ContainerEvent),
    /// A provider's health or budget changed.
    Provider(ProviderEvent),
    /// A Tanren dispatch changed status.
    Tanren(TanrenEvent),
    /// A scheduled task is due or completed.
    Task(TaskEvent),
    /// System health changed.
    Health(HealthEvent),
    /// An IPC message was received from a container.
    Ipc(IpcEvent),
    /// Configuration was reloaded.
    Config(ConfigEvent),
}

/// A message received from a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEvent {
    /// The group this message belongs to.
    pub group: GroupId,
    /// The channel the message arrived on.
    pub channel: ChannelId,
    /// Who sent the message.
    pub sender: String,
    /// The message text content.
    pub text: String,
    /// When the message was received.
    pub timestamp: DateTime<Utc>,
}

/// A container lifecycle state change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerEvent {
    /// The container that changed state.
    pub container: ContainerId,
    /// The group this container belongs to.
    pub group: GroupId,
    /// What kind of state change occurred.
    pub kind: ContainerEventKind,
}

/// The kind of container state change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContainerEventKind {
    /// Container process started.
    Started,
    /// Container is ready to accept work.
    Ready,
    /// Container is actively processing a request.
    Processing,
    /// Container finished processing and is idle.
    Idle,
    /// Container exited normally.
    Exited,
    /// Container failed (crash, OOM, timeout).
    Failed,
}

/// A provider health or budget change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEvent {
    /// The provider that changed.
    pub provider: ProviderId,
    /// What kind of change occurred.
    pub kind: ProviderEventKind,
}

/// The kind of provider event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderEventKind {
    /// Provider became healthy / available.
    Healthy,
    /// Provider became unhealthy / unavailable.
    Unhealthy,
    /// Provider budget threshold was crossed.
    BudgetAlert,
    /// Provider budget was exhausted.
    BudgetExhausted,
}

/// A Tanren dispatch status change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TanrenEvent {
    /// The dispatch that changed status.
    pub dispatch: DispatchId,
    /// What kind of status change occurred.
    pub kind: TanrenEventKind,
}

/// The kind of Tanren dispatch event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TanrenEventKind {
    /// Dispatch was submitted.
    Submitted,
    /// Dispatch completed successfully.
    Completed,
    /// Dispatch failed.
    Failed,
}

/// A scheduled task event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskEvent {
    /// The task that is due or completed.
    pub task: TaskId,
    /// What kind of task event occurred.
    pub kind: TaskEventKind,
}

/// The kind of task event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskEventKind {
    /// Task is due for execution.
    Due,
    /// Task completed successfully.
    Completed,
    /// Task failed.
    Failed,
}

/// A system health change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthEvent {
    /// The component whose health changed (e.g. "database", "provider:anthropic").
    pub component: String,
    /// Whether the component is currently healthy.
    pub healthy: bool,
    /// Optional human-readable message.
    pub message: Option<String>,
}

/// An IPC message received from a container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcEvent {
    /// The container that sent the message.
    pub container: ContainerId,
    /// The message payload.
    pub payload: String,
}

/// A configuration reload event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigEvent {
    /// The config keys that changed.
    pub keys_changed: Vec<String>,
}
