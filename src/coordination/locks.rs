//! Resource lock manager with deadlock prevention.
//!
//! Provides file-level and path-level locking to prevent conflicts
//! when multiple tasks operate on the same resources.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::error::Result;
use crate::task::TaskId;

/// Unique identifier for a lockable resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceId {
    /// A file path.
    File(PathBuf),
    /// A directory path.
    Directory(PathBuf),
    /// A custom named resource.
    Named(String),
}

impl ResourceId {
    /// Create a resource ID for a file.
    pub fn file(path: impl AsRef<Path>) -> Self {
        Self::File(path.as_ref().to_path_buf())
    }

    /// Create a resource ID for a directory.
    pub fn directory(path: impl AsRef<Path>) -> Self {
        Self::Directory(path.as_ref().to_path_buf())
    }

    /// Create a named resource ID.
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }
}

/// Information about a held lock.
#[derive(Debug, Clone)]
pub struct Lock {
    /// The resource being locked.
    pub resource: ResourceId,
    /// The task holding the lock.
    pub holder: TaskId,
    /// When the lock was acquired.
    pub acquired_at: Instant,
    /// Whether this is an exclusive lock (write) or shared (read).
    pub exclusive: bool,
}

/// Lock acquisition result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    /// Lock acquired successfully.
    Acquired,
    /// Lock is held by another task.
    Blocked { holder: TaskId },
    /// Would cause a deadlock.
    WouldDeadlock { cycle: Vec<TaskId> },
    /// Lock timed out waiting.
    Timeout,
}

/// Lock state for a resource.
#[derive(Default)]
struct ResourceLockState {
    /// Exclusive lock holder (if any).
    exclusive_holder: Option<TaskId>,
    /// Shared lock holders.
    shared_holders: HashSet<TaskId>,
    /// Queue of tasks waiting for this lock.
    waiters: VecDeque<(TaskId, bool)>, // (task_id, wants_exclusive)
}

/// Manages resource locks across tasks.
pub struct LockManager {
    /// Lock states by resource.
    locks: Arc<Mutex<HashMap<ResourceId, ResourceLockState>>>,
    /// Which resources each task is waiting for.
    waiting_for: Arc<Mutex<HashMap<TaskId, ResourceId>>>,
    /// Default lock timeout.
    default_timeout: Duration,
}

impl LockManager {
    /// Create a new lock manager.
    pub fn new() -> Self {
        Self {
            locks: Arc::new(Mutex::new(HashMap::new())),
            waiting_for: Arc::new(Mutex::new(HashMap::new())),
            default_timeout: Duration::from_secs(30),
        }
    }

