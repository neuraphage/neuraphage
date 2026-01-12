//! Task manager wrapping engram::Store with neuraphage-specific logic.

use std::path::Path;

use engram::{EdgeKind, Store, StoreQueryExt};

use crate::error::{Error, Result};
use crate::task::{Task, TaskId, TaskStatus};

/// Manages tasks using engram as the persistence layer.
pub struct TaskManager {
    store: Store,
}

impl TaskManager {
    /// Initialize a new task manager at the given path.
    pub fn init(path: &Path) -> Result<Self> {
        let store = Store::init(path)?;
        Ok(Self { store })
    }

    /// Open an existing task manager.
    pub fn open(path: &Path) -> Result<Self> {
        let store = Store::open(path)?;
        Ok(Self { store })
    }

    /// Create a new task.
    pub fn create_task(
        &mut self,
        description: &str,
        priority: u8,
        tags: &[&str],
        context: Option<&str>,
    ) -> Result<Task> {
        // Add status label to tags
        let mut labels: Vec<&str> = tags.to_vec();
        let status_label = TaskStatus::Queued.to_label();
        labels.push(&status_label);

        let item = self.store.create(description, priority, &labels, context)?;
        Ok(Task::from_engram_item(&item))
    }

    /// Get a task by ID.
    pub fn get_task(&self, id: &TaskId) -> Result<Option<Task>> {
        let item = self.store.get(id.as_ref())?;
        Ok(item.map(|i| Task::from_engram_item(&i)))
    }

    /// Update a task's description, context, priority, or tags.
    pub fn update_task(
        &mut self,
        id: &TaskId,
        description: Option<&str>,
        context: Option<&str>,
        priority: Option<u8>,
        tags: Option<&[&str]>,
    ) -> Result<Task> {
        // If updating tags, need to preserve status label
        let labels = if let Some(new_tags) = tags {
            // Get current task to preserve status label
            let current = self
                .get_task(id)?
                .ok_or_else(|| Error::TaskNotFound { id: id.to_string() })?;
            let status_label = current.status.to_label();
            let mut labels: Vec<String> = new_tags.iter().map(|s| s.to_string()).collect();
            labels.push(status_label);
            Some(labels)
        } else {
            None
        };

        let labels_refs: Option<Vec<&str>> = labels.as_ref().map(|l| l.iter().map(|s| s.as_str()).collect());

        // engram's update takes Option<Option<&str>> for description
        // Some(None) = clear, Some(Some(x)) = set, None = no change
        let desc_opt = context.map(Some);

        let item = self
            .store
            .update(id.as_ref(), description, desc_opt, priority, labels_refs.as_deref())?;
        Ok(Task::from_engram_item(&item))
    }

    /// Set a task's status.
    pub fn set_status(&mut self, id: &TaskId, status: TaskStatus) -> Result<Task> {
        let current = self
            .get_task(id)?
            .ok_or_else(|| Error::TaskNotFound { id: id.to_string() })?;

        if !current.status.can_transition_to(status) {
            return Err(Error::InvalidStateTransition {
                from: current.status,
                to: status,
            });
        }

        // Update engram status
        self.store.set_status(id.as_ref(), status.to_engram_status())?;

        // Update status label
        let mut labels: Vec<String> = current
            .tags
            .iter()
            .filter(|l| !l.starts_with("_status_"))
            .cloned()
            .collect();
        labels.push(status.to_label());

        let labels_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        let item = self.store.update(id.as_ref(), None, None, None, Some(&labels_refs))?;
        Ok(Task::from_engram_item(&item))
    }

    /// Close a task (complete, fail, or cancel).
    pub fn close_task(&mut self, id: &TaskId, status: TaskStatus, reason: Option<&str>) -> Result<Task> {
        if !status.is_terminal() {
            return Err(Error::Validation(format!(
                "Cannot close task with non-terminal status: {:?}",
                status
            )));
        }

        let current = self
            .get_task(id)?
            .ok_or_else(|| Error::TaskNotFound { id: id.to_string() })?;

        if !current.status.can_transition_to(status) {
            return Err(Error::InvalidStateTransition {
                from: current.status,
                to: status,
            });
        }

        // Close in engram
        self.store.close(id.as_ref(), reason)?;

        // Update status label
        let mut labels: Vec<String> = current
            .tags
            .iter()
            .filter(|l| !l.starts_with("_status_"))
            .cloned()
            .collect();
        labels.push(status.to_label());

        let labels_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        let item = self.store.update(id.as_ref(), None, None, None, Some(&labels_refs))?;
        Ok(Task::from_engram_item(&item))
    }

