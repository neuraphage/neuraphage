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

    /// Get diff between two branches for a specific file.
    pub async fn get_file_diff(&self, repo_path: &Path, file: &Path, branch: &str) -> Result<String> {
        let output = Command::new("git")
            .args(["show", &format!("{}:{}", branch, file.display())])
            .current_dir(repo_path)
            .output()
            .await?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            // File might not exist on this branch
            Ok(String::new())
        }
    }

    /// Try to rebase a branch onto a target.
    /// Returns Ok(()) if rebase succeeds, Err if conflicts occur.
    pub async fn try_rebase(&self, repo_path: &Path, branch: &str, target: &str) -> Result<()> {
        // First, checkout the branch
        let output = Command::new("git")
            .args(["checkout", branch])
            .current_dir(repo_path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::Git {
                command: "checkout".to_string(),
                stderr: stderr.to_string(),
            });
        }

        // Attempt rebase
        let output = Command::new("git")
            .args(["rebase", target])
            .current_dir(repo_path)
            .output()
            .await?;

        if output.status.success() {
            return Ok(());
        }

        // Rebase failed - abort and return error
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(repo_path)
            .output()
            .await;

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Git {
            command: "rebase".to_string(),
            stderr: stderr.to_string(),
        })
    }

    /// Check if a branch can be fast-forwarded to another.
    pub async fn can_fast_forward(&self, repo_path: &Path, from: &str, to: &str) -> Result<bool> {
        // A fast-forward is possible if `from` is an ancestor of `to`
        let output = Command::new("git")
            .args(["merge-base", "--is-ancestor", from, to])
            .current_dir(repo_path)
            .output()
            .await?;

        Ok(output.status.success())
    }

    /// Detect merge conflicts without actually merging.
    /// Uses git merge-tree to simulate the merge.
    pub async fn detect_conflicts(&self, repo_path: &Path, branch: &str, target: &str) -> Result<Vec<ConflictInfo>> {
        // Use git merge-tree to detect conflicts without modifying worktree
        let output = Command::new("git")
            .args(["merge-tree", "--write-tree", target, branch])
            .current_dir(repo_path)
            .output()
            .await?;

        if output.status.success() {
            return Ok(Vec::new()); // No conflicts
        }

        // Parse conflict information from stderr
        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(Self::parse_conflicts(&stderr))
    }

    /// Parse git merge-tree output for conflict details.
    fn parse_conflicts(output: &str) -> Vec<ConflictInfo> {
        let mut conflicts = Vec::new();
        for line in output.lines() {
            if line.starts_with("CONFLICT") {
                // Extract file and type from conflict line
                // Format: "CONFLICT (content): Merge conflict in <file>"
                let conflict_type = if line.contains("(content)") {
                    ConflictType::Content
                } else if line.contains("(modify/delete)") {
                    ConflictType::DeleteModify
                } else if line.contains("(add/add)") {
                    ConflictType::AddAdd
                } else if line.contains("(rename/rename)") {
                    ConflictType::RenameRename
                } else {
                    ConflictType::Content
                };

                if let Some(file) = line.split("Merge conflict in ").nth(1) {
                    conflicts.push(ConflictInfo {
                        file: PathBuf::from(file.trim()),
                        conflict_type,
                    });
                }
            }
        }
        conflicts
    }
}

/// Information about a merge conflict.
#[derive(Debug, Clone)]
pub struct ConflictInfo {
    /// The file with conflicts
    pub file: PathBuf,
    /// Type of conflict
    pub conflict_type: ConflictType,
}

/// Type of merge conflict.
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictType {
    /// Both modified same lines
    Content,
    /// One deleted, one modified
    DeleteModify,
    /// Both added same file
    AddAdd,
    /// Renamed to different names
    RenameRename,
}

/// A request to merge a task branch.
#[derive(Debug, Clone)]
pub struct MergeRequest {
    /// Task that produced the branch
    pub task_id: TaskId,
    /// Branch to merge
    pub branch: String,
    /// Target branch (usually main)
    pub target: String,
    /// Path to the repository
    pub repo_path: PathBuf,
    /// When the task completed
    pub completed_at: DateTime<Utc>,
}

/// Result of a merge operation.
#[derive(Debug)]
pub enum MergeResult {
    /// Merge completed successfully
    Success { task_id: TaskId },
    /// Merge had conflicts that need resolution
    Conflict {
        task_id: TaskId,
        conflicts: Vec<ConflictInfo>,
    },
    /// Merge was escalated to user
    Escalated { task_id: TaskId, reason: String },
}

/// Queue for managing branch merges.
#[derive(Debug, Default)]
pub struct MergeQueue {
    /// Tasks ready to merge, ordered by completion time
    pending: std::collections::VecDeque<MergeRequest>,
    /// Currently merging task
    active: Option<MergeRequest>,
}

