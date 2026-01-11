//! Task scheduler with priority queue and rate limit management.
//!
//! The scheduler coordinates task execution across the daemon, managing:
//! - Priority-based task ordering
//! - Concurrent task limits
//! - Rate limiting across all tasks

use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::error::Result;
use crate::task::{Task, TaskId};

/// Configuration for the scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum concurrent running tasks.
    pub max_concurrent: usize,
    /// Maximum requests per minute (global rate limit).
    pub max_requests_per_minute: u32,
    /// Time window for rate limiting.
    pub rate_window: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 5,
            max_requests_per_minute: 50,
            rate_window: Duration::from_secs(60),
        }
    }
}

/// Result of attempting to schedule a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleResult {
    /// Task can run immediately.
    Ready,
    /// Task is queued, waiting for a slot.
    Queued { position: usize },
    /// Task is rate limited, retry after duration.
    RateLimited { retry_after: Duration },
    /// Cannot schedule (e.g., already running).
    Rejected { reason: String },
}

/// A queued task with priority information.
#[derive(Debug, Clone)]
struct QueuedTask {
    /// Task ID.
    id: TaskId,
    /// Priority (lower = higher priority).
    priority: u8,
    /// When the task was queued.
    queued_at: Instant,
}

impl PartialEq for QueuedTask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for QueuedTask {}

impl PartialOrd for QueuedTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Lower priority number = higher priority (goes first)
        // If same priority, earlier queued time wins
        other
            .priority
            .cmp(&self.priority)
            .then_with(|| self.queued_at.cmp(&other.queued_at))
    }
}

/// Task scheduler managing concurrent execution and rate limits.
pub struct Scheduler {
    /// Configuration.
    config: SchedulerConfig,
    /// Priority queue of waiting tasks.
    queue: Arc<Mutex<BinaryHeap<QueuedTask>>>,
    /// Currently running task IDs.
    running: Arc<Mutex<HashMap<TaskId, Instant>>>,
    /// Request timestamps for rate limiting (sliding window).
    request_times: Arc<Mutex<Vec<Instant>>>,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            queue: Arc::new(Mutex::new(BinaryHeap::new())),
            running: Arc::new(Mutex::new(HashMap::new())),
            request_times: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Attempt to schedule a task.
    pub async fn schedule(&self, task: &Task) -> Result<ScheduleResult> {
        // Check if already running
        {
            let running = self.running.lock().await;
            if running.contains_key(&task.id) {
                return Ok(ScheduleResult::Rejected {
                    reason: "Task is already running".to_string(),
                });
            }
        }

        // Check if already queued
        {
            let queue = self.queue.lock().await;
            if queue.iter().any(|q| q.id == task.id) {
                let position = queue.iter().filter(|q| q.id == task.id).count();
                return Ok(ScheduleResult::Queued { position });
            }
        }

        // Check rate limit
        if let Some(retry_after) = self.check_rate_limit().await {
            return Ok(ScheduleResult::RateLimited { retry_after });
        }

        // Check concurrent limit
        let running_count = self.running.lock().await.len();
        if running_count >= self.config.max_concurrent {
            // Add to queue
            let mut queue = self.queue.lock().await;
            queue.push(QueuedTask {
                id: task.id.clone(),
                priority: task.priority,
                queued_at: Instant::now(),
            });
            let position = queue.len();
            return Ok(ScheduleResult::Queued { position });
        }

        // Task can run
        Ok(ScheduleResult::Ready)
    }

    /// Mark a task as started running.
    pub async fn start_task(&self, task_id: &TaskId) -> Result<()> {
        // Record start time
        let mut running = self.running.lock().await;
        running.insert(task_id.clone(), Instant::now());

        // Remove from queue if present
        let mut queue = self.queue.lock().await;
        let items: Vec<_> = queue.drain().filter(|q| q.id != *task_id).collect();
        *queue = items.into_iter().collect();

        Ok(())
    }

    /// Mark a task as finished (completed, failed, or cancelled).
    pub async fn finish_task(&self, task_id: &TaskId) -> Result<Option<TaskId>> {
        // Remove from running
        let mut running = self.running.lock().await;
        running.remove(task_id);

        // Check if we can start the next queued task
        if running.len() < self.config.max_concurrent {
            let mut queue = self.queue.lock().await;
            if let Some(next) = queue.pop() {
                return Ok(Some(next.id));
            }
        }

        Ok(None)
    }

    /// Record an API request (for rate limiting).
    pub async fn record_request(&self) {
        let now = Instant::now();
        let mut times = self.request_times.lock().await;

        // Prune old entries outside the window
        let cutoff = now - self.config.rate_window;
        times.retain(|t| *t > cutoff);

        // Add new request
        times.push(now);
    }

    /// Check rate limit, returning retry duration if limited.
    async fn check_rate_limit(&self) -> Option<Duration> {
        let now = Instant::now();
        let times = self.request_times.lock().await;

        // Count requests in window
        let cutoff = now - self.config.rate_window;
        let recent_count = times.iter().filter(|t| **t > cutoff).count() as u32;

        if recent_count >= self.config.max_requests_per_minute {
            // Find oldest request in window
            if let Some(oldest) = times.iter().filter(|t| **t > cutoff).min() {
                let retry_after = self.config.rate_window - now.duration_since(*oldest);
                return Some(retry_after);
            }
        }

        None
    }

    /// Get the number of currently running tasks.
    pub async fn running_count(&self) -> usize {
        self.running.lock().await.len()
    }