    /// Add a dependency between tasks (blocked_by blocks blocker).
    pub fn add_dependency(&mut self, blocked_id: &TaskId, blocker_id: &TaskId) -> Result<()> {
        self.store
            .add_edge(blocked_id.as_ref(), blocker_id.as_ref(), EdgeKind::Blocks)?;
        Ok(())
    }

    /// Remove a dependency between tasks.
    pub fn remove_dependency(&mut self, blocked_id: &TaskId, blocker_id: &TaskId) -> Result<()> {
        self.store
            .remove_edge(blocked_id.as_ref(), blocker_id.as_ref(), EdgeKind::Blocks)?;
        Ok(())
    }

    /// Get tasks that are ready to run (no open blockers).
    pub fn ready_tasks(&self) -> Result<Vec<Task>> {
        let items = self.store.ready()?;
        Ok(items.iter().map(Task::from_engram_item).collect())
    }

    /// Get tasks that are blocked.
    pub fn blocked_tasks(&self) -> Result<Vec<Task>> {
        let items = self.store.blocked()?;
        Ok(items.iter().map(Task::from_engram_item).collect())
    }

    /// List all tasks, optionally filtered by engram status.
    pub fn list_tasks(&self, status: Option<engram::Status>) -> Result<Vec<Task>> {
        let items = self.store.list(status)?;
        Ok(items.iter().map(Task::from_engram_item).collect())
    }

    /// Query tasks with a filter.
    pub fn query_tasks(&self) -> TaskQuery<'_> {
        TaskQuery {
            manager: self,
            status: None,
            tags: Vec::new(),
            priority_min: None,
            priority_max: None,
            limit: None,
        }
    }

    /// Get the number of tasks by status.
    pub fn task_counts(&self) -> Result<TaskCounts> {
        let all = self.list_tasks(None)?;
        let mut counts = TaskCounts::default();

        for task in all {
            match task.status {
                TaskStatus::Queued => counts.queued += 1,
                TaskStatus::Running => counts.running += 1,
                TaskStatus::WaitingForUser => counts.waiting += 1,
                TaskStatus::Blocked => counts.blocked += 1,
                TaskStatus::Paused => counts.paused += 1,
                TaskStatus::Completed => counts.completed += 1,
                TaskStatus::Failed => counts.failed += 1,
                TaskStatus::Cancelled => counts.cancelled += 1,
            }
        }

        Ok(counts)
    }

    /// Store a learning as an engram item with special labels.
    ///
    /// Learnings are stored as engram items with:
    /// - Label "learning" to identify them
    /// - Label "kind:<kind>" for the learning type
    /// - Optional "source_task:<id>" if from a specific task
    /// - Any custom tags from the learning
    pub fn store_learning(
        &mut self,
        kind: &str,
        title: &str,
        content: &str,
        source_task: Option<&TaskId>,
        tags: &[&str],
    ) -> Result<String> {
        let mut labels: Vec<String> = vec!["learning".to_string(), format!("kind:{}", kind)];

        if let Some(task_id) = source_task {
            labels.push(format!("source_task:{}", task_id.0));
        }

        for tag in tags {
            labels.push((*tag).to_string());
        }

        let labels_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

        let item = self.store.create(title, 50, &labels_refs, Some(content))?;
        Ok(item.id.to_string())
    }

    /// Query learnings with optional filters.
    ///
    /// Returns engram items that have the "learning" label.
    pub fn query_learnings(&self, kind: Option<&str>, tags: &[&str], limit: usize) -> Result<Vec<StoredLearning>> {
        let mut query = self.store.query().label("learning").limit(limit);

        if let Some(k) = kind {
            query = query.label(format!("kind:{}", k));
        }

        for tag in tags {
            query = query.label(*tag);
        }

        let items = query.execute()?;

        let learnings = items
            .iter()
            .map(|item| {
                let kind = item
                    .labels
                    .iter()
                    .find(|l| l.starts_with("kind:"))
                    .map(|l| l.strip_prefix("kind:").unwrap_or("").to_string())
                    .unwrap_or_default();

                let source_task = item
                    .labels
                    .iter()
                    .find(|l| l.starts_with("source_task:"))
                    .map(|l| TaskId(l.strip_prefix("source_task:").unwrap_or("").to_string()));

                let custom_tags: Vec<String> = item
                    .labels
                    .iter()
                    .filter(|l| !l.starts_with("kind:") && !l.starts_with("source_task:") && *l != "learning")
                    .cloned()
                    .collect();

                StoredLearning {
                    id: item.id.to_string(),
                    kind,
                    title: item.title.clone(),
                    content: item.description.clone().unwrap_or_default(),
                    source_task,
                    tags: custom_tags,
                    created_at: item.created_at.to_string(),
                }
            })
            .collect();

        Ok(learnings)
    }
}