impl MergeQueue {
    /// Create a new merge queue.
    pub fn new() -> Self {
        Self {
            pending: std::collections::VecDeque::new(),
            active: None,
        }
    }

    /// Add a completed task to the merge queue.
    pub fn enqueue(&mut self, request: MergeRequest) {
        self.pending.push_back(request);
    }

    /// Get the next request without removing it.
    pub fn peek(&self) -> Option<&MergeRequest> {
        if self.active.is_some() {
            None // Already processing
        } else {
            self.pending.front()
        }
    }

    /// Remove and return the next request.
    pub fn pop(&mut self) -> Option<MergeRequest> {
        self.active.take();
        self.pending.pop_front()
    }

    /// Mark a request as active (being processed).
    pub fn set_active(&mut self, request: MergeRequest) {
        self.active = Some(request);
    }

    /// Get the currently active request.
    pub fn active(&self) -> Option<&MergeRequest> {
        self.active.as_ref()
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.active.is_none()
    }

    /// Get the number of pending merges.
    pub fn len(&self) -> usize {
        self.pending.len() + if self.active.is_some() { 1 } else { 0 }
    }
}

/// Configuration for MergeCop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeCopConfig {
    /// How often to check queue (in seconds)
    #[serde(default = "default_interval")]
    pub interval_secs: u64,

    /// Auto-merge clean fast-forwards without AI
    #[serde(default = "default_auto_merge")]
    pub auto_merge_clean: bool,

    /// Max retries before escalating to user
    #[serde(default = "default_max_retries")]
    pub max_conflict_retries: usize,

    /// Enable MergeCop (can disable for manual-only merges)
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_interval() -> u64 {
    60
}
fn default_auto_merge() -> bool {
    true
}
fn default_max_retries() -> usize {
    2
}
fn default_enabled() -> bool {
    true
}

impl Default for MergeCopConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            auto_merge_clean: default_auto_merge(),
            max_conflict_retries: default_max_retries(),
            enabled: default_enabled(),
        }
    }
}

/// Decision on how to handle a merge.
#[derive(Debug)]
pub enum MergeDecision {
    /// Clean merge, no AI needed
    AutoMerge,
    /// Conflicts detected, needs resolution
    NeedsResolution { conflicts: Vec<ConflictInfo> },
    /// Cannot merge, escalate to user
    Escalate { reason: String },
}

/// Result of conflict resolution attempt.
#[derive(Debug)]
pub enum ResolutionResult {
    /// Successfully resolved all conflicts
    Resolved,
    /// Partial resolution, some conflicts remain
    Partial { remaining: Vec<ConflictInfo> },
    /// Resolution failed
    Failed { reason: String },
}

/// Context for conflict resolution.
#[derive(Debug, Clone)]
pub struct ResolutionContext {
    /// Description of what the task was trying to do
    pub task_description: String,
    /// Original task goal
    pub task_goal: Option<String>,
    /// Files the task modified
    pub modified_files: Vec<PathBuf>,
}

impl Default for ResolutionContext {
    fn default() -> Self {
        Self {
            task_description: "Unknown task".to_string(),
            task_goal: None,
            modified_files: Vec::new(),
        }
    }
}

/// MergeCop handles intelligent merge decisions.
/// Uses GC pattern for clean cases, escalates conflicts.
pub struct MergeCop {
    config: MergeCopConfig,
    /// Track resolution attempts per task
    resolution_attempts: std::collections::HashMap<TaskId, usize>,
}

impl MergeCop {
    /// Create a new MergeCop.
    pub fn new(config: MergeCopConfig) -> Self {
        Self {
            config,
            resolution_attempts: std::collections::HashMap::new(),
        }
    }

    /// Get the configuration.
    pub fn config(&self) -> &MergeCopConfig {
        &self.config
    }

    /// Get resolution attempts for a task.
    pub fn get_attempts(&self, task_id: &TaskId) -> usize {
        self.resolution_attempts.get(task_id).copied().unwrap_or(0)
    }

    /// Increment resolution attempts for a task.
    pub fn increment_attempts(&mut self, task_id: &TaskId) {
        *self.resolution_attempts.entry(task_id.clone()).or_insert(0) += 1;
    }

    /// Clear resolution attempts for a task.
    pub fn clear_attempts(&mut self, task_id: &TaskId) {
        self.resolution_attempts.remove(task_id);
    }

    /// Check if we should escalate based on retry count.
    pub fn should_escalate(&self, task_id: &TaskId) -> bool {
        self.get_attempts(task_id) >= self.config.max_conflict_retries
    }

