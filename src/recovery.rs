//! Execution state persistence for crash recovery.
//!
//! The ExecutionStateStore persists running task state to survive daemon crashes,
//! enabling task recovery on restart.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::error::{Error, Result};

/// Persists execution state for crash recovery.
pub struct ExecutionStateStore {
    /// Base directory for state files.
    path: PathBuf,
}

/// Persisted state for a running task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedExecutionState {
    /// Task ID.
    pub task_id: String,
    /// Current iteration.
    pub iteration: u32,
    /// Tokens used so far.
    pub tokens_used: u64,
    /// Cost so far in USD.
    pub cost: f64,
    /// Phase of agentic loop (initialization, thinking, tool_execution, etc).
    pub phase: String,
    /// Working directory for the task.
    pub working_dir: PathBuf,
    /// Worktree path if using git isolation.
    pub worktree_path: Option<PathBuf>,
    /// Last checkpoint timestamp.
    pub checkpoint_at: DateTime<Utc>,
    /// Reason task was started (for recovery decision).
    pub started_reason: String,
}

impl ExecutionStateStore {
    /// Create a new execution state store.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Ensure the state directory exists.
    pub async fn ensure_dir(&self) -> Result<()> {
        if !self.path.exists() {
            fs::create_dir_all(&self.path).await.map_err(|e| {
                Error::Io(std::io::Error::other(format!(
                    "Failed to create execution state directory: {}",
                    e
                )))
            })?;
        }
        Ok(())
    }

    /// Get the file path for a task's state.
    fn state_path(&self, task_id: &str) -> PathBuf {
        self.path.join(format!("{}.json", task_id))
    }

    /// Save execution state (called periodically and on phase transitions).
    pub async fn save(&self, task_id: &str, state: &PersistedExecutionState) -> Result<()> {
        self.ensure_dir().await?;
        let file_path = self.state_path(task_id);
        let json = serde_json::to_string_pretty(state).map_err(|e| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to serialize execution state: {}", e),
            ))
        })?;
        fs::write(&file_path, json)
            .await
            .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to write execution state: {}", e))))?;
        Ok(())
    }

    /// Load execution state for a task.
    pub async fn load(&self, task_id: &str) -> Result<Option<PersistedExecutionState>> {
        let file_path = self.state_path(task_id);
        if !file_path.exists() {
            return Ok(None);
        }
        let json = fs::read_to_string(&file_path)
            .await
            .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to read execution state: {}", e))))?;
        let state: PersistedExecutionState = serde_json::from_str(&json).map_err(|e| {
            log::warn!("Corrupted execution state file {}: {}", file_path.display(), e);
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Failed to parse execution state: {}", e),
            ))
        })?;
        Ok(Some(state))
    }

    /// Remove execution state (task completed or failed).
    pub async fn remove(&self, task_id: &str) -> Result<()> {
        let file_path = self.state_path(task_id);
        if file_path.exists() {
            fs::remove_file(&file_path).await.map_err(|e| {
                Error::Io(std::io::Error::other(format!(
                    "Failed to remove execution state: {}",
                    e
                )))
            })?;
        }
        Ok(())
    }

    /// List all persisted execution states (for recovery).
    pub async fn list_all(&self) -> Result<Vec<PersistedExecutionState>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let mut states = Vec::new();
        let mut entries = fs::read_dir(&self.path).await.map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "Failed to read execution state directory: {}",
                e
            )))
        })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| Error::Io(std::io::Error::other(format!("Failed to read directory entry: {}", e))))?
        {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
                match fs::read_to_string(&path).await {
                    Ok(json) => match serde_json::from_str(&json) {
                        Ok(state) => states.push(state),
                        Err(e) => {
                            log::warn!("Skipping corrupted state file {}: {}", path.display(), e);
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to read state file {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(states)
    }

    /// Check if a task has persisted state.
    pub async fn has_state(&self, task_id: &str) -> bool {
        self.state_path(task_id).exists()
    }

    /// Get the base path.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, ExecutionStateStore) {
        let temp = TempDir::new().unwrap();
        let store = ExecutionStateStore::new(temp.path().join("execution_state"));
        (temp, store)
    }

    fn make_state(task_id: &str) -> PersistedExecutionState {
        PersistedExecutionState {
            task_id: task_id.to_string(),
            iteration: 5,
            tokens_used: 10000,
            cost: 0.25,
            phase: "tool_execution".to_string(),
            working_dir: PathBuf::from("/home/user/project"),
            worktree_path: None,
            checkpoint_at: Utc::now(),
            started_reason: "user_request".to_string(),
        }
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let (_temp, store) = setup().await;

        let state = make_state("task-123");
        store.save("task-123", &state).await.unwrap();

        let loaded = store.load("task-123").await.unwrap().unwrap();
        assert_eq!(loaded.task_id, "task-123");
        assert_eq!(loaded.iteration, 5);
        assert_eq!(loaded.tokens_used, 10000);
        assert_eq!(loaded.phase, "tool_execution");
    }

    #[tokio::test]
    async fn test_load_nonexistent() {
        let (_temp, store) = setup().await;

        let loaded = store.load("nonexistent").await.unwrap();
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn test_remove() {
        let (_temp, store) = setup().await;

        let state = make_state("task-456");
        store.save("task-456", &state).await.unwrap();
        assert!(store.has_state("task-456").await);

        store.remove("task-456").await.unwrap();
        assert!(!store.has_state("task-456").await);
    }

    #[tokio::test]
    async fn test_list_all() {
        let (_temp, store) = setup().await;

        store.save("task-1", &make_state("task-1")).await.unwrap();
        store.save("task-2", &make_state("task-2")).await.unwrap();
        store.save("task-3", &make_state("task-3")).await.unwrap();

        let all = store.list_all().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_list_all_empty() {
        let (_temp, store) = setup().await;

        let all = store.list_all().await.unwrap();
        assert!(all.is_empty());
    }
}
