//! Task types for Neuraphage.
//!
//! A Task wraps an engram::Item with additional neuraphage-specific state.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a task.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    /// Generate a new task ID using UUID v7 (time-ordered).
    pub fn new() -> Self {
        Self(format!("task-{}", Uuid::now_v7()))
    }

    /// Create a TaskId from an engram item ID.
    pub fn from_engram_id(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for TaskId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Task status with neuraphage-specific states.
///
/// Maps to engram's 4 states with additional granularity via labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// Waiting in queue to be scheduled
    Queued,
    /// Actively executing
    Running,
    /// Waiting for user input
    WaitingForUser,
    /// Blocked on resource or dependency
    Blocked,
    /// Paused by user
    Paused,
    /// Successfully completed
    Completed,
    /// Failed with error
    Failed,
    /// Cancelled by user
    Cancelled,
}

impl TaskStatus {
    /// Convert to engram status.
    pub fn to_engram_status(&self) -> engram::Status {
        match self {
            TaskStatus::Queued | TaskStatus::WaitingForUser | TaskStatus::Paused => engram::Status::Open,
            TaskStatus::Running => engram::Status::InProgress,
            TaskStatus::Blocked => engram::Status::Blocked,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled => engram::Status::Closed,
        }
    }

    /// Create status label for engram item.
    pub fn to_label(&self) -> String {
        match self {
            TaskStatus::Queued => "_status_queued".to_string(),
            TaskStatus::Running => "_status_running".to_string(),
            TaskStatus::WaitingForUser => "_status_waiting".to_string(),
            TaskStatus::Blocked => "_status_blocked".to_string(),
            TaskStatus::Paused => "_status_paused".to_string(),
            TaskStatus::Completed => "_status_completed".to_string(),
            TaskStatus::Failed => "_status_failed".to_string(),
            TaskStatus::Cancelled => "_status_cancelled".to_string(),
        }
    }

    /// Parse status from engram item labels.
    pub fn from_labels(labels: &[String], engram_status: engram::Status) -> Self {
        // Check for specific status label
        for label in labels {
            if let Some(status_str) = label.strip_prefix("_status_") {
                match status_str {
                    "queued" => return TaskStatus::Queued,
                    "running" => return TaskStatus::Running,
                    "waiting" => return TaskStatus::WaitingForUser,
                    "blocked" => return TaskStatus::Blocked,
                    "paused" => return TaskStatus::Paused,
                    "completed" => return TaskStatus::Completed,
                    "failed" => return TaskStatus::Failed,
                    "cancelled" => return TaskStatus::Cancelled,
                    _ => {}
                }
            }
        }

        // Fall back to engram status
        match engram_status {
            engram::Status::Open => TaskStatus::Queued,
            engram::Status::InProgress => TaskStatus::Running,
            engram::Status::Blocked => TaskStatus::Blocked,
            engram::Status::Closed => TaskStatus::Completed,
        }
    }

    /// Check if the task is in a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled)
    }

    /// Check if the task can transition to the given status.
    pub fn can_transition_to(&self, to: TaskStatus) -> bool {
        use TaskStatus::*;
        match (self, to) {
            // From Queued
            (Queued, Running | Cancelled) => true,
            // From Running
            (Running, WaitingForUser | Blocked | Paused | Completed | Failed | Cancelled) => true,
            // From WaitingForUser
            (WaitingForUser, Running | Cancelled) => true,
            // From Blocked
            (Blocked, Queued | Running | Cancelled) => true,
            // From Paused
            (Paused, Running | Cancelled) => true,
            // Terminal states cannot transition
            (Completed | Failed | Cancelled, _) => false,
            // Same state is always allowed
            (from, to) if *from == to => true,
            // Everything else is not allowed
            _ => false,
        }
    }
}