    /// Build a conflict resolution prompt for the LLM.
    pub fn build_resolution_prompt(
        &self,
        conflict: &ConflictInfo,
        context: &ResolutionContext,
        ours: &str,
        theirs: &str,
    ) -> String {
        let mut prompt = format!(
            "You are resolving a merge conflict in `{}`.\n\n",
            conflict.file.display()
        );

        prompt.push_str(&format!("**Task Description:** {}\n\n", context.task_description));

        if let Some(goal) = &context.task_goal {
            prompt.push_str(&format!("**Task Goal:** {}\n\n", goal));
        }

        prompt.push_str(&format!("**Conflict Type:** {:?}\n\n", conflict.conflict_type));

        prompt.push_str("**Our changes (from task branch):**\n```\n");
        prompt.push_str(ours);
        prompt.push_str("\n```\n\n");

        prompt.push_str("**Their changes (from target branch):**\n```\n");
        prompt.push_str(theirs);
        prompt.push_str("\n```\n\n");

        prompt.push_str(
            "**Instructions:**\n\
             1. Analyze both versions and understand what each is trying to accomplish\n\
             2. Produce a merged version that preserves the intent of both changes\n\
             3. If the changes are incompatible, prefer the task branch changes but ensure correctness\n\
             4. Return ONLY the resolved code, no explanations\n\n\
             **Resolved code:**\n",
        );

        prompt
    }

    /// Attempt to resolve conflicts using AI assistance.
    ///
    /// This reads the conflicting files, gets both versions,
    /// asks the LLM to resolve, and applies the resolution.
    pub async fn attempt_resolution(
        &mut self,
        request: &MergeRequest,
        conflicts: &[ConflictInfo],
        _context: &ResolutionContext,
        git: &GitCoordinator,
    ) -> Result<ResolutionResult> {
        // Check if we've exceeded retry limit
        if self.should_escalate(&request.task_id) {
            return Ok(ResolutionResult::Failed {
                reason: format!("Exceeded {} resolution attempts", self.config.max_conflict_retries),
            });
        }

        self.increment_attempts(&request.task_id);

        // For now, Smart MergeCop prepares the resolution context
        // but doesn't actually call the LLM (that requires daemon integration)
        // Instead, we return a structured result that the daemon can use

        // Try rebase strategy first
        let rebase_result = git
            .try_rebase(&request.repo_path, &request.branch, &request.target)
            .await;

        if rebase_result.is_ok() {
            self.clear_attempts(&request.task_id);
            return Ok(ResolutionResult::Resolved);
        }

        // Rebase failed, conflicts remain
        // In full implementation, we'd call LLM here
        // For now, report the conflicts for external resolution

        Ok(ResolutionResult::Partial {
            remaining: conflicts.to_vec(),
        })
    }

    /// Evaluate a merge request and decide how to handle it.
    pub async fn evaluate(&self, request: &MergeRequest, git: &GitCoordinator) -> Result<MergeDecision> {
        // 1. Check if fast-forward possible (target is ancestor of branch)
        if git
            .can_fast_forward(&request.repo_path, &request.target, &request.branch)
            .await?
        {
            return Ok(MergeDecision::AutoMerge);
        }

        // 2. Try to detect conflicts without actually merging
        let conflicts = git
            .detect_conflicts(&request.repo_path, &request.branch, &request.target)
            .await?;

        if conflicts.is_empty() {
            // Clean merge commit (no fast-forward but no conflicts)
            return Ok(MergeDecision::AutoMerge);
        }

        // 3. Conflicts exist - in basic MergeCop, we escalate
        // Smart MergeCop (Phase 4) will add AI resolution here
        Ok(MergeDecision::NeedsResolution { conflicts })
    }

    /// Process the merge queue.
    /// Returns results for any completed merges.
    pub async fn process_queue(&self, queue: &mut MergeQueue, git: &GitCoordinator) -> Result<Vec<MergeResult>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        while let Some(request) = queue.peek().cloned() {
            queue.set_active(request.clone());

            let decision = self.evaluate(&request, git).await?;

            match decision {
                MergeDecision::AutoMerge => {
                    // Perform the merge
                    match git
                        .merge_branch(&request.repo_path, &request.branch, &request.target)
                        .await
                    {
                        Ok(()) => {
                            results.push(MergeResult::Success {
                                task_id: request.task_id.clone(),
                            });
                        }
                        Err(e) => {
                            // Merge failed unexpectedly - escalate
                            results.push(MergeResult::Escalated {
                                task_id: request.task_id.clone(),
                                reason: e.to_string(),
                            });
                        }
                    }
                    queue.pop();
                }
                MergeDecision::NeedsResolution { conflicts } => {
                    // In basic MergeCop, we just report the conflict
                    // Smart MergeCop will attempt AI resolution
                    results.push(MergeResult::Conflict {
                        task_id: request.task_id.clone(),
                        conflicts,
                    });
                    queue.pop();
                }
                MergeDecision::Escalate { reason } => {
                    results.push(MergeResult::Escalated {
                        task_id: request.task_id.clone(),
                        reason,
                    });
                    queue.pop();
                }
            }
        }