    /// Create a lock manager with custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            default_timeout: timeout,
            ..Self::new()
        }
    }

    /// Attempt to acquire an exclusive (write) lock.
    pub async fn acquire_exclusive(&self, resource: ResourceId, task_id: &TaskId) -> Result<LockResult> {
        self.acquire_internal(resource, task_id, true).await
    }

    /// Attempt to acquire a shared (read) lock.
    pub async fn acquire_shared(&self, resource: ResourceId, task_id: &TaskId) -> Result<LockResult> {
        self.acquire_internal(resource, task_id, false).await
    }

    /// Internal lock acquisition.
    async fn acquire_internal(&self, resource: ResourceId, task_id: &TaskId, exclusive: bool) -> Result<LockResult> {
        let mut locks = self.locks.lock().await;
        let state = locks.entry(resource.clone()).or_default();

        // Check if we already hold the lock
        if state.exclusive_holder.as_ref() == Some(task_id) {
            return Ok(LockResult::Acquired);
        }
        if state.shared_holders.contains(task_id) {
            if exclusive {
                // Can't upgrade from shared to exclusive if others hold shared
                if state.shared_holders.len() > 1 {
                    return Ok(LockResult::Blocked {
                        holder: state.shared_holders.iter().find(|t| *t != task_id).unwrap().clone(),
                    });
                }
                // Upgrade: remove shared, acquire exclusive
                state.shared_holders.remove(task_id);
                state.exclusive_holder = Some(task_id.clone());
            }
            return Ok(LockResult::Acquired);
        }

        // Check for conflicts
        if exclusive {
            // Exclusive requires no other holders
            if let Some(holder) = &state.exclusive_holder {
                return Ok(LockResult::Blocked { holder: holder.clone() });
            }
            if !state.shared_holders.is_empty() {
                let holder = state.shared_holders.iter().next().unwrap().clone();
                return Ok(LockResult::Blocked { holder });
            }
            // Acquire exclusive
            state.exclusive_holder = Some(task_id.clone());
        } else {
            // Shared requires no exclusive holder
            if let Some(holder) = &state.exclusive_holder {
                return Ok(LockResult::Blocked { holder: holder.clone() });
            }
            // Acquire shared
            state.shared_holders.insert(task_id.clone());
        }

        Ok(LockResult::Acquired)
    }

    /// Release a lock held by a task.
    pub async fn release(&self, resource: &ResourceId, task_id: &TaskId) -> Result<()> {
        let mut locks = self.locks.lock().await;

        if let Some(state) = locks.get_mut(resource) {
            if state.exclusive_holder.as_ref() == Some(task_id) {
                state.exclusive_holder = None;
            }
            state.shared_holders.remove(task_id);

            // Clean up empty states
            if state.exclusive_holder.is_none() && state.shared_holders.is_empty() && state.waiters.is_empty() {
                locks.remove(resource);
            }
        }

        // Remove from waiting_for
        let mut waiting = self.waiting_for.lock().await;
        waiting.remove(task_id);

        Ok(())
    }

    /// Release all locks held by a task.
    pub async fn release_all(&self, task_id: &TaskId) -> Result<Vec<ResourceId>> {
        let mut released = Vec::new();
        let mut locks = self.locks.lock().await;

        let resources: Vec<_> = locks.keys().cloned().collect();
        for resource in resources {
            if let Some(state) = locks.get_mut(&resource) {
                let was_held =
                    state.exclusive_holder.as_ref() == Some(task_id) || state.shared_holders.contains(task_id);

                if was_held {
                    if state.exclusive_holder.as_ref() == Some(task_id) {
                        state.exclusive_holder = None;
                    }
                    state.shared_holders.remove(task_id);
                    released.push(resource.clone());
                }
            }
        }

        // Clean up empty states
        locks.retain(|_, state| {
            state.exclusive_holder.is_some() || !state.shared_holders.is_empty() || !state.waiters.is_empty()
        });

        // Remove from waiting_for
        let mut waiting = self.waiting_for.lock().await;
        waiting.remove(task_id);

        Ok(released)
    }

    /// Check if a resource is locked.
    pub async fn is_locked(&self, resource: &ResourceId) -> bool {
        let locks = self.locks.lock().await;
        if let Some(state) = locks.get(resource) {
            state.exclusive_holder.is_some() || !state.shared_holders.is_empty()
        } else {
            false
        }
    }

    /// Get the holder of a lock (if any).
    pub async fn get_holder(&self, resource: &ResourceId) -> Option<TaskId> {
        let locks = self.locks.lock().await;
        if let Some(state) = locks.get(resource) {
            state
                .exclusive_holder
                .clone()
                .or_else(|| state.shared_holders.iter().next().cloned())
        } else {
            None
        }
    }

    /// Get all locks held by a task.
    pub async fn locks_held_by(&self, task_id: &TaskId) -> Vec<Lock> {
        let locks = self.locks.lock().await;
        let mut result = Vec::new();

        for (resource, state) in locks.iter() {
            if state.exclusive_holder.as_ref() == Some(task_id) {
                result.push(Lock {
                    resource: resource.clone(),
                    holder: task_id.clone(),
                    acquired_at: Instant::now(), // We don't track acquisition time yet
                    exclusive: true,
                });
            } else if state.shared_holders.contains(task_id) {
                result.push(Lock {
                    resource: resource.clone(),
                    holder: task_id.clone(),
                    acquired_at: Instant::now(),
                    exclusive: false,
                });
            }
        }

        result
    }

    /// Get statistics about current locks.
    pub async fn stats(&self) -> LockStats {
        let locks = self.locks.lock().await;
        let mut exclusive = 0;
        let mut shared = 0;
        let mut total_shared_holders = 0;

        for state in locks.values() {
            if state.exclusive_holder.is_some() {
                exclusive += 1;
            }
            if !state.shared_holders.is_empty() {
                shared += 1;
                total_shared_holders += state.shared_holders.len();
            }
        }

        LockStats {
            exclusive_locks: exclusive,
            shared_locks: shared,
            total_shared_holders,
            total_resources: locks.len(),
        }
    }

    /// Get the default lock timeout.
    pub fn default_timeout(&self) -> Duration {
        self.default_timeout
    }
}

impl Default for LockManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about current locks.
#[derive(Debug, Clone, Default)]
pub struct LockStats {
    /// Number of resources with exclusive locks.
    pub exclusive_locks: usize,
    /// Number of resources with shared locks.
    pub shared_locks: usize,
    /// Total number of shared lock holders.
    pub total_shared_holders: usize,
    /// Total number of locked resources.
    pub total_resources: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    #[tokio::test]
    async fn test_exclusive_lock() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task = task_id("task-1");

