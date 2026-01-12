//! Notification system for user alerts.
//!
//! Provides terminal title updates and notification logging.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::task::TaskId;

/// Kind of notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationKind {
    /// Task needs user input.
    NeedsInput,
    /// Task completed successfully.
    TaskCompleted,
    /// Task failed.
    TaskFailed,
    /// General information.
    Info,
    /// Warning message.
    Warning,
    /// Error message.
    Error,
}

/// A notification message.
#[derive(Debug, Clone)]
pub struct Notification {
    /// Notification kind.
    pub kind: NotificationKind,
    /// Brief message.
    pub message: String,
    /// Related task (if any).
    pub task_id: Option<TaskId>,
    /// When the notification was created.
    pub timestamp: DateTime<Utc>,
    /// Whether the notification has been read.
    pub read: bool,
}

impl Notification {
    /// Create a new notification.
    pub fn new(kind: NotificationKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            task_id: None,
            timestamp: Utc::now(),
            read: false,
        }
    }

    /// Set the related task.
    pub fn for_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    /// Create a needs-input notification.
    pub fn needs_input(task_id: TaskId, prompt: impl Into<String>) -> Self {
        Self::new(NotificationKind::NeedsInput, prompt).for_task(task_id)
    }

    /// Create a task-completed notification.
    pub fn task_completed(task_id: TaskId, summary: impl Into<String>) -> Self {
        Self::new(NotificationKind::TaskCompleted, summary).for_task(task_id)
    }

    /// Create a task-failed notification.
    pub fn task_failed(task_id: TaskId, error: impl Into<String>) -> Self {
        Self::new(NotificationKind::TaskFailed, error).for_task(task_id)
    }
}

/// Manages notifications and user alerts.
pub struct Notifier {
    /// Recent notifications.
    notifications: Arc<Mutex<VecDeque<Notification>>>,
    /// Maximum notifications to keep.
    max_notifications: usize,
    /// Current terminal title.
    current_title: Arc<Mutex<String>>,
    /// Whether terminal title updates are enabled.
    title_enabled: bool,
}

impl Notifier {
    /// Create a new notifier.
    pub fn new() -> Self {
        Self {
            notifications: Arc::new(Mutex::new(VecDeque::new())),
            max_notifications: 100,
            current_title: Arc::new(Mutex::new(String::new())),
            title_enabled: true,
        }
    }

    /// Create a notifier with custom settings.
    pub fn with_settings(max_notifications: usize, title_enabled: bool) -> Self {
        Self {
            notifications: Arc::new(Mutex::new(VecDeque::new())),
            max_notifications,
            current_title: Arc::new(Mutex::new(String::new())),
            title_enabled,
        }
    }

    /// Add a notification.
    pub async fn notify(&self, notification: Notification) {
        // Update terminal title for important notifications
        if self.title_enabled {
            match notification.kind {
                NotificationKind::NeedsInput => {
                    self.set_title(&format!("[!] Input needed: {}", notification.message))
                        .await;
                }
                NotificationKind::TaskCompleted => {
                    self.set_title(&format!("[✓] Completed: {}", notification.message))
                        .await;
                }
                NotificationKind::TaskFailed => {
                    self.set_title(&format!("[✗] Failed: {}", notification.message)).await;
                }
                _ => {}
            }
        }

        // Add to notification list
        let mut notifications = self.notifications.lock().await;
        notifications.push_back(notification);
        while notifications.len() > self.max_notifications {
            notifications.pop_front();
        }
    }

    /// Get recent notifications.
    pub async fn recent(&self, limit: usize) -> Vec<Notification> {
        let notifications = self.notifications.lock().await;
        notifications.iter().rev().take(limit).cloned().collect()
    }

    /// Get unread notifications.
    pub async fn unread(&self) -> Vec<Notification> {
        let notifications = self.notifications.lock().await;
        notifications.iter().filter(|n| !n.read).cloned().collect()
    }

    /// Mark a notification as read.
    pub async fn mark_read(&self, index: usize) {
        let mut notifications = self.notifications.lock().await;
        if let Some(notification) = notifications.iter_mut().rev().nth(index) {
            notification.read = true;
        }
    }

    /// Mark all notifications as read.
    pub async fn mark_all_read(&self) {
        let mut notifications = self.notifications.lock().await;
        for notification in notifications.iter_mut() {
            notification.read = true;
        }
    }