        Ok(results)
    }
}

// ================================
// MainWatcher - Proactive Rebase
// ================================

/// Configuration for MainWatcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MainWatcherConfig {
    /// How often to check for updates (default: 30s).
    #[serde(default = "default_main_watcher_interval")]
    pub interval_secs: u64,

    /// Name of the main branch to watch (default: "main").
    #[serde(default = "default_main_branch")]
    pub main_branch: String,

    /// Enable main watching (can disable for manual-only rebases).
    #[serde(default = "default_main_watcher_enabled")]
    pub enabled: bool,

    /// Rebase cooldown in seconds (minimum time between rebases for same task).
    #[serde(default = "default_rebase_cooldown")]
    pub rebase_cooldown_secs: u64,
}

fn default_main_watcher_interval() -> u64 {
    30
}
fn default_main_branch() -> String {
    "main".to_string()
}
fn default_main_watcher_enabled() -> bool {
    false // Start disabled, enable after validation
}
fn default_rebase_cooldown() -> u64 {
    60
}

impl Default for MainWatcherConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_main_watcher_interval(),
            main_branch: default_main_branch(),
            enabled: default_main_watcher_enabled(),
            rebase_cooldown_secs: default_rebase_cooldown(),
        }
    }
}

/// Information about a main branch update.
#[derive(Debug, Clone)]
pub struct MainUpdate {
    /// Path to the repository.
    pub repo_path: PathBuf,
    /// Previous commit hash (if known).
    pub previous_commit: Option<String>,
    /// New commit hash on main.
    pub new_commit: String,
    /// Branch name that was updated.
    pub branch: String,
}

/// Basic commit information.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    /// Full commit SHA.
    pub sha: String,
    /// Commit message (first line).
    pub message: String,
}

/// Result of a rebase operation.
#[derive(Debug, Clone)]
pub enum RebaseResult {
    /// Rebase completed successfully.
    Success {
        /// Previous HEAD before rebase.
        previous_head: String,
        /// New HEAD after rebase.
        new_head: String,
    },
    /// Rebase encountered conflicts.
    Conflict {
        /// Details about the conflict.
        details: String,
        /// Files with conflicts.
        conflicting_files: Vec<PathBuf>,
    },
    /// Rebase failed (not due to conflicts).
    Failed {
        /// Reason for failure.
        reason: String,
    },
    /// Skipped (e.g., cooldown not elapsed).
    Skipped {
        /// Reason for skipping.
        reason: String,
    },
}

/// Watches the main branch for new commits.
pub struct MainWatcher {
    config: MainWatcherConfig,
    /// Last known commit on main for each tracked repo.
    last_commits: HashMap<PathBuf, String>,
    /// Last rebase time per task (for cooldown).
    last_rebase_times: HashMap<TaskId, DateTime<Utc>>,
    /// Track repos that have been fetched at least once.
    fetched_repos: std::collections::HashSet<PathBuf>,
}

impl MainWatcher {
    /// Create a new MainWatcher.
    pub fn new(config: MainWatcherConfig) -> Self {
        Self {
            config,
            last_commits: HashMap::new(),
            last_rebase_times: HashMap::new(),
            fetched_repos: std::collections::HashSet::new(),
        }
    }

    /// Get the configuration.
    pub fn config(&self) -> &MainWatcherConfig {
        &self.config
    }

    /// Clear cached state (e.g., on daemon restart).
    pub fn clear_cache(&mut self) {
        self.last_commits.clear();
        self.fetched_repos.clear();
        // Don't clear last_rebase_times - we want cooldown to persist
    }

    /// Fetch from remote if not already done for this repo.
    async fn ensure_fetched(&mut self, repo_path: &Path) -> Result<()> {
        let key = repo_path.to_path_buf();
        if self.fetched_repos.contains(&key) {
            return Ok(());
        }

        let output = Command::new("git")
            .args(["fetch", "origin", &self.config.main_branch])
            .current_dir(repo_path)
            .output()
            .await?;

        if output.status.success() {
            self.fetched_repos.insert(key);
        }
        // Don't fail if fetch fails - might be offline
        Ok(())
    }

