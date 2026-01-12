//! Event bus for inter-task communication.
//!
//! Provides a publish-subscribe system for tasks to communicate events
//! and coordinate their activities.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::sync::broadcast::{self, Receiver, Sender};

use crate::task::TaskId;

/// Kinds of events that can be published.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    /// A task started.
    TaskStarted,
    /// A task completed successfully.
    TaskCompleted,
    /// A task failed.
    TaskFailed,
    /// A task was cancelled.
    TaskCancelled,
    /// A task is waiting for user input.
    TaskWaitingForUser,
    /// A file was modified.
    FileModified,
    /// A learning was extracted.
    LearningExtracted,
    /// A resource lock was acquired.
    LockAcquired,
    /// A resource lock was released.
    LockReleased,
    /// Rate limit reached.
    RateLimitReached,

    // Supervision events
    /// A nudge was sent to a task.
    NudgeSent,
    /// A sync message was relayed between tasks.
    SyncRelayed,
    /// A task was paused by the watcher.
    TaskPaused,
    /// Supervision entered degraded mode.
    SupervisionDegraded,
    /// Supervision recovered from degraded mode.
    SupervisionRecovered,

    // Proactive rebase events
    /// Main branch received new commits.
    MainUpdated,
    /// A task needs to rebase against main.
    RebaseRequired,
    /// A task completed rebasing successfully.
    RebaseCompleted,
    /// A rebase encountered conflicts and was aborted.
    RebaseConflict,

    /// Custom event type.
    Custom(String),
}

impl EventKind {
    /// Returns true if this event kind should be persisted to engram.
    /// Durable events form the audit trail for debugging.
    pub fn is_durable(&self) -> bool {
        match self {
            // Task lifecycle events - always persist
            EventKind::TaskStarted => true,
            EventKind::TaskCompleted => true,
            EventKind::TaskFailed => true,
            EventKind::TaskCancelled => true,

            // Coordination events - always persist
            EventKind::MainUpdated => true,
            EventKind::RebaseRequired => true,
            EventKind::RebaseCompleted => true,
            EventKind::RebaseConflict => true,

            // Supervision events - persist for debugging
            EventKind::NudgeSent => true,
            EventKind::SyncRelayed => true,
            EventKind::TaskPaused => true,
            EventKind::SupervisionDegraded => true,
            EventKind::SupervisionRecovered => true,

            // Resource events - persist
            EventKind::LearningExtracted => true,
            EventKind::LockAcquired => true,
            EventKind::LockReleased => true,

            // Ephemeral events - do not persist
            EventKind::TaskWaitingForUser => false, // Transient state
            EventKind::FileModified => false,       // High volume, can reconstruct from git
            EventKind::RateLimitReached => false,   // Operational, not coordination

            // Custom events - don't persist by default
            EventKind::Custom(_) => false,
        }
    }

    /// Convert to string representation for storage.
    pub fn to_string(&self) -> String {
        match self {
            EventKind::TaskStarted => "task_started".to_string(),
            EventKind::TaskCompleted => "task_completed".to_string(),
            EventKind::TaskFailed => "task_failed".to_string(),
            EventKind::TaskCancelled => "task_cancelled".to_string(),
            EventKind::TaskWaitingForUser => "task_waiting_for_user".to_string(),
            EventKind::FileModified => "file_modified".to_string(),
            EventKind::LearningExtracted => "learning_extracted".to_string(),
            EventKind::LockAcquired => "lock_acquired".to_string(),
            EventKind::LockReleased => "lock_released".to_string(),
            EventKind::RateLimitReached => "rate_limit_reached".to_string(),
            EventKind::NudgeSent => "nudge_sent".to_string(),
            EventKind::SyncRelayed => "sync_relayed".to_string(),
            EventKind::TaskPaused => "task_paused".to_string(),
            EventKind::SupervisionDegraded => "supervision_degraded".to_string(),
            EventKind::SupervisionRecovered => "supervision_recovered".to_string(),
            EventKind::MainUpdated => "main_updated".to_string(),
            EventKind::RebaseRequired => "rebase_required".to_string(),
            EventKind::RebaseCompleted => "rebase_completed".to_string(),
            EventKind::RebaseConflict => "rebase_conflict".to_string(),
            EventKind::Custom(s) => format!("custom:{}", s),
        }
    }
}