    /// Get the number of queued tasks.
    pub async fn queue_length(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// Get the current rate limit usage (requests in window).
    pub async fn rate_limit_usage(&self) -> (u32, u32) {
        let now = Instant::now();
        let times = self.request_times.lock().await;
        let cutoff = now - self.config.rate_window;
        let recent_count = times.iter().filter(|t| **t > cutoff).count() as u32;
        (recent_count, self.config.max_requests_per_minute)
    }

    /// Get list of running task IDs.
    pub async fn running_tasks(&self) -> Vec<TaskId> {
        self.running.lock().await.keys().cloned().collect()
    }

    /// Get list of queued task IDs (in priority order).
    pub async fn queued_tasks(&self) -> Vec<TaskId> {
        let queue = self.queue.lock().await;
        let mut tasks: Vec<_> = queue.iter().cloned().collect();
        tasks.sort();
        tasks.into_iter().map(|q| q.id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskStatus;

    fn test_task(id: &str, priority: u8) -> Task {
        Task {
            id: TaskId(id.to_string()),
            description: format!("Test task {}", id),
            context: None,
            status: TaskStatus::Queued,
            priority,
            tags: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_at: None,
            close_reason: None,
            parent_id: None,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
        }
    }

    #[tokio::test]
    async fn test_scheduler_ready() {
        let scheduler = Scheduler::new(SchedulerConfig::default());
        let task = test_task("task-1", 2);

        let result = scheduler.schedule(&task).await.unwrap();
        assert_eq!(result, ScheduleResult::Ready);
    }

    #[tokio::test]
    async fn test_scheduler_concurrent_limit() {
        let config = SchedulerConfig {
            max_concurrent: 2,
            ..Default::default()
        };
        let scheduler = Scheduler::new(config);

        // Start two tasks
        let task1 = test_task("task-1", 2);
        let task2 = test_task("task-2", 2);
        let task3 = test_task("task-3", 2);

        assert_eq!(scheduler.schedule(&task1).await.unwrap(), ScheduleResult::Ready);
        scheduler.start_task(&task1.id).await.unwrap();

        assert_eq!(scheduler.schedule(&task2).await.unwrap(), ScheduleResult::Ready);
        scheduler.start_task(&task2.id).await.unwrap();

        // Third task should be queued
        let result = scheduler.schedule(&task3).await.unwrap();
        assert!(matches!(result, ScheduleResult::Queued { .. }));
    }

    #[tokio::test]
    async fn test_scheduler_priority_ordering() {
        let config = SchedulerConfig {
            max_concurrent: 1,
            ..Default::default()
        };
        let scheduler = Scheduler::new(config);

        // Start a task to fill the slot
        let running = test_task("running", 2);
        scheduler.start_task(&running.id).await.unwrap();

        // Queue tasks with different priorities
        let low = test_task("low", 4); // Low priority
        let high = test_task("high", 0); // High priority
        let medium = test_task("medium", 2);

        scheduler.schedule(&low).await.unwrap();
        scheduler.schedule(&medium).await.unwrap();
        scheduler.schedule(&high).await.unwrap();

        // Finish running task - high priority should be next
        let next = scheduler.finish_task(&running.id).await.unwrap();
        assert_eq!(next, Some(TaskId("high".to_string())));
    }

    #[tokio::test]
    async fn test_scheduler_already_running() {
        let scheduler = Scheduler::new(SchedulerConfig::default());
        let task = test_task("task-1", 2);

        scheduler.start_task(&task.id).await.unwrap();

        let result = scheduler.schedule(&task).await.unwrap();
        assert!(matches!(result, ScheduleResult::Rejected { .. }));
    }

    #[tokio::test]
    async fn test_scheduler_rate_limit() {
        let config = SchedulerConfig {
            max_requests_per_minute: 3,
            rate_window: Duration::from_secs(1),
            ..Default::default()
        };
        let scheduler = Scheduler::new(config);

        // Record requests up to limit
        for _ in 0..3 {
            scheduler.record_request().await;
        }

        // Next schedule should be rate limited
        let task = test_task("task-1", 2);
        let result = scheduler.schedule(&task).await.unwrap();
        assert!(matches!(result, ScheduleResult::RateLimited { .. }));
    }

    #[tokio::test]
    async fn test_scheduler_stats() {
        let scheduler = Scheduler::new(SchedulerConfig::default());

        let task1 = test_task("task-1", 2);
        let task2 = test_task("task-2", 2);

        scheduler.start_task(&task1.id).await.unwrap();
        scheduler.start_task(&task2.id).await.unwrap();

        assert_eq!(scheduler.running_count().await, 2);

        let running = scheduler.running_tasks().await;
        assert!(running.contains(&task1.id));
        assert!(running.contains(&task2.id));
    }

    #[tokio::test]
    async fn test_scheduler_finish_promotes_next() {
        let config = SchedulerConfig {
            max_concurrent: 1,
            ..Default::default()
        };
        let scheduler = Scheduler::new(config);

        let task1 = test_task("task-1", 2);
        let task2 = test_task("task-2", 2);

        scheduler.start_task(&task1.id).await.unwrap();
        scheduler.schedule(&task2).await.unwrap();

        assert_eq!(scheduler.queue_length().await, 1);

        // Finish task1, task2 should be promoted
        let next = scheduler.finish_task(&task1.id).await.unwrap();
        assert_eq!(next, Some(task2.id));
    }
}