    /// Check if main has new commits in a repository.
    pub async fn check_for_updates(&mut self, repo_path: &Path) -> Result<Option<MainUpdate>> {
        // Ensure we've fetched at least once
        self.ensure_fetched(repo_path).await?;

        // Get current HEAD of origin/main
        let output = Command::new("git")
            .args(["rev-parse", &format!("origin/{}", self.config.main_branch)])
            .current_dir(repo_path)
            .output()
            .await?;

        if !output.status.success() {
            return Ok(None); // Remote might not exist yet
        }

        let current_commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        // Compare to last known
        let repo_key = repo_path.to_path_buf();
        if let Some(last) = self.last_commits.get(&repo_key)
            && last == &current_commit
        {
            return Ok(None); // No change
        }

        // Update cached value
        let previous = self.last_commits.insert(repo_key.clone(), current_commit.clone());

        Ok(Some(MainUpdate {
            repo_path: repo_key,
            previous_commit: previous,
            new_commit: current_commit,
            branch: self.config.main_branch.clone(),
        }))
    }

    /// Force a fetch from remote for a repository.
    pub async fn fetch_updates(&mut self, repo_path: &Path) -> Result<bool> {
        let output = Command::new("git")
            .args(["fetch", "origin", &self.config.main_branch])
            .current_dir(repo_path)
            .output()
            .await?;

        self.fetched_repos.insert(repo_path.to_path_buf());
        Ok(output.status.success())
    }

    /// Check if a task branch is behind main.
    pub async fn is_behind_main(&self, repo_path: &Path, branch: &str) -> Result<bool> {
        let main_ref = format!("origin/{}", self.config.main_branch);

        // Check if main is an ancestor of branch
        // If main is NOT an ancestor, the branch is behind (or diverged)
        let output = Command::new("git")
            .args(["merge-base", "--is-ancestor", &main_ref, branch])
            .current_dir(repo_path)
            .output()
            .await?;

        // Exit 0 = main IS ancestor = branch is up to date
        // Exit 1 = main is NOT ancestor = branch needs rebase
        Ok(!output.status.success())
    }

    /// Get the number of commits a branch is behind main.
    pub async fn commits_behind(&self, repo_path: &Path, branch: &str) -> Result<usize> {
        let main_ref = format!("origin/{}", self.config.main_branch);

        let output = Command::new("git")
            .args(["rev-list", "--count", &format!("{}..{}", branch, main_ref)])
            .current_dir(repo_path)
            .output()
            .await?;

        if !output.status.success() {
            return Ok(0);
        }

        let count_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(count_str.parse().unwrap_or(0))
    }

