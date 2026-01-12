//! Git worktree management for parallel task execution.
//!
//! Each task operates in its own worktree with a dedicated branch,
//! preventing file conflicts between concurrent tasks and enabling
//! clean merge workflows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::error::{Error, Result};
use crate::task::TaskId;

/// Minimum git version required for worktree support.
const MIN_GIT_VERSION: (u32, u32) = (2, 5);

/// Manages Git worktrees for parallel task execution.
pub struct GitCoordinator {
    /// Base directory for worktrees (e.g., ~/.config/neuraphage/.worktrees)
    worktree_base: PathBuf,
    /// Track active worktrees
    active_worktrees: HashMap<TaskId, WorktreeInfo>,
}

/// Information about an active worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Task that owns this worktree
    pub task_id: TaskId,
    /// Path to the worktree
    pub path: PathBuf,
    /// Branch name (e.g., "neuraphage/task-abc123")
    pub branch: String,
    /// Source repository path
    pub repo_path: PathBuf,
    /// When the worktree was created
    pub created_at: DateTime<Utc>,
}

/// Persisted worktree registry for crash recovery.
#[derive(Debug, Serialize, Deserialize)]
pub struct WorktreeRegistry {
    pub worktrees: HashMap<String, PersistedWorktreeInfo>,
}

/// Serializable worktree info.
#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedWorktreeInfo {
    pub task_id: String,
    pub path: String,
    pub branch: String,
    pub repo_path: String,
    pub created_at: String,
}

/// Result of reconciling in-memory state with actual git state.
#[derive(Debug)]
pub struct ReconcileResult {
    /// Worktrees that exist on disk but aren't tracked
    pub orphaned: Vec<PathBuf>,
    /// Tracked worktrees that no longer exist on disk
    pub missing: Vec<TaskId>,
}

impl GitCoordinator {
    /// Create a new GitCoordinator.
    pub fn new(worktree_base: PathBuf) -> Self {
        Self {
            worktree_base,
            active_worktrees: HashMap::new(),
        }
    }

    /// Check if git is available and meets minimum version.
    pub async fn check_git_version() -> Result<(u32, u32, u32)> {
        let output = Command::new("git").args(["--version"]).output().await?;

        if !output.status.success() {
            return Err(Error::Git {
                command: "version check".to_string(),
                stderr: "git not found".to_string(),
            });
        }

        let version_str = String::from_utf8_lossy(&output.stdout);
        // Parse "git version 2.39.0" or similar
        let version = version_str.split_whitespace().nth(2).ok_or_else(|| Error::Git {
            command: "version parse".to_string(),
            stderr: format!("Could not parse version: {}", version_str),
        })?;

        let parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();

        let (major, minor, patch) = match parts.as_slice() {
            [major, minor, patch, ..] => (*major, *minor, *patch),
            [major, minor] => (*major, *minor, 0),
            [major] => (*major, 0, 0),
            _ => {
                return Err(Error::Git {
                    command: "version parse".to_string(),
                    stderr: format!("Invalid version format: {}", version),
                });
            }
        };

        if (major, minor) < MIN_GIT_VERSION {
            return Err(Error::Git {
                command: "version check".to_string(),
                stderr: format!(
                    "Git version {}.{}.{} is too old. Minimum required: {}.{}",
                    major, minor, patch, MIN_GIT_VERSION.0, MIN_GIT_VERSION.1
                ),
            });
        }

        Ok((major, minor, patch))
    }

