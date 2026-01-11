//! Watcher for stuck task detection.
//!
//! Monitors running tasks and detects when they are stuck, struggling,
//! or running away. Takes corrective actions like nudging, escalating,
//! or pausing tasks.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::task::TaskId;

/// Configuration for the Watcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// How often to check tasks (default: 30 seconds).
    #[serde(with = "humantime_serde", default = "default_interval")]
    pub interval: Duration,
    /// Enable watcher (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Maximum iterations without progress before flagging.
    #[serde(default = "default_max_iterations_without_progress")]
    pub max_iterations_without_progress: usize,
    /// Maximum tokens without file changes before flagging.
    #[serde(default = "default_max_tokens_without_changes")]
    pub max_tokens_without_changes: usize,
}

fn default_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_enabled() -> bool {
    true
}

fn default_max_iterations_without_progress() -> usize {
    10
}

fn default_max_tokens_without_changes() -> usize {
    50000
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            interval: default_interval(),
            enabled: default_enabled(),
            max_iterations_without_progress: default_max_iterations_without_progress(),
            max_tokens_without_changes: default_max_tokens_without_changes(),
        }
    }
}

/// A snapshot of task state for the Watcher to evaluate.
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    /// Task ID.
    pub task_id: TaskId,
    /// Current iteration count.
    pub iteration: usize,
    /// Tokens used in this session.
    pub tokens_used: usize,
    /// Recent tool calls (name, success, duration).
    pub recent_tool_calls: Vec<ToolCallSummary>,
    /// Time since last meaningful progress (file write, user response).
    pub time_since_progress: Duration,
    /// Recent conversation snippet.
    pub recent_conversation: String,
    /// Whether the task has made file changes.
    pub has_file_changes: bool,
}

/// Summary of a tool call for the Watcher.
#[derive(Debug, Clone)]
pub struct ToolCallSummary {
    /// Tool name.
    pub name: String,
    /// Whether the call succeeded.
    pub success: bool,
    /// How long the call took.
    pub duration: Duration,
    /// Brief result summary.
    pub result_summary: String,
}

/// Task health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskHealth {
    /// Task is making good progress.
    Healthy,
    /// Task is having some difficulty.
    Struggling,
    /// Task appears stuck.
    Stuck,
    /// Task is burning tokens without progress.
    Runaway,
}

/// Watcher recommendation for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WatcherRecommendation {
    /// Continue without intervention.
    Continue,
    /// Inject a nudge message.
    Nudge,
    /// Escalate to a more capable persona.
    Escalate,
    /// Pause the task and notify the user.
    Pause,
    /// Abort the task as hopeless.
    Abort,
}

/// Result of watching a task.
#[derive(Debug, Clone)]
pub struct WatchResult {
    /// Task ID.
    pub task_id: TaskId,
    /// Health assessment.
    pub status: TaskHealth,
    /// Confidence in the assessment (0.0 - 1.0).
    pub confidence: f64,
    /// Brief diagnosis.
    pub diagnosis: String,
    /// Recommended action.
    pub recommendation: WatcherRecommendation,
    /// Optional nudge message to inject.
    pub nudge_message: Option<String>,
}

impl WatchResult {
    /// Create a healthy result.
    pub fn healthy(task_id: TaskId) -> Self {
        Self {
            task_id,
            status: TaskHealth::Healthy,
            confidence: 1.0,
            diagnosis: "Task is making good progress".to_string(),
            recommendation: WatcherRecommendation::Continue,
            nudge_message: None,
        }
    }

    /// Create a result indicating the task is stuck.
    pub fn stuck(task_id: TaskId, diagnosis: impl Into<String>) -> Self {
        Self {
            task_id,
            status: TaskHealth::Stuck,
            confidence: 0.8,
            diagnosis: diagnosis.into(),
            recommendation: WatcherRecommendation::Nudge,
            nudge_message: None,
        }
    }

    /// Set the nudge message.
    pub fn with_nudge(mut self, message: impl Into<String>) -> Self {
        self.nudge_message = Some(message.into());
        self
    }

    /// Set the recommendation.
    pub fn with_recommendation(mut self, recommendation: WatcherRecommendation) -> Self {
        self.recommendation = recommendation;
        self
    }