    /// Get list of commits on main since branch diverged.
    pub async fn commits_since_divergence(&self, repo_path: &Path, branch: &str) -> Result<Vec<CommitInfo>> {
        let main_ref = format!("origin/{}", self.config.main_branch);

        // Find merge base
        let base_output = Command::new("git")
            .args(["merge-base", &main_ref, branch])
            .current_dir(repo_path)
            .output()
            .await?;

        if !base_output.status.success() {
            return Ok(Vec::new());
        }

        let base = String::from_utf8_lossy(&base_output.stdout).trim().to_string();

        // Get commits from base to main
        let log_output = Command::new("git")
            .args(["log", "--oneline", "--format=%H|%s", &format!("{}..{}", base, main_ref)])
            .current_dir(repo_path)
            .output()
            .await?;

        let commits = String::from_utf8_lossy(&log_output.stdout)
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(2, '|').collect();
                if parts.len() == 2 {
                    Some(CommitInfo {
                        sha: parts[0].to_string(),
                        message: parts[1].to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(commits)
    }

    /// Check if cooldown has elapsed for a task.
    pub fn cooldown_elapsed(&self, task_id: &TaskId) -> bool {
        if let Some(last_time) = self.last_rebase_times.get(task_id) {
            let elapsed = Utc::now().signed_duration_since(*last_time);
            elapsed.num_seconds() >= self.config.rebase_cooldown_secs as i64
        } else {
            true // No previous rebase, cooldown elapsed
        }
    }

    /// Record that a task was rebased.
    pub fn record_rebase(&mut self, task_id: &TaskId) {
        self.last_rebase_times.insert(task_id.clone(), Utc::now());
    }

    /// Perform a rebase of a worktree against main.
    ///
    /// This handles:
    /// 1. Checking cooldown
    /// 2. Stashing uncommitted changes
    /// 3. Fetching latest
    /// 4. Rebasing
    /// 5. Unstashing changes
    /// 6. Aborting if conflicts occur
    pub async fn perform_rebase(&mut self, task_id: &TaskId, worktree_path: &Path) -> Result<RebaseResult> {
        // Check cooldown
        if !self.cooldown_elapsed(task_id) {
            return Ok(RebaseResult::Skipped {
                reason: "Cooldown not elapsed".to_string(),
            });
        }

        // Get current HEAD before rebase
        let head_before = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(worktree_path)
            .output()
            .await?;

        let previous_head = String::from_utf8_lossy(&head_before.stdout).trim().to_string();

        // Check for uncommitted changes and stash if needed
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree_path)
            .output()
            .await?;

        let has_changes = !String::from_utf8_lossy(&status.stdout).trim().is_empty();
        let mut stashed = false;

        if has_changes {
            let stash = Command::new("git")
                .args(["stash", "push", "-m", "neuraphage-rebase-autostash"])
                .current_dir(worktree_path)
                .output()
                .await?;

            stashed = stash.status.success();
        }

        // Fetch latest
        let fetch = Command::new("git")
            .args(["fetch", "origin", &self.config.main_branch])
            .current_dir(worktree_path)
            .output()
            .await?;

        if !fetch.status.success() {
            // Unstash if we stashed
            if stashed {
                let _ = Command::new("git")
                    .args(["stash", "pop"])
                    .current_dir(worktree_path)
                    .output()
                    .await;
            }
            return Ok(RebaseResult::Failed {
                reason: format!("Failed to fetch origin/{}", self.config.main_branch),
            });
        }

        // Attempt rebase
        let main_ref = format!("origin/{}", self.config.main_branch);
        let rebase = Command::new("git")
            .args(["rebase", &main_ref])
            .current_dir(worktree_path)
            .output()
            .await?;

        if rebase.status.success() {
            // Get new HEAD
            let head_after = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(worktree_path)
                .output()
                .await?;

            let new_head = String::from_utf8_lossy(&head_after.stdout).trim().to_string();

            // Unstash if we stashed
            if stashed {
                let _ = Command::new("git")
                    .args(["stash", "pop"])
                    .current_dir(worktree_path)
                    .output()
                    .await;
            }

            // Record successful rebase
            self.record_rebase(task_id);

            return Ok(RebaseResult::Success {
                previous_head,
                new_head,
            });
        }

        // Rebase failed - likely conflicts
        let stderr = String::from_utf8_lossy(&rebase.stderr);

        // Get conflicting files
        let diff_output = Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(worktree_path)
            .output()
            .await?;

        let conflicting_files: Vec<PathBuf> = String::from_utf8_lossy(&diff_output.stdout)
            .lines()
            .map(PathBuf::from)
            .collect();

        // Abort the failed rebase
        let _ = Command::new("git")
            .args(["rebase", "--abort"])
            .current_dir(worktree_path)
            .output()
            .await;

        // Unstash if we stashed
        if stashed {
            let _ = Command::new("git")
                .args(["stash", "pop"])
                .current_dir(worktree_path)
                .output()
                .await;
        }

        Ok(RebaseResult::Conflict {
            details: stderr.to_string(),
            conflicting_files,
        })
    }

    /// Get rebase status for a task.
    pub async fn get_rebase_status(&self, task_id: &TaskId, worktree: &WorktreeInfo) -> Result<TaskRebaseStatus> {
        let behind = self.commits_behind(&worktree.repo_path, &worktree.branch).await?;

        Ok(TaskRebaseStatus {
            task_id: task_id.0.clone(),
            branch: worktree.branch.clone(),
            commits_behind: behind,
            last_rebased: self.last_rebase_times.get(task_id).copied(),
        })
    }
}

/// Rebase status for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRebaseStatus {
    /// Task ID.
    pub task_id: String,
    /// Branch name.
    pub branch: String,
    /// Number of commits behind main.
    pub commits_behind: usize,
    /// Last successful rebase time.
    pub last_rebased: Option<DateTime<Utc>>,
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

    #[test]
    fn test_merge_queue_basic() {
        let mut queue = MergeQueue::new();
        assert!(queue.is_empty());
        assert_eq!(queue.len(), 0);

        // Enqueue a request
        let request = MergeRequest {
            task_id: TaskId("task-1".to_string()),
            branch: "neuraphage/task-1".to_string(),
            target: "main".to_string(),
            repo_path: PathBuf::from("/tmp/repo"),
            completed_at: Utc::now(),
        };
        queue.enqueue(request);

        assert!(!queue.is_empty());
        assert_eq!(queue.len(), 1);
        assert!(queue.peek().is_some());
        assert_eq!(queue.peek().unwrap().task_id.0, "task-1");

        // Pop the request
        let popped = queue.pop();
        assert!(popped.is_some());
        assert!(queue.is_empty());
    }

    #[test]
    fn test_merge_queue_ordering() {
        let mut queue = MergeQueue::new();

        // Enqueue multiple requests
        for i in 1..=3 {
            queue.enqueue(MergeRequest {
                task_id: TaskId(format!("task-{}", i)),
                branch: format!("neuraphage/task-{}", i),
                target: "main".to_string(),
                repo_path: PathBuf::from("/tmp/repo"),
                completed_at: Utc::now(),
            });
        }

        assert_eq!(queue.len(), 3);

        // FIFO order
        assert_eq!(queue.pop().unwrap().task_id.0, "task-1");
        assert_eq!(queue.pop().unwrap().task_id.0, "task-2");
        assert_eq!(queue.pop().unwrap().task_id.0, "task-3");
        assert!(queue.is_empty());
    }

    #[test]
    fn test_merge_queue_active() {
        let mut queue = MergeQueue::new();

        let request = MergeRequest {
            task_id: TaskId("task-1".to_string()),
            branch: "neuraphage/task-1".to_string(),
            target: "main".to_string(),
            repo_path: PathBuf::from("/tmp/repo"),
            completed_at: Utc::now(),
        };
        queue.enqueue(request.clone());

        // Set active
        queue.set_active(request);
        assert!(queue.active().is_some());

        // Peek returns None when active is set
        assert!(queue.peek().is_none());

        // Pop clears active
        queue.pop();
        assert!(queue.active().is_none());
    }

    #[test]
    fn test_mergecop_config_default() {
        let config = MergeCopConfig::default();
        assert_eq!(config.interval_secs, 60);
        assert!(config.auto_merge_clean);
        assert_eq!(config.max_conflict_retries, 2);
        assert!(config.enabled);
    }

    #[test]
    fn test_parse_conflicts() {
        // Only lines with "Merge conflict in" are parsed
        let output =
            "CONFLICT (content): Merge conflict in src/main.rs\nCONFLICT (add/add): Merge conflict in src/new.rs\n";

        let conflicts = GitCoordinator::parse_conflicts(output);
        assert_eq!(conflicts.len(), 2);

        assert_eq!(conflicts[0].file, PathBuf::from("src/main.rs"));
        assert_eq!(conflicts[0].conflict_type, ConflictType::Content);

        assert_eq!(conflicts[1].file, PathBuf::from("src/new.rs"));
        assert_eq!(conflicts[1].conflict_type, ConflictType::AddAdd);
    }

    #[test]
    fn test_conflict_type_equality() {
        assert_eq!(ConflictType::Content, ConflictType::Content);
        assert_ne!(ConflictType::Content, ConflictType::DeleteModify);
    }

    #[test]
    fn test_resolution_context_default() {
        let ctx = ResolutionContext::default();
        assert_eq!(ctx.task_description, "Unknown task");
        assert!(ctx.task_goal.is_none());
        assert!(ctx.modified_files.is_empty());
    }

    #[test]
    fn test_mergecop_attempt_tracking() {
        let mut cop = MergeCop::new(MergeCopConfig::default());
        let task_id = TaskId("test-task".to_string());

        assert_eq!(cop.get_attempts(&task_id), 0);
        assert!(!cop.should_escalate(&task_id));

        cop.increment_attempts(&task_id);
        assert_eq!(cop.get_attempts(&task_id), 1);

        cop.increment_attempts(&task_id);
        assert_eq!(cop.get_attempts(&task_id), 2);
        assert!(cop.should_escalate(&task_id)); // Default max is 2

        cop.clear_attempts(&task_id);
        assert_eq!(cop.get_attempts(&task_id), 0);
    }

    #[test]
    fn test_build_resolution_prompt() {
        let cop = MergeCop::new(MergeCopConfig::default());
        let conflict = ConflictInfo {
            file: PathBuf::from("src/main.rs"),
            conflict_type: ConflictType::Content,
        };
        let context = ResolutionContext {
            task_description: "Add user authentication".to_string(),
            task_goal: Some("Implement login flow".to_string()),
            modified_files: vec![PathBuf::from("src/auth.rs")],
        };

        let prompt = cop.build_resolution_prompt(&conflict, &context, "our code", "their code");

        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("Add user authentication"));
        assert!(prompt.contains("Implement login flow"));
        assert!(prompt.contains("our code"));
        assert!(prompt.contains("their code"));
        assert!(prompt.contains("Content"));
    }