        let result = manager.acquire_exclusive(resource.clone(), &task).await.unwrap();
        assert_eq!(result, LockResult::Acquired);

        assert!(manager.is_locked(&resource).await);
        assert_eq!(manager.get_holder(&resource).await, Some(task.clone()));
    }

    #[tokio::test]
    async fn test_exclusive_blocks_exclusive() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        manager.acquire_exclusive(resource.clone(), &task1).await.unwrap();

        let result = manager.acquire_exclusive(resource.clone(), &task2).await.unwrap();
        assert!(matches!(result, LockResult::Blocked { holder } if holder == task1));
    }

    #[tokio::test]
    async fn test_shared_locks() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        let result1 = manager.acquire_shared(resource.clone(), &task1).await.unwrap();
        assert_eq!(result1, LockResult::Acquired);

        let result2 = manager.acquire_shared(resource.clone(), &task2).await.unwrap();
        assert_eq!(result2, LockResult::Acquired);

        // Both should hold the lock
        let stats = manager.stats().await;
        assert_eq!(stats.total_shared_holders, 2);
    }

    #[tokio::test]
    async fn test_exclusive_blocks_shared() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        manager.acquire_exclusive(resource.clone(), &task1).await.unwrap();

        let result = manager.acquire_shared(resource.clone(), &task2).await.unwrap();
        assert!(matches!(result, LockResult::Blocked { holder } if holder == task1));
    }

    #[tokio::test]
    async fn test_shared_blocks_exclusive() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        manager.acquire_shared(resource.clone(), &task1).await.unwrap();

        let result = manager.acquire_exclusive(resource.clone(), &task2).await.unwrap();
        assert!(matches!(result, LockResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn test_release_lock() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        manager.acquire_exclusive(resource.clone(), &task1).await.unwrap();
        manager.release(&resource, &task1).await.unwrap();

        // Now task2 can acquire
        let result = manager.acquire_exclusive(resource.clone(), &task2).await.unwrap();
        assert_eq!(result, LockResult::Acquired);
    }

    #[tokio::test]
    async fn test_release_all() {
        let manager = LockManager::new();
        let resource1 = ResourceId::file("/test1.txt");
        let resource2 = ResourceId::file("/test2.txt");
        let task = task_id("task-1");

        manager.acquire_exclusive(resource1.clone(), &task).await.unwrap();
        manager.acquire_shared(resource2.clone(), &task).await.unwrap();

        let released = manager.release_all(&task).await.unwrap();
        assert_eq!(released.len(), 2);

        assert!(!manager.is_locked(&resource1).await);
        assert!(!manager.is_locked(&resource2).await);
    }

    #[tokio::test]
    async fn test_locks_held_by() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task = task_id("task-1");

        manager.acquire_exclusive(resource.clone(), &task).await.unwrap();

        let locks = manager.locks_held_by(&task).await;
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].resource, resource);
        assert!(locks[0].exclusive);
    }

    #[tokio::test]
    async fn test_reacquire_same_lock() {
        let manager = LockManager::new();
        let resource = ResourceId::file("/test.txt");
        let task = task_id("task-1");

        manager.acquire_exclusive(resource.clone(), &task).await.unwrap();

        // Re-acquiring the same lock should succeed
        let result = manager.acquire_exclusive(resource.clone(), &task).await.unwrap();
        assert_eq!(result, LockResult::Acquired);
    }

    #[tokio::test]
    async fn test_named_resource() {
        let manager = LockManager::new();
        let resource = ResourceId::named("api-rate-limit");
        let task = task_id("task-1");

        let result = manager.acquire_exclusive(resource.clone(), &task).await.unwrap();
        assert_eq!(result, LockResult::Acquired);
    }

    #[tokio::test]
    async fn test_stats() {
        let manager = LockManager::new();
        let task1 = task_id("task-1");
        let task2 = task_id("task-2");

        manager
            .acquire_exclusive(ResourceId::file("/exclusive.txt"), &task1)
            .await
            .unwrap();
        manager
            .acquire_shared(ResourceId::file("/shared.txt"), &task1)
            .await
            .unwrap();
        manager
            .acquire_shared(ResourceId::file("/shared.txt"), &task2)
            .await
            .unwrap();

        let stats = manager.stats().await;
        assert_eq!(stats.exclusive_locks, 1);
        assert_eq!(stats.shared_locks, 1);
        assert_eq!(stats.total_shared_holders, 2);
        assert_eq!(stats.total_resources, 2);
    }
}