    /// Set the confidence.
    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence.clamp(0.0, 1.0);
        self
    }
}

/// The Watcher monitors running tasks for stuck states.
pub struct Watcher {
    /// Configuration.
    config: WatcherConfig,
}

impl Watcher {
    /// Create a new Watcher.
    pub fn new(config: WatcherConfig) -> Self {
        Self { config }
    }

    /// Get the watcher configuration.
    pub fn config(&self) -> &WatcherConfig {
        &self.config
    }

    /// Evaluate a task snapshot and return a watch result.
    ///
    /// This is a heuristic-based evaluation. For production use,
    /// this would be augmented by an LLM call using the Watcher persona.
    pub fn evaluate(&self, snapshot: &TaskSnapshot) -> WatchResult {
        // Check for repeated tool calls (potential stuck loop)
        if self.detect_repeated_calls(&snapshot.recent_tool_calls) {
            return WatchResult::stuck(snapshot.task_id.clone(), "Repeating the same tool calls")
                .with_nudge("Try a different approach - the current one isn't working")
                .with_confidence(0.85);
        }

        // Check for high token burn without file changes
        if snapshot.tokens_used > self.config.max_tokens_without_changes && !snapshot.has_file_changes {
            return WatchResult {
                task_id: snapshot.task_id.clone(),
                status: TaskHealth::Runaway,
                confidence: 0.9,
                diagnosis: "High token usage without making file changes".to_string(),
                recommendation: WatcherRecommendation::Pause,
                nudge_message: Some("Stop and assess: you've used many tokens without writing any files".to_string()),
            };
        }

        // Check for too many iterations without progress
        if snapshot.time_since_progress > Duration::from_secs(300) {
            return WatchResult::stuck(
                snapshot.task_id.clone(),
                format!(
                    "No meaningful progress in {} seconds",
                    snapshot.time_since_progress.as_secs()
                ),
            )
            .with_nudge("You've been working for a while without making progress. Consider asking the user for help.")
            .with_recommendation(WatcherRecommendation::Escalate);
        }

        // Check for repeated failures
        let recent_failures: usize = snapshot.recent_tool_calls.iter().filter(|c| !c.success).count();
        if recent_failures >= 3 {
            return WatchResult::stuck(snapshot.task_id.clone(), "Multiple recent tool failures")
                .with_nudge("Several tool calls have failed. Address the errors before continuing.")
                .with_confidence(0.75);
        }

        // Task seems healthy
        WatchResult::healthy(snapshot.task_id.clone())
    }

    /// Detect if recent tool calls show a stuck pattern.
    fn detect_repeated_calls(&self, calls: &[ToolCallSummary]) -> bool {
        if calls.len() < 3 {
            return false;
        }

        // Check if the last 3 calls are the same tool with similar results
        let last_three: Vec<_> = calls.iter().rev().take(3).collect();
        let same_tool = last_three.windows(2).all(|w| w[0].name == w[1].name);
        let all_failed = last_three.iter().all(|c| !c.success);

        same_tool && all_failed
    }

    /// Check if the watcher is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the check interval.
    pub fn interval(&self) -> Duration {
        self.config.interval
    }
}

impl Default for Watcher {
    fn default() -> Self {
        Self::new(WatcherConfig::default())
    }
}

// humantime_serde helper for duration serialization
mod humantime_serde {
    use std::time::Duration;

    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&humantime::format_duration(*duration).to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        humantime::parse_duration(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    fn make_tool_call(name: &str, success: bool) -> ToolCallSummary {
        ToolCallSummary {
            name: name.to_string(),
            success,
            duration: Duration::from_millis(100),
            result_summary: "test".to_string(),
        }
    }

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert_eq!(config.interval, Duration::from_secs(30));
        assert!(config.enabled);
        assert_eq!(config.max_iterations_without_progress, 10);
        assert_eq!(config.max_tokens_without_changes, 50000);
    }

    #[test]
    fn test_watch_result_healthy() {
        let result = WatchResult::healthy(task_id("task-1"));
        assert_eq!(result.status, TaskHealth::Healthy);
        assert_eq!(result.recommendation, WatcherRecommendation::Continue);
        assert!(result.nudge_message.is_none());
    }