    /// Clear all notifications.
    pub async fn clear(&self) {
        let mut notifications = self.notifications.lock().await;
        notifications.clear();
    }

    /// Get notification count.
    pub async fn count(&self) -> usize {
        self.notifications.lock().await.len()
    }

    /// Get unread count.
    pub async fn unread_count(&self) -> usize {
        let notifications = self.notifications.lock().await;
        notifications.iter().filter(|n| !n.read).count()
    }

    /// Set terminal title.
    pub async fn set_title(&self, title: &str) {
        if !self.title_enabled {
            return;
        }

        let mut current = self.current_title.lock().await;
        if *current != title {
            *current = title.to_string();
            // OSC escape sequence to set terminal title
            let _ = write!(io::stdout(), "\x1b]0;{}\x07", title);
            let _ = io::stdout().flush();
        }
    }

    /// Reset terminal title.
    pub async fn reset_title(&self) {
        if self.title_enabled {
            let mut current = self.current_title.lock().await;
            *current = String::new();
            let _ = write!(io::stdout(), "\x1b]0;\x07");
            let _ = io::stdout().flush();
        }
    }

    /// Update title with task status summary.
    pub async fn update_status_title(&self, running: usize, waiting: usize, completed: usize) {
        let title = format!(
            "neuraphage: {} running, {} waiting, {} done",
            running, waiting, completed
        );
        self.set_title(&title).await;
    }
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    #[tokio::test]
    async fn test_notification_creation() {
        let n = Notification::new(NotificationKind::Info, "Test message");
        assert_eq!(n.kind, NotificationKind::Info);
        assert_eq!(n.message, "Test message");
        assert!(n.task_id.is_none());
        assert!(!n.read);
    }

    #[tokio::test]
    async fn test_notification_for_task() {
        let n = Notification::needs_input(task_id("task-1"), "Choose an option");
        assert_eq!(n.kind, NotificationKind::NeedsInput);
        assert_eq!(n.task_id, Some(task_id("task-1")));
    }

    #[tokio::test]
    async fn test_notifier_add_and_get() {
        let notifier = Notifier::with_settings(10, false);

        notifier
            .notify(Notification::new(NotificationKind::Info, "First"))
            .await;
        notifier
            .notify(Notification::new(NotificationKind::Info, "Second"))
            .await;

        let recent = notifier.recent(10).await;
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].message, "Second"); // Most recent first
        assert_eq!(recent[1].message, "First");
    }

    #[tokio::test]
    async fn test_notifier_max_limit() {
        let notifier = Notifier::with_settings(3, false);

        for i in 0..5 {
            notifier
                .notify(Notification::new(NotificationKind::Info, format!("Msg {}", i)))
                .await;
        }

        assert_eq!(notifier.count().await, 3);
        let recent = notifier.recent(10).await;
        assert_eq!(recent[0].message, "Msg 4");
        assert_eq!(recent[2].message, "Msg 2");
    }

    #[tokio::test]
    async fn test_unread_tracking() {
        let notifier = Notifier::with_settings(10, false);

        notifier.notify(Notification::new(NotificationKind::Info, "One")).await;
        notifier.notify(Notification::new(NotificationKind::Info, "Two")).await;

        assert_eq!(notifier.unread_count().await, 2);

        notifier.mark_read(0).await; // Mark "Two" (most recent) as read
        assert_eq!(notifier.unread_count().await, 1);

        notifier.mark_all_read().await;
        assert_eq!(notifier.unread_count().await, 0);
    }

    #[tokio::test]
    async fn test_clear() {
        let notifier = Notifier::with_settings(10, false);

        notifier.notify(Notification::new(NotificationKind::Info, "Test")).await;
        assert_eq!(notifier.count().await, 1);

        notifier.clear().await;
        assert_eq!(notifier.count().await, 0);
    }

    #[test]
    fn test_convenience_constructors() {
        let completed = Notification::task_completed(task_id("task-1"), "All done");
        assert_eq!(completed.kind, NotificationKind::TaskCompleted);

        let failed = Notification::task_failed(task_id("task-2"), "Error occurred");
        assert_eq!(failed.kind, NotificationKind::TaskFailed);
    }
}