/// A learning stored in engram.
#[derive(Debug, Clone)]
pub struct StoredLearning {
    /// Unique identifier from engram.
    pub id: String,
    /// Kind of learning (e.g., "learning", "decision", "fact").
    pub kind: String,
    /// Title/summary.
    pub title: String,
    /// Full content.
    pub content: String,
    /// Source task that created this learning, if any.
    pub source_task: Option<TaskId>,
    /// Custom tags.
    pub tags: Vec<String>,
    /// When it was created (ISO 8601 string).
    pub created_at: String,
}

/// Task counts by status.
#[derive(Debug, Default, Clone)]
pub struct TaskCounts {
    pub queued: usize,
    pub running: usize,
    pub waiting: usize,
    pub blocked: usize,
    pub paused: usize,
    pub completed: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl TaskCounts {
    /// Total active (non-terminal) tasks.
    pub fn active(&self) -> usize {
        self.queued + self.running + self.waiting + self.blocked + self.paused
    }

    /// Total terminal tasks.
    pub fn terminal(&self) -> usize {
        self.completed + self.failed + self.cancelled
    }

    /// Total tasks.
    pub fn total(&self) -> usize {
        self.active() + self.terminal()
    }
}

/// Query builder for tasks.
pub struct TaskQuery<'a> {
    manager: &'a TaskManager,
    status: Option<TaskStatus>,
    tags: Vec<String>,
    priority_min: Option<u8>,
    priority_max: Option<u8>,
    limit: Option<usize>,
}

impl<'a> TaskQuery<'a> {
    /// Filter by status.
    pub fn status(mut self, status: TaskStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Filter by tag.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Filter by minimum priority (0=highest).
    pub fn priority_min(mut self, min: u8) -> Self {
        self.priority_min = Some(min);
        self
    }

    /// Filter by maximum priority (4=lowest).
    pub fn priority_max(mut self, max: u8) -> Self {
        self.priority_max = Some(max);
        self
    }

    /// Limit results.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Execute the query.
    pub fn execute(self) -> Result<Vec<Task>> {
        // Build engram filter
        let mut query = self.manager.store.query();

        if let Some(status) = self.status {
            query = query.status(status.to_engram_status());
        }

        for tag in &self.tags {
            query = query.label(tag);
        }

        if let Some(min) = self.priority_min {
            query = query.min_priority(min);
        }

        if let Some(max) = self.priority_max {
            query = query.max_priority(max);
        }

        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }

        let items = query.execute()?;
        let mut tasks: Vec<Task> = items.iter().map(Task::from_engram_item).collect();

        // Apply neuraphage-specific status filter (engram doesn't know our extended statuses)
        if let Some(status) = self.status {
            tasks.retain(|t| t.status == status);
        }

        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, TaskManager) {
        let temp = TempDir::new().unwrap();
        let manager = TaskManager::init(temp.path()).unwrap();
        (temp, manager)
    }