    // MainWatcher tests

    #[test]
    fn test_main_watcher_config_default() {
        let config = MainWatcherConfig::default();
        assert_eq!(config.interval_secs, 30);
        assert_eq!(config.main_branch, "main");
        assert!(!config.enabled); // Disabled by default
        assert_eq!(config.rebase_cooldown_secs, 60);
    }

    #[test]
    fn test_main_watcher_new() {
        let config = MainWatcherConfig::default();
        let watcher = MainWatcher::new(config.clone());
        assert_eq!(watcher.config().interval_secs, config.interval_secs);
        assert_eq!(watcher.config().main_branch, config.main_branch);
    }

    #[test]
    fn test_main_watcher_cooldown() {
        let config = MainWatcherConfig {
            rebase_cooldown_secs: 60,
            ..Default::default()
        };
        let mut watcher = MainWatcher::new(config);
        let task_id = TaskId("test-cooldown".to_string());

        // Initially cooldown is elapsed (no previous rebase)
        assert!(watcher.cooldown_elapsed(&task_id));

        // Record a rebase
        watcher.record_rebase(&task_id);

        // Cooldown not elapsed (just rebased)
        assert!(!watcher.cooldown_elapsed(&task_id));

        // Manually set to past time
        let past = Utc::now() - chrono::Duration::seconds(120);
        watcher.last_rebase_times.insert(task_id.clone(), past);

        // Cooldown should now be elapsed
        assert!(watcher.cooldown_elapsed(&task_id));
    }