/// A neuraphage task.
///
/// Wraps an engram::Item with additional runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique task identifier (maps to engram item ID)
    pub id: TaskId,
    /// Brief task description (maps to engram item title)
    pub description: String,
    /// Extended context (maps to engram item description)
    pub context: Option<String>,
    /// Current status
    pub status: TaskStatus,
    /// Priority (0=critical, 4=low)
    pub priority: u8,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// When the task was created
    pub created_at: DateTime<Utc>,
    /// When the task was last updated
    pub updated_at: DateTime<Utc>,
    /// When the task was closed (if applicable)
    pub closed_at: Option<DateTime<Utc>>,
    /// Close reason (if applicable)
    pub close_reason: Option<String>,
    /// Parent task ID (for subtasks)
    pub parent_id: Option<TaskId>,
    /// Current iteration count
    pub iteration: u32,
    /// Total tokens used
    pub tokens_used: u64,
    /// Total cost in USD
    pub cost: f64,
}

impl Task {
    /// Create a new task.
    pub fn new(description: impl Into<String>, priority: u8) -> Self {
        let now = Utc::now();
        Self {
            id: TaskId::new(),
            description: description.into(),
            context: None,
            status: TaskStatus::Queued,
            priority,
            tags: Vec::new(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            close_reason: None,
            parent_id: None,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
        }
    }

    /// Create a task from an engram item.
    pub fn from_engram_item(item: &engram::Item) -> Self {
        // Filter out status labels from tags
        let tags: Vec<String> = item
            .labels
            .iter()
            .filter(|l| !l.starts_with("_status_"))
            .cloned()
            .collect();

        let status = TaskStatus::from_labels(&item.labels, item.status);

        Self {
            id: TaskId::from_engram_id(&item.id),
            description: item.title.clone(),
            context: item.description.clone(),
            status,
            priority: item.priority,
            tags,
            created_at: item.created_at,
            updated_at: item.updated_at,
            closed_at: item.closed_at,
            close_reason: item.close_reason.clone(),
            parent_id: None, // Would need to query edges to populate
            iteration: 0,    // Runtime state, not persisted in engram
            tokens_used: 0,
            cost: 0.0,
        }
    }

    /// Get labels for engram item (includes status label).
    pub fn to_engram_labels(&self) -> Vec<String> {
        let mut labels = self.tags.clone();
        labels.push(self.status.to_label());
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_id_generation() {
        let id1 = TaskId::new();
        let id2 = TaskId::new();
        assert_ne!(id1, id2);
        assert!(id1.0.starts_with("task-"));
    }

    #[test]
    fn test_status_to_engram() {
        assert_eq!(TaskStatus::Queued.to_engram_status(), engram::Status::Open);
        assert_eq!(TaskStatus::Running.to_engram_status(), engram::Status::InProgress);
        assert_eq!(TaskStatus::Blocked.to_engram_status(), engram::Status::Blocked);
        assert_eq!(TaskStatus::Completed.to_engram_status(), engram::Status::Closed);
    }

    #[test]
    fn test_status_roundtrip() {
        let statuses = [
            TaskStatus::Queued,
            TaskStatus::Running,
            TaskStatus::WaitingForUser,
            TaskStatus::Blocked,
            TaskStatus::Paused,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ];

        for status in statuses {
            let label = status.to_label();
            let labels = vec![label];
            let recovered = TaskStatus::from_labels(&labels, status.to_engram_status());
            assert_eq!(status, recovered, "Roundtrip failed for {:?}", status);
        }
    }

    #[test]
    fn test_status_transitions() {
        // Valid transitions
        assert!(TaskStatus::Queued.can_transition_to(TaskStatus::Running));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::Completed));
        assert!(TaskStatus::Running.can_transition_to(TaskStatus::WaitingForUser));
        assert!(TaskStatus::WaitingForUser.can_transition_to(TaskStatus::Running));

        // Invalid transitions
        assert!(!TaskStatus::Completed.can_transition_to(TaskStatus::Running));
        assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Running));
        assert!(!TaskStatus::Queued.can_transition_to(TaskStatus::Completed));
    }

    #[test]
    fn test_task_creation() {
        let task = Task::new("Test task", 2);
        assert!(task.id.0.starts_with("task-"));
        assert_eq!(task.description, "Test task");
        assert_eq!(task.priority, 2);
        assert_eq!(task.status, TaskStatus::Queued);
        assert!(task.tags.is_empty());
    }

    #[test]
    fn test_terminal_status() {
        assert!(!TaskStatus::Queued.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
    }
}