    #[test]
    fn test_watch_result_stuck() {
        let result = WatchResult::stuck(task_id("task-1"), "Stuck on something")
            .with_nudge("Try something else")
            .with_confidence(0.9);

        assert_eq!(result.status, TaskHealth::Stuck);
        assert_eq!(result.confidence, 0.9);
        assert_eq!(result.nudge_message.as_deref(), Some("Try something else"));
    }

    #[test]
    fn test_watcher_evaluate_healthy() {
        let watcher = Watcher::default();
        let snapshot = TaskSnapshot {
            task_id: task_id("task-1"),
            iteration: 5,
            tokens_used: 10000,
            recent_tool_calls: vec![make_tool_call("read_file", true), make_tool_call("edit_file", true)],
            time_since_progress: Duration::from_secs(60),
            recent_conversation: "Working on the task...".to_string(),
            has_file_changes: true,
        };

        let result = watcher.evaluate(&snapshot);
        assert_eq!(result.status, TaskHealth::Healthy);
    }

    #[test]
    fn test_watcher_evaluate_repeated_calls() {
        let watcher = Watcher::default();
        let snapshot = TaskSnapshot {
            task_id: task_id("task-1"),
            iteration: 10,
            tokens_used: 20000,
            recent_tool_calls: vec![
                make_tool_call("glob", false),
                make_tool_call("glob", false),
                make_tool_call("glob", false),
            ],
            time_since_progress: Duration::from_secs(120),
            recent_conversation: "Looking for the file...".to_string(),
            has_file_changes: false,
        };

        let result = watcher.evaluate(&snapshot);
        assert_eq!(result.status, TaskHealth::Stuck);
        assert!(result.nudge_message.is_some());
    }

    #[test]
    fn test_watcher_evaluate_runaway() {
        let watcher = Watcher::default();
        let snapshot = TaskSnapshot {
            task_id: task_id("task-1"),
            iteration: 50,
            tokens_used: 100000,
            recent_tool_calls: vec![make_tool_call("read_file", true)],
            time_since_progress: Duration::from_secs(120),
            recent_conversation: "Still working...".to_string(),
            has_file_changes: false,
        };

        let result = watcher.evaluate(&snapshot);
        assert_eq!(result.status, TaskHealth::Runaway);
        assert_eq!(result.recommendation, WatcherRecommendation::Pause);
    }

    #[test]
    fn test_watcher_evaluate_long_no_progress() {
        let watcher = Watcher::default();
        let snapshot = TaskSnapshot {
            task_id: task_id("task-1"),
            iteration: 20,
            tokens_used: 30000,
            recent_tool_calls: vec![make_tool_call("bash", true)],
            time_since_progress: Duration::from_secs(400),
            recent_conversation: "Hmm...".to_string(),
            has_file_changes: true,
        };

        let result = watcher.evaluate(&snapshot);
        assert_eq!(result.status, TaskHealth::Stuck);
        assert_eq!(result.recommendation, WatcherRecommendation::Escalate);
    }

    #[test]
    fn test_watcher_detect_repeated_calls() {
        let watcher = Watcher::default();

        // Not enough calls
        let calls = vec![make_tool_call("glob", false)];
        assert!(!watcher.detect_repeated_calls(&calls));

        // Different tools
        let calls = vec![
            make_tool_call("glob", false),
            make_tool_call("read", false),
            make_tool_call("grep", false),
        ];
        assert!(!watcher.detect_repeated_calls(&calls));

        // Same tool, all failures
        let calls = vec![
            make_tool_call("glob", false),
            make_tool_call("glob", false),
            make_tool_call("glob", false),
        ];
        assert!(watcher.detect_repeated_calls(&calls));

        // Same tool, some success
        let calls = vec![
            make_tool_call("glob", true),
            make_tool_call("glob", false),
            make_tool_call("glob", false),
        ];
        assert!(!watcher.detect_repeated_calls(&calls));
    }

    #[test]
    fn test_watcher_is_enabled() {
        let watcher = Watcher::default();
        assert!(watcher.is_enabled());

        let config = WatcherConfig {
            enabled: false,
            ..Default::default()
        };
        let watcher = Watcher::new(config);
        assert!(!watcher.is_enabled());
    }
}