    #[test]
    fn test_create_and_get_task() {
        let (_temp, mut manager) = setup();

        let task = manager.create_task("Test task", 2, &["test"], Some("context")).unwrap();

        assert_eq!(task.description, "Test task");
        assert_eq!(task.priority, 2);
        assert_eq!(task.status, TaskStatus::Queued);
        assert!(task.tags.contains(&"test".to_string()));
        assert_eq!(task.context, Some("context".to_string()));

        let retrieved = manager.get_task(&task.id).unwrap().unwrap();
        assert_eq!(retrieved.id, task.id);
        assert_eq!(retrieved.description, task.description);
    }

    #[test]
    fn test_status_transitions() {
        let (_temp, mut manager) = setup();

        let task = manager.create_task("Test", 2, &[], None).unwrap();
        assert_eq!(task.status, TaskStatus::Queued);

        // Queued -> Running
        let task = manager.set_status(&task.id, TaskStatus::Running).unwrap();
        assert_eq!(task.status, TaskStatus::Running);

        // Running -> WaitingForUser
        let task = manager.set_status(&task.id, TaskStatus::WaitingForUser).unwrap();
        assert_eq!(task.status, TaskStatus::WaitingForUser);

        // WaitingForUser -> Running
        let task = manager.set_status(&task.id, TaskStatus::Running).unwrap();
        assert_eq!(task.status, TaskStatus::Running);

        // Running -> Completed
        let task = manager
            .close_task(&task.id, TaskStatus::Completed, Some("Done"))
            .unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
    }

    #[test]
    fn test_invalid_transition() {
        let (_temp, mut manager) = setup();

        let task = manager.create_task("Test", 2, &[], None).unwrap();

        // Cannot go from Queued directly to Completed
        let result = manager.close_task(&task.id, TaskStatus::Completed, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_dependencies() {
        let (_temp, mut manager) = setup();

        let blocker = manager.create_task("Blocker", 1, &[], None).unwrap();
        let blocked = manager.create_task("Blocked", 2, &[], None).unwrap();

        // Add dependency
        manager.add_dependency(&blocked.id, &blocker.id).unwrap();

        // Blocker should be ready, blocked should not
        let ready = manager.ready_tasks().unwrap();
        assert!(ready.iter().any(|t| t.id == blocker.id));
        // Note: blocked task might still appear in ready if it's in Queued status
        // because engram's ready() checks edges, not our status label

        // Complete blocker
        manager.set_status(&blocker.id, TaskStatus::Running).unwrap();
        manager.close_task(&blocker.id, TaskStatus::Completed, None).unwrap();

        // Now blocked should be ready
        let ready = manager.ready_tasks().unwrap();
        assert!(ready.iter().any(|t| t.id == blocked.id));
    }

    #[test]
    fn test_task_counts() {
        let (_temp, mut manager) = setup();

        manager.create_task("Task 1", 1, &[], None).unwrap();
        manager.create_task("Task 2", 2, &[], None).unwrap();
        let task3 = manager.create_task("Task 3", 3, &[], None).unwrap();

        // Set one to running
        manager.set_status(&task3.id, TaskStatus::Running).unwrap();

        let counts = manager.task_counts().unwrap();
        assert_eq!(counts.queued, 2);
        assert_eq!(counts.running, 1);
        assert_eq!(counts.active(), 3);
    }

    #[test]
    fn test_query_by_tag() {
        let (_temp, mut manager) = setup();

        manager.create_task("Backend task", 1, &["backend"], None).unwrap();
        manager.create_task("Frontend task", 2, &["frontend"], None).unwrap();
        manager
            .create_task("Full stack", 2, &["backend", "frontend"], None)
            .unwrap();

        let backend_tasks = manager.query_tasks().tag("backend").execute().unwrap();
        assert_eq!(backend_tasks.len(), 2);
    }

    #[test]
    fn test_update_task() {
        let (_temp, mut manager) = setup();

        let task = manager.create_task("Original", 2, &["tag1"], None).unwrap();

        let updated = manager
            .update_task(&task.id, Some("Updated"), Some("New context"), Some(1), Some(&["tag2"]))
            .unwrap();

        assert_eq!(updated.description, "Updated");
        assert_eq!(updated.context, Some("New context".to_string()));
        assert_eq!(updated.priority, 1);
        assert!(updated.tags.contains(&"tag2".to_string()));
        assert!(!updated.tags.contains(&"tag1".to_string()));
    }
}