    #[test]
    fn test_main_watcher_clear_cache() {
        let config = MainWatcherConfig::default();
        let mut watcher = MainWatcher::new(config);

        // Add some cached data
        watcher
            .last_commits
            .insert(PathBuf::from("/tmp/repo1"), "abc123".to_string());
        watcher.fetched_repos.insert(PathBuf::from("/tmp/repo1"));

        let task_id = TaskId("test".to_string());
        watcher.record_rebase(&task_id);

        // Clear cache
        watcher.clear_cache();

        // Commits and fetched repos should be cleared
        assert!(watcher.last_commits.is_empty());
        assert!(watcher.fetched_repos.is_empty());

        // But rebase times should persist
        assert!(watcher.last_rebase_times.contains_key(&task_id));
    }

    #[tokio::test]
    async fn test_main_watcher_check_for_updates() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");

        std::fs::create_dir_all(&repo_path).unwrap();

        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return; // Skip if git not available
        }

        let config = MainWatcherConfig::default();
        let mut watcher = MainWatcher::new(config);

        // First check should return None (no origin/main yet - local repo only)
        let result = watcher.check_for_updates(&repo_path).await;
        // This will likely return None since there's no remote
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_main_watcher_is_behind_main() {
        let temp = TempDir::new().unwrap();
        let repo_path = temp.path().join("repo");

        std::fs::create_dir_all(&repo_path).unwrap();

        if !init_git_repo(&repo_path) || !create_initial_commit(&repo_path) {
            return;
        }

        let config = MainWatcherConfig {
            main_branch: "main".to_string(),
            ..Default::default()
        };
        let watcher = MainWatcher::new(config);

        // Without a remote, this will return an error or false
        // The important thing is it doesn't crash
        let _result = watcher.is_behind_main(&repo_path, "main").await;
    }

    #[test]
    fn test_task_rebase_status() {
        let status = TaskRebaseStatus {
            task_id: "task-123".to_string(),
            branch: "neuraphage/task-123".to_string(),
            commits_behind: 3,
            last_rebased: Some(Utc::now()),
        };

        assert_eq!(status.task_id, "task-123");
        assert_eq!(status.commits_behind, 3);
        assert!(status.last_rebased.is_some());
    }

    #[test]
    fn test_rebase_result_variants() {
        // Success
        let success = RebaseResult::Success {
            previous_head: "abc123".to_string(),
            new_head: "def456".to_string(),
        };
        assert!(matches!(success, RebaseResult::Success { .. }));

        // Conflict
        let conflict = RebaseResult::Conflict {
            details: "CONFLICT in foo.rs".to_string(),
            conflicting_files: vec![PathBuf::from("foo.rs")],
        };
        assert!(matches!(conflict, RebaseResult::Conflict { .. }));

        // Failed
        let failed = RebaseResult::Failed {
            reason: "Network error".to_string(),
        };
        assert!(matches!(failed, RebaseResult::Failed { .. }));

        // Skipped
        let skipped = RebaseResult::Skipped {
            reason: "Cooldown not elapsed".to_string(),
        };
        assert!(matches!(skipped, RebaseResult::Skipped { .. }));
    }

    #[test]
    fn test_main_update() {
        let update = MainUpdate {
            repo_path: PathBuf::from("/tmp/repo"),
            previous_commit: Some("abc123".to_string()),
            new_commit: "def456".to_string(),
            branch: "main".to_string(),
        };

        assert_eq!(update.repo_path, PathBuf::from("/tmp/repo"));
        assert_eq!(update.previous_commit, Some("abc123".to_string()));
        assert_eq!(update.new_commit, "def456");
        assert_eq!(update.branch, "main");
    }

    #[test]
    fn test_commit_info() {
        let info = CommitInfo {
            sha: "abc123def456".to_string(),
            message: "feat: add new feature".to_string(),
        };

        assert_eq!(info.sha, "abc123def456");
        assert_eq!(info.message, "feat: add new feature");
    }
}
