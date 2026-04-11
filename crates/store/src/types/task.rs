//! Task types — the store-level row representation of scheduled tasks.
//!
//! The scheduler crate will define a richer `ScheduledTask` enum for
//! its domain logic; store persists a flat row and returns [`StoredTask`]
//! to keep the persistence layer backend-agnostic.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use forgeclaw_core::id::{GroupId, TaskId};

/// A new task to insert into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTask {
    /// Group this task belongs to.
    pub group_id: GroupId,
    /// Prompt the task will run.
    pub prompt: String,
    /// How the task is scheduled.
    pub schedule_kind: ScheduleKind,
    /// Schedule payload — a cron expression, interval seconds, or
    /// ISO-8601 timestamp depending on `schedule_kind`.
    pub schedule_value: String,
    /// Initial status.
    pub status: TaskStatus,
    /// Initial next-run timestamp (None for manual-only tasks).
    pub next_run: Option<DateTime<Utc>>,
}

/// A task row returned by the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTask {
    /// Primary key.
    pub id: TaskId,
    /// Group this task belongs to.
    pub group_id: GroupId,
    /// Prompt the task will run.
    pub prompt: String,
    /// How the task is scheduled.
    pub schedule_kind: ScheduleKind,
    /// Schedule payload (cron expression, interval seconds, or ISO-8601).
    pub schedule_value: String,
    /// Current status.
    pub status: TaskStatus,
    /// Next scheduled run, if any.
    pub next_run: Option<DateTime<Utc>>,
    /// JSON-encoded [`TaskRunResult`] from the most recent run, if any.
    pub last_result: Option<String>,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task was last updated.
    pub updated_at: DateTime<Utc>,
}

/// Outcome of running a scheduled task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRunResult {
    /// When the run started (UTC).
    pub ran_at: DateTime<Utc>,
    /// What happened.
    pub outcome: RunOutcome,
    /// Next run time, if the task is still active.
    pub next_run: Option<DateTime<Utc>>,
    /// New status after the run.
    pub status: TaskStatus,
}

/// Coarse outcome of a single task run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RunOutcome {
    /// The task succeeded.
    Success {
        /// Optional human-readable detail.
        detail: Option<String>,
    },
    /// The task failed.
    Failure {
        /// Human-readable error description.
        error: String,
    },
}

/// Kinds of task schedule supported by the store row layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleKind {
    /// A cron expression in `schedule_value`.
    Cron,
    /// A fixed interval (seconds) in `schedule_value`.
    Interval,
    /// A single absolute timestamp in `schedule_value`.
    Once,
}

impl ScheduleKind {
    /// Return the canonical lowercase string form used in the DB column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cron => "cron",
            Self::Interval => "interval",
            Self::Once => "once",
        }
    }
}

impl fmt::Display for ScheduleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScheduleKind {
    type Err = UnknownScheduleKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cron" => Ok(Self::Cron),
            "interval" => Ok(Self::Interval),
            "once" => Ok(Self::Once),
            other => Err(UnknownScheduleKind(other.to_owned())),
        }
    }
}

/// Error returned when a task row contains a schedule kind the store
/// doesn't understand (indicates schema drift or manual tampering).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown schedule kind: {0}")]
pub struct UnknownScheduleKind(pub String);

/// Task lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Eligible for dispatch.
    Active,
    /// Temporarily paused (e.g. by a circuit breaker).
    Paused,
    /// Finished successfully (e.g. a `Once` task that ran).
    Completed,
    /// Terminal failure.
    Failed,
}

impl TaskStatus {
    /// Return the canonical lowercase string form used in the DB column.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = UnknownTaskStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(UnknownTaskStatus(other.to_owned())),
        }
    }
}

/// Error returned when a task row contains a status the store doesn't
/// understand.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown task status: {0}")]
pub struct UnknownTaskStatus(pub String);

#[cfg(test)]
mod tests {
    use super::{ScheduleKind, TaskStatus};
    use std::str::FromStr;

    #[test]
    fn schedule_kind_round_trips() {
        for kind in [
            ScheduleKind::Cron,
            ScheduleKind::Interval,
            ScheduleKind::Once,
        ] {
            let parsed =
                ScheduleKind::from_str(kind.as_str()).expect("canonical form should parse");
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn task_status_round_trips() {
        for status in [
            TaskStatus::Active,
            TaskStatus::Paused,
            TaskStatus::Completed,
            TaskStatus::Failed,
        ] {
            let parsed =
                TaskStatus::from_str(status.as_str()).expect("canonical form should parse");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn unknown_schedule_kind_errors() {
        assert!(ScheduleKind::from_str("bogus").is_err());
    }

    #[test]
    fn unknown_task_status_errors() {
        assert!(TaskStatus::from_str("bogus").is_err());
    }
}