    /// Check if a path is inside a git repository.
    pub async fn is_git_repo(&self, path: &Path) -> bool {
        Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(path)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get the root of the git repository containing a path.
    pub async fn repo_root(&self, path: &Path) -> Result<PathBuf> {
        let output = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .output()
            .await?;

        if !output.status.success() {
            return Err(Error::NotGitRepo {
                path: path.to_path_buf(),
            });
        }

        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(PathBuf::from(root))
    }

    /// Check if a branch exists in the repository.
    async fn branch_exists(&self, repo_path: &Path, branch: &str) -> Result<bool> {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", branch])
            .current_dir(repo_path)
            .output()
            .await?;

        Ok(output.status.success())
    }

    /// Set up a worktree for a task.
    ///
    /// Creates a new worktree with a dedicated branch from the current HEAD.
    pub async fn setup_worktree(&mut self, task_id: &TaskId, repo_path: &Path) -> Result<PathBuf> {
        // Ensure repo_path is a git repository
        let repo_root = self.repo_root(repo_path).await?;

        // Create worktree path
        let worktree_path = self.worktree_base.join(&task_id.0);
        let branch_name = format!("neuraphage/{}", task_id.0);

        // Ensure worktree base exists
        tokio::fs::create_dir_all(&self.worktree_base).await?;

        // Check if worktree already exists
        if worktree_path.exists() {
            return Err(Error::WorktreeExists {
                task_id: task_id.clone(),
                path: worktree_path,
            });
        }

        // Check if branch already exists
        if self.branch_exists(&repo_root, &branch_name).await? {
            // Branch exists, create worktree using existing branch
            let output = Command::new("git")
                .args(["worktree", "add", worktree_path.to_str().unwrap(), &branch_name])
                .current_dir(&repo_root)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::Git {
                    command: "worktree add (existing branch)".to_string(),
                    stderr: stderr.to_string(),
                });
            }
        } else {
            // Create the worktree with a new branch
            let output = Command::new("git")
                .args(["worktree", "add", worktree_path.to_str().unwrap(), "-b", &branch_name])
                .current_dir(&repo_root)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Cleanup partial worktree if it was created
                let _ = tokio::fs::remove_dir_all(&worktree_path).await;
                return Err(Error::Git {
                    command: "worktree add".to_string(),
                    stderr: stderr.to_string(),
                });
            }
        }

        // Track the worktree
        let info = WorktreeInfo {
            task_id: task_id.clone(),
            path: worktree_path.clone(),
            branch: branch_name,
            repo_path: repo_root,
            created_at: Utc::now(),
        };
        self.active_worktrees.insert(task_id.clone(), info);

        Ok(worktree_path)
    }

    /// Clean up a worktree after task completion.
    ///
    /// Removes the worktree but preserves the branch for review.
    pub async fn cleanup_worktree(&mut self, task_id: &TaskId, delete_branch: bool) -> Result<()> {
        let info = self
            .active_worktrees
            .remove(task_id)
            .ok_or_else(|| Error::WorktreeNotFound {
                task_id: task_id.clone(),
            })?;

        // Remove the worktree
        let output = Command::new("git")
            .args(["worktree", "remove", info.path.to_str().unwrap()])
            .current_dir(&info.repo_path)
            .output()
            .await?;

        if !output.status.success() {
            // Try force remove if normal remove fails
            let output = Command::new("git")
                .args(["worktree", "remove", "--force", info.path.to_str().unwrap()])
                .current_dir(&info.repo_path)
                .output()
                .await?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Error::Git {
                    command: "worktree remove".to_string(),
                    stderr: stderr.to_string(),
                });
            }
        }

        // Optionally delete the branch
        if delete_branch {
            let _ = Command::new("git")
                .args(["branch", "-d", &info.branch])
                .current_dir(&info.repo_path)
                .output()
                .await;
            // Ignore branch delete errors (might have unmerged changes)
        }

        Ok(())
    }

    /// List all active worktrees.
    pub fn list_worktrees(&self) -> Vec<&WorktreeInfo> {
        self.active_worktrees.values().collect()
    }

    /// Get worktree info for a task.
    pub fn get_worktree(&self, task_id: &TaskId) -> Option<&WorktreeInfo> {
        self.active_worktrees.get(task_id)
    }

    /// Prune stale worktree references.
    pub async fn prune(&self, repo_path: &Path) -> Result<()> {
        Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(repo_path)
            .output()
            .await?;
        Ok(())
    }

    /// Merge a branch into target.
    pub async fn merge_branch(&self, repo_path: &Path, branch: &str, target: &str) -> Result<()> {
        // Checkout target branch first
        let output = Command::new("git")
            .args(["checkout", target])
            .current_dir(repo_path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Git {
                command: format!("checkout {}", target),
                stderr: stderr.to_string(),
            });
        }

        // Merge the branch
        let output = Command::new("git")
            .args(["merge", branch, "--no-edit"])
            .current_dir(repo_path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Git {
                command: format!("merge {}", branch),
                stderr: stderr.to_string(),
            });
        }

        Ok(())
    }

    /// Save worktree state to disk.
    pub async fn save_state(&self, path: &Path) -> Result<()> {
        let registry = WorktreeRegistry {
            worktrees: self
                .active_worktrees
                .iter()
                .map(|(id, info)| {
                    (
                        id.0.clone(),
                        PersistedWorktreeInfo {
                            task_id: info.task_id.0.clone(),
                            path: info.path.to_string_lossy().to_string(),
                            branch: info.branch.clone(),
                            repo_path: info.repo_path.to_string_lossy().to_string(),
                            created_at: info.created_at.to_rfc3339(),
                        },
                    )
                })
                .collect(),
        };

        let json = serde_json::to_string_pretty(&registry)?;
        tokio::fs::write(path, json).await?;
        Ok(())
    }

    /// Load worktree state from disk and reconcile with actual git state.
    pub async fn load_state(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        let json = tokio::fs::read_to_string(path).await?;
        let registry: WorktreeRegistry = serde_json::from_str(&json)?;

        for (_, info) in registry.worktrees {
            let worktree_path = PathBuf::from(&info.path);

            // Verify worktree still exists
            if worktree_path.exists() {
                self.active_worktrees.insert(
                    TaskId(info.task_id.clone()),
                    WorktreeInfo {
                        task_id: TaskId(info.task_id),
                        path: worktree_path,
                        branch: info.branch,
                        repo_path: PathBuf::from(info.repo_path),
                        created_at: DateTime::parse_from_rfc3339(&info.created_at)
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(|_| Utc::now()),
                    },
                );
            }
        }

        Ok(())
    }

    /// Reconcile in-memory state with actual git worktrees.
    pub async fn reconcile(&mut self, repo_path: &Path) -> Result<ReconcileResult> {
        let output = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(repo_path)
            .output()
            .await?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let actual_worktrees = Self::parse_worktree_list(&stdout);

        let mut orphaned = Vec::new();
        let mut missing = Vec::new();

        // Find orphaned (in git but not tracked)
        for path in &actual_worktrees {
            if path.to_string_lossy().contains(".worktrees/") {
                let task_id = path.file_name().and_then(|n| n.to_str()).map(|s| TaskId(s.to_string()));
                if let Some(id) = task_id
                    && !self.active_worktrees.contains_key(&id)
                {
                    orphaned.push(path.clone());
                }
            }
        }

        // Find missing (tracked but not in git)
        for (id, info) in &self.active_worktrees {
            if !actual_worktrees.contains(&info.path) {
                missing.push(id.clone());
            }
        }

        // Remove missing from tracking
        for id in &missing {
            self.active_worktrees.remove(id);
        }

        Ok(ReconcileResult { orphaned, missing })
    }

    /// Parse `git worktree list --porcelain` output into paths.
    fn parse_worktree_list(output: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for line in output.lines() {
            if let Some(path) = line.strip_prefix("worktree ") {
                paths.push(PathBuf::from(path));
            }
        }
        paths
    }

    /// Get the worktree base directory.
    pub fn worktree_base(&self) -> &Path {
        &self.worktree_base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    fn init_git_repo(path: &Path) -> bool {
        StdCommand::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
            && StdCommand::new("git")
                .args(["config", "user.email", "test@test.com"])
                .current_dir(path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            && StdCommand::new("git")
                .args(["config", "user.name", "Test"])
                .current_dir(path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }

    fn create_initial_commit(path: &Path) -> bool {
        std::fs::write(path.join("README.md"), "# Test").is_ok()
            && StdCommand::new("git")
                .args(["add", "."])
                .current_dir(path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            && StdCommand::new("git")
                .args(["commit", "-m", "Initial commit"])
                .current_dir(path)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
    }

    #[tokio::test]
    async fn test_git_version_check() {
        // This test might fail in environments without git
        let result = GitCoordinator::check_git_version().await;
        if let Ok((major, minor, _)) = result {
            assert!(major >= MIN_GIT_VERSION.0);
            if major == MIN_GIT_VERSION.0 {
                assert!(minor >= MIN_GIT_VERSION.1);
            }
        }
    }

    #[tokio::test]
    async fn test_is_git_repo() {
        let temp = TempDir::new().unwrap();
        let coordinator = GitCoordinator::new(temp.path().join("worktrees"));

        // Not a git repo initially
        assert!(!coordinator.is_git_repo(temp.path()).await);

        // Initialize git repo
        if init_git_repo(temp.path()) {
            assert!(coordinator.is_git_repo(temp.path()).await);
        }
    }

    #[tokio::test]
    async fn test_repo_root() {
        let temp = TempDir::new().unwrap();
        let coordinator = GitCoordinator::new(temp.path().join("worktrees"));

        // Initialize git repo
        if !init_git_repo(temp.path()) {
            return; // Skip if git not available
        }

        let root = coordinator.repo_root(temp.path()).await.unwrap();
        assert_eq!(root.canonicalize().unwrap(), temp.path().canonicalize().unwrap());
    }

    #[tokio::test]
    async fn test_setup_and_cleanup_worktree() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let worktree_base = temp.path().join("worktrees");

        std::fs::create_dir_all(&repo_path).unwrap();

        // Initialize git repo with a commit
        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return; // Skip if git not available
        }

        let mut coordinator = GitCoordinator::new(worktree_base);

        // Create worktree
        let task_id = TaskId("test-task-123".to_string());
        let worktree_path = coordinator.setup_worktree(&task_id, &repo_path).await.unwrap();

        assert!(worktree_path.exists());
        assert!(coordinator.get_worktree(&task_id).is_some());

        // Verify branch exists
        let info = coordinator.get_worktree(&task_id).unwrap();
        assert_eq!(info.branch, "neuraphage/test-task-123");

        // Cleanup worktree
        coordinator.cleanup_worktree(&task_id, false).await.unwrap();

        assert!(!worktree_path.exists());
        assert!(coordinator.get_worktree(&task_id).is_none());
    }

    #[tokio::test]
    async fn test_worktree_already_exists() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let worktree_base = temp.path().join("worktrees");

        std::fs::create_dir_all(&repo_path).unwrap();

        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return;
        }

        let mut coordinator = GitCoordinator::new(worktree_base);

        let task_id = TaskId("test-task-456".to_string());
        let _ = coordinator.setup_worktree(&task_id, &repo_path).await.unwrap();

        // Try to create same worktree again
        let result = coordinator.setup_worktree(&task_id, &repo_path).await;
        assert!(matches!(result, Err(Error::WorktreeExists { .. })));

        // Cleanup
        let _ = coordinator.cleanup_worktree(&task_id, true).await;
    }

    #[tokio::test]
    async fn test_list_worktrees() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let worktree_base = temp.path().join("worktrees");

        std::fs::create_dir_all(&repo_path).unwrap();

        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return;
        }

        let mut coordinator = GitCoordinator::new(worktree_base);

        // Create two worktrees
        let task1 = TaskId("task-1".to_string());
        let task2 = TaskId("task-2".to_string());

        let _ = coordinator.setup_worktree(&task1, &repo_path).await.unwrap();
        let _ = coordinator.setup_worktree(&task2, &repo_path).await.unwrap();

        let worktrees = coordinator.list_worktrees();
        assert_eq!(worktrees.len(), 2);

        // Cleanup
        let _ = coordinator.cleanup_worktree(&task1, true).await;
        let _ = coordinator.cleanup_worktree(&task2, true).await;
    }

    #[tokio::test]
    async fn test_save_and_load_state() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");
        let worktree_base = temp.path().join("worktrees");
        let state_file = temp.path().join("worktree_state.json");

        std::fs::create_dir_all(&repo_path).unwrap();

        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return;
        }

        let mut coordinator = GitCoordinator::new(worktree_base.clone());

        // Create worktree
        let task_id = TaskId("test-persist".to_string());
        let _ = coordinator.setup_worktree(&task_id, &repo_path).await.unwrap();

        // Save state
        coordinator.save_state(&state_file).await.unwrap();

        // Create new coordinator and load state
        let mut coordinator2 = GitCoordinator::new(worktree_base);
        coordinator2.load_state(&state_file).await.unwrap();

        assert!(coordinator2.get_worktree(&task_id).is_some());

        // Cleanup
        let _ = coordinator.cleanup_worktree(&task_id, true).await;
    }

    #[test]
    fn test_parse_worktree_list() {
        let output = "worktree /home/user/repo\nHEAD abc123\nbranch refs/heads/main\n\nworktree /home/user/.worktrees/task-1\nHEAD def456\nbranch refs/heads/neuraphage/task-1\n";

        let paths = GitCoordinator::parse_worktree_list(output);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/home/user/repo"));
        assert_eq!(paths[1], PathBuf::from("/home/user/.worktrees/task-1"));
    }
}