/// An event in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event ID.
    pub id: String,
    /// Kind of event.
    pub kind: EventKind,
    /// Source task (if applicable).
    pub source_task: Option<TaskId>,
    /// Target task (if applicable).
    pub target_task: Option<TaskId>,
    /// Event payload (JSON).
    pub payload: serde_json::Value,
    /// When the event was created.
    pub timestamp: DateTime<Utc>,
}

impl Event {
    /// Create a new event.
    pub fn new(kind: EventKind) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            kind,
            source_task: None,
            target_task: None,
            payload: serde_json::Value::Null,
            timestamp: Utc::now(),
        }
    }

    /// Set the source task.
    pub fn from_task(mut self, task_id: TaskId) -> Self {
        self.source_task = Some(task_id);
        self
    }

    /// Set the target task.
    pub fn to_task(mut self, task_id: TaskId) -> Self {
        self.target_task = Some(task_id);
        self
    }

    /// Set the payload.
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

/// Subscription to events.
pub struct Subscription {
    /// Receiver for events.
    receiver: Receiver<Event>,
    /// Filter for event kinds (empty = all).
    kinds: Vec<EventKind>,
    /// Filter for source tasks (empty = all).
    source_tasks: Vec<TaskId>,
}

impl Subscription {
    /// Receive the next event matching the filters.
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => {
                    if self.matches(&event) {
                        return Some(event);
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }

    /// Try to receive an event without blocking.
    pub fn try_recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    if self.matches(&event) {
                        return Some(event);
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// Check if an event matches the subscription filters.
    fn matches(&self, event: &Event) -> bool {
        // Check kind filter
        if !self.kinds.is_empty() && !self.kinds.contains(&event.kind) {
            return false;
        }

        // Check source task filter
        if !self.source_tasks.is_empty() {
            if let Some(source) = &event.source_task {
                if !self.source_tasks.contains(source) {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }
}

/// Event bus for inter-task communication.
pub struct EventBus {
    /// Broadcast sender for events.
    sender: Sender<Event>,
    /// History of recent events.
    history: Arc<Mutex<Vec<Event>>>,
    /// Maximum history size.
    max_history: usize,
    /// Event counts by kind.
    counts: Arc<Mutex<HashMap<EventKind, usize>>>,
    /// Optional engram store for persisting durable events.
    engram_store: Option<Arc<std::sync::Mutex<engram::Store>>>,
}

impl EventBus {
    /// Create a new event bus.
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            history: Arc::new(Mutex::new(Vec::new())),
            max_history: 1000,
            counts: Arc::new(Mutex::new(HashMap::new())),
            engram_store: None,
        }
    }

    /// Create an event bus with custom history size.
    pub fn with_history_size(size: usize) -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            history: Arc::new(Mutex::new(Vec::new())),
            max_history: size,
            counts: Arc::new(Mutex::new(HashMap::new())),
            engram_store: None,
        }
    }

    /// Create an event bus that persists durable events to engram.
    pub fn with_engram(store: Arc<std::sync::Mutex<engram::Store>>) -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            sender,
            history: Arc::new(Mutex::new(Vec::new())),
            max_history: 1000,
            counts: Arc::new(Mutex::new(HashMap::new())),
            engram_store: Some(store),
        }
    }

    /// Set the engram store for event persistence.
    pub fn set_engram_store(&mut self, store: Arc<std::sync::Mutex<engram::Store>>) {
        self.engram_store = Some(store);
    }

    /// Publish an event.
    pub async fn publish(&self, event: Event) {
        // Persist durable events to engram (fire-and-forget)
        if let Some(store) = &self.engram_store {
            if event.kind.is_durable() {
                let engram_event = self.to_engram_event(&event);
                if let Ok(mut s) = store.lock() {
                    if let Err(e) = s.record_event_raw(&engram_event) {
                        log::warn!("Failed to persist event to engram: {}", e);
                    }
                }
            }
        }

        // Add to history
        {
            let mut history = self.history.lock().await;
            history.push(event.clone());
            while history.len() > self.max_history {
                history.remove(0);
            }
        }

        // Update counts
        {
            let mut counts = self.counts.lock().await;
            *counts.entry(event.kind.clone()).or_insert(0) += 1;
        }

        // Broadcast (ignore if no receivers)
        let _ = self.sender.send(event);
    }

    /// Convert an EventBus event to an engram Event.
    fn to_engram_event(&self, event: &Event) -> engram::Event {
        engram::Event {
            id: format!("eg-evt-{}", &event.id[..10.min(event.id.len())]),
            kind: event.kind.to_string(),
            source_task: event.source_task.as_ref().map(|t| t.0.clone()),
            target_task: event.target_task.as_ref().map(|t| t.0.clone()),
            payload: event.payload.clone(),
            timestamp: event.timestamp,
        }
    }

    /// Subscribe to all events.
    pub fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.sender.subscribe(),
            kinds: Vec::new(),
            source_tasks: Vec::new(),
        }
    }

    /// Subscribe to specific event kinds.
    pub fn subscribe_to(&self, kinds: Vec<EventKind>) -> Subscription {
        Subscription {
            receiver: self.sender.subscribe(),
            kinds,
            source_tasks: Vec::new(),
        }
    }

    /// Subscribe to events from specific tasks.
    pub fn subscribe_to_tasks(&self, task_ids: Vec<TaskId>) -> Subscription {
        Subscription {
            receiver: self.sender.subscribe(),
            kinds: Vec::new(),
            source_tasks: task_ids,
        }
    }

    /// Get recent events.
    pub async fn recent_events(&self, limit: usize) -> Vec<Event> {
        let history = self.history.lock().await;
        let start = history.len().saturating_sub(limit);
        history[start..].to_vec()
    }

    /// Get events matching a filter.
    pub async fn query_events(
        &self,
        kinds: Option<&[EventKind]>,
        source_task: Option<&TaskId>,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Vec<Event> {
        let history = self.history.lock().await;
        history
            .iter()
            .rev()
            .filter(|e| {
                if let Some(k) = kinds
                    && !k.contains(&e.kind)
                {
                    return false;
                }
                if let Some(task) = source_task
                    && e.source_task.as_ref() != Some(task)
                {
                    return false;
                }
                if let Some(since) = since
                    && e.timestamp < since
                {
                    return false;
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Get event counts by kind.
    pub async fn event_counts(&self) -> HashMap<EventKind, usize> {
        self.counts.lock().await.clone()
    }

    /// Clear event history.
    pub async fn clear_history(&self) {
        let mut history = self.history.lock().await;
        history.clear();
    }

    /// Get the number of subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// Convenience functions for common events
impl EventBus {
    /// Publish a task started event.
    pub async fn task_started(&self, task_id: TaskId) {
        self.publish(Event::new(EventKind::TaskStarted).from_task(task_id))
            .await;
    }

    /// Publish a task completed event.
    pub async fn task_completed(&self, task_id: TaskId, summary: &str) {
        self.publish(
            Event::new(EventKind::TaskCompleted)
                .from_task(task_id)
                .with_payload(serde_json::json!({"summary": summary})),
        )
        .await;
    }

    /// Publish a task failed event.
    pub async fn task_failed(&self, task_id: TaskId, error: &str) {
        self.publish(
            Event::new(EventKind::TaskFailed)
                .from_task(task_id)
                .with_payload(serde_json::json!({"error": error})),
        )
        .await;
    }

    /// Publish a task cancelled event.
    pub async fn task_cancelled(&self, task_id: TaskId) {
        self.publish(Event::new(EventKind::TaskCancelled).from_task(task_id))
            .await;
    }

    /// Publish a file modified event.
    pub async fn file_modified(&self, task_id: TaskId, path: &str) {
        self.publish(
            Event::new(EventKind::FileModified)
                .from_task(task_id)
                .with_payload(serde_json::json!({"path": path})),
        )
        .await;
    }

    /// Publish a learning extracted event.
    pub async fn learning_extracted(&self, task_id: TaskId, learning_id: &str) {
        self.publish(
            Event::new(EventKind::LearningExtracted)
                .from_task(task_id)
                .with_payload(serde_json::json!({"learning_id": learning_id})),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    #[tokio::test]
    async fn test_publish_subscribe() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        let event = Event::new(EventKind::TaskStarted).from_task(task_id("task-1"));
        bus.publish(event.clone()).await;

        let received = sub.try_recv();
        assert!(received.is_some());
        assert_eq!(received.unwrap().kind, EventKind::TaskStarted);
    }

    #[tokio::test]
    async fn test_subscribe_to_kinds() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe_to(vec![EventKind::TaskCompleted]);

        // Publish different event types
        bus.publish(Event::new(EventKind::TaskStarted)).await;
        bus.publish(Event::new(EventKind::TaskCompleted)).await;
        bus.publish(Event::new(EventKind::TaskFailed)).await;

        // Should only receive TaskCompleted
        let received = sub.try_recv();
        assert!(received.is_some());
        assert_eq!(received.unwrap().kind, EventKind::TaskCompleted);

        // No more matching events
        assert!(sub.try_recv().is_none());
    }

    #[tokio::test]
    async fn test_subscribe_to_tasks() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe_to_tasks(vec![task_id("task-1")]);

        // Publish from different tasks
        bus.publish(Event::new(EventKind::TaskStarted).from_task(task_id("task-1")))
            .await;
        bus.publish(Event::new(EventKind::TaskStarted).from_task(task_id("task-2")))
            .await;

        // Should only receive from task-1
        let received = sub.try_recv();
        assert!(received.is_some());
        assert_eq!(received.unwrap().source_task, Some(task_id("task-1")));

        // No more matching events
        assert!(sub.try_recv().is_none());
    }

    #[tokio::test]
    async fn test_recent_events() {
        let bus = EventBus::with_history_size(10);

        for i in 0..5 {
            bus.publish(Event::new(EventKind::TaskStarted).with_payload(serde_json::json!({"index": i})))
                .await;
        }

        let recent = bus.recent_events(3).await;
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].payload["index"], 2);
        assert_eq!(recent[2].payload["index"], 4);
    }

    #[tokio::test]
    async fn test_history_limit() {
        let bus = EventBus::with_history_size(5);

        for i in 0..10 {
            bus.publish(Event::new(EventKind::TaskStarted).with_payload(serde_json::json!({"index": i})))
                .await;
        }

        let recent = bus.recent_events(100).await;
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0].payload["index"], 5);
    }

    #[tokio::test]
    async fn test_query_events() {
        let bus = EventBus::new();

        bus.task_started(task_id("task-1")).await;
        bus.task_completed(task_id("task-1"), "done").await;
        bus.task_started(task_id("task-2")).await;

        let completed = bus
            .query_events(Some(&[EventKind::TaskCompleted]), None, None, 10)
            .await;
        assert_eq!(completed.len(), 1);

        let from_task1 = bus.query_events(None, Some(&task_id("task-1")), None, 10).await;
        assert_eq!(from_task1.len(), 2);
    }

    #[tokio::test]
    async fn test_event_counts() {
        let bus = EventBus::new();

        bus.task_started(task_id("task-1")).await;
        bus.task_started(task_id("task-2")).await;
        bus.task_completed(task_id("task-1"), "done").await;

        let counts = bus.event_counts().await;
        assert_eq!(counts.get(&EventKind::TaskStarted), Some(&2));
        assert_eq!(counts.get(&EventKind::TaskCompleted), Some(&1));
    }

    #[tokio::test]
    async fn test_convenience_methods() {
        let bus = EventBus::new();
        let mut sub = bus.subscribe();

        bus.task_started(task_id("task-1")).await;
        bus.task_completed(task_id("task-1"), "All done").await;
        bus.file_modified(task_id("task-1"), "/test.txt").await;

        let e1 = sub.try_recv().unwrap();
        assert_eq!(e1.kind, EventKind::TaskStarted);

        let e2 = sub.try_recv().unwrap();
        assert_eq!(e2.kind, EventKind::TaskCompleted);
        assert_eq!(e2.payload["summary"], "All done");

        let e3 = sub.try_recv().unwrap();
        assert_eq!(e3.kind, EventKind::FileModified);
        assert_eq!(e3.payload["path"], "/test.txt");
    }

    #[test]
    fn test_event_builder() {
        let event = Event::new(EventKind::Custom("test".to_string()))
            .from_task(task_id("task-1"))
            .to_task(task_id("task-2"))
            .with_payload(serde_json::json!({"key": "value"}));

        assert_eq!(event.source_task, Some(task_id("task-1")));
        assert_eq!(event.target_task, Some(task_id("task-2")));
        assert_eq!(event.payload["key"], "value");
    }
}
