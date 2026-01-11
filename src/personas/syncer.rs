//! Syncer for cross-task learning relay.
//!
//! Monitors concurrent tasks for learnings and routes relevant
//! discoveries between them in real-time.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::coordination::Knowledge;
use crate::task::TaskId;

/// Configuration for the Syncer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncerConfig {
    /// How often to check for new learnings (default: 15 seconds).
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// Enable syncer (default: true).
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Maximum messages per task per minute.
    #[serde(default = "default_max_messages")]
    pub max_messages_per_task_per_minute: usize,
    /// Cooldown after relay (default: 10 seconds).
    #[serde(default = "default_cooldown")]
    pub cooldown_after_relay_secs: u64,
    /// Relay threshold for blocking discoveries.
    #[serde(default = "default_blocking_threshold")]
    pub blocking_threshold: RelayThreshold,
    /// Relay threshold for helpful discoveries.
    #[serde(default = "default_helpful_threshold")]
    pub helpful_threshold: RelayThreshold,
}

fn default_interval() -> u64 {
    15
}

fn default_enabled() -> bool {
    true
}

fn default_max_messages() -> usize {
    5
}

fn default_cooldown() -> u64 {
    10
}

fn default_blocking_threshold() -> RelayThreshold {
    RelayThreshold::Always
}

fn default_helpful_threshold() -> RelayThreshold {
    RelayThreshold::RelatedOnly
}

impl Default for SyncerConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            enabled: default_enabled(),
            max_messages_per_task_per_minute: default_max_messages(),
            cooldown_after_relay_secs: default_cooldown(),
            blocking_threshold: default_blocking_threshold(),
            helpful_threshold: default_helpful_threshold(),
        }
    }
}

impl SyncerConfig {
    /// Get the interval as a Duration.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.interval_secs)
    }

    /// Get the cooldown as a Duration.
    pub fn cooldown(&self) -> Duration {
        Duration::from_secs(self.cooldown_after_relay_secs)
    }
}

/// When to relay learnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelayThreshold {
    /// Always relay.
    Always,
    /// Only relay to related tasks.
    RelatedOnly,
    /// Only relay to sibling tasks (same parent).
    SameParentOnly,
    /// Never relay.
    Never,
}

/// Urgency of a sync message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncUrgency {
    /// Blocking - task needs this to proceed.
    Blocking,
    /// Helpful - will save time/effort.
    Helpful,
    /// FYI - nice to know.
    Fyi,
}

/// Relevance level of a learning to a recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncRelevance {
    /// Highly relevant.
    High,
    /// Moderately relevant.
    Medium,
    /// Low relevance.
    Low,
}

/// A sync message to be delivered to a task.
#[derive(Debug, Clone)]
pub struct SyncMessage {
    /// Source task that discovered the learning.
    pub source_task: TaskId,
    /// Target task to receive the message.
    pub target_task: TaskId,
    /// The learning being shared.
    pub learning_summary: String,
    /// Relevance to the target task.
    pub relevance: SyncRelevance,
    /// Urgency of the message.
    pub urgency: SyncUrgency,
    /// The message content for the recipient.
    pub message: String,
    /// When the message was created.
    pub created_at: DateTime<Utc>,
}

impl SyncMessage {
    /// Create a new sync message.
    pub fn new(
        source_task: TaskId,
        target_task: TaskId,
        learning_summary: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            source_task,
            target_task,
            learning_summary: learning_summary.into(),
            relevance: SyncRelevance::Medium,
            urgency: SyncUrgency::Helpful,
            message: message.into(),
            created_at: Utc::now(),
        }
    }

    /// Set the relevance.
    pub fn with_relevance(mut self, relevance: SyncRelevance) -> Self {
        self.relevance = relevance;
        self
    }

    /// Set the urgency.
    pub fn with_urgency(mut self, urgency: SyncUrgency) -> Self {
        self.urgency = urgency;
        self
    }

    /// Format as a system message for injection into task context.
    pub fn format_for_injection(&self) -> String {
        format!(
            "<sync-message from=\"{}\" urgency=\"{:?}\">\n{}\n</sync-message>",
            self.source_task.0, self.urgency, self.message
        )
    }
}

/// Result of syncer evaluation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Source of the learning.
    pub learning_source: TaskId,
    /// Summary of what was learned.
    pub learning_summary: String,
    /// Messages to send to recipients.
    pub recipients: Vec<SyncMessage>,
    /// Reason if no recipients.
    pub skip_reason: Option<String>,
}

impl SyncResult {
    /// Create a result with no recipients.
    pub fn skipped(source: TaskId, summary: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            learning_source: source,
            learning_summary: summary.into(),
            recipients: Vec::new(),
            skip_reason: Some(reason.into()),
        }
    }

    /// Create a result with recipients.
    pub fn with_recipients(source: TaskId, summary: impl Into<String>, recipients: Vec<SyncMessage>) -> Self {
        Self {
            learning_source: source,
            learning_summary: summary.into(),
            recipients,
            skip_reason: None,
        }
    }
}

/// Summary of a running task for the Syncer.
#[derive(Debug, Clone)]
pub struct TaskSummary {
    /// Task ID.
    pub task_id: TaskId,
    /// Task description.
    pub description: String,
    /// Task tags.
    pub tags: Vec<String>,
    /// Current focus (last tool call, current file).
    pub current_focus: String,
    /// Recent learnings from this task.
    pub recent_learnings: Vec<Knowledge>,
    /// Working directory.
    pub working_directory: Option<String>,
    /// Parent task (if subtask).
    pub parent_task: Option<TaskId>,
}

/// Relationship between tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskRelationship {
    /// Tasks share a parent.
    SameParent,
    /// Tasks share tags.
    SharedTags,
    /// Tasks work in the same directory.
    SameDirectory,
    /// Tasks are in the same explicit group.
    ExplicitGroup,
    /// No direct relationship.
    None,
}

/// The Syncer monitors tasks and routes learnings between them.
pub struct Syncer {
    /// Configuration.
    config: SyncerConfig,
    /// Recently relayed learnings (to prevent duplicates).
    relayed_learnings: HashMap<String, DateTime<Utc>>,
    /// Message counts per task for rate limiting.
    message_counts: HashMap<TaskId, Vec<DateTime<Utc>>>,
}

impl Syncer {
    /// Create a new Syncer.
    pub fn new(config: SyncerConfig) -> Self {
        Self {
            config,
            relayed_learnings: HashMap::new(),
            message_counts: HashMap::new(),
        }
    }

    /// Get the syncer configuration.
    pub fn config(&self) -> &SyncerConfig {
        &self.config
    }

    /// Check if the syncer is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Get the check interval.
    pub fn interval(&self) -> Duration {
        self.config.interval()
    }

    /// Evaluate a new learning and determine recipients.
    pub fn evaluate_learning(
        &mut self,
        source_task: &TaskSummary,
        learning: &Knowledge,
        other_tasks: &[TaskSummary],
    ) -> SyncResult {
        // Check if we've recently relayed this learning
        let learning_key = format!("{}:{}", source_task.task_id.0, learning.content);
        if let Some(last_relay) = self.relayed_learnings.get(&learning_key) {
            let cooldown = self.config.cooldown();
            if Utc::now().signed_duration_since(*last_relay).num_seconds() < cooldown.as_secs() as i64 {
                return SyncResult::skipped(
                    source_task.task_id.clone(),
                    &learning.content,
                    "Recently relayed (cooldown)",
                );
            }
        }

        // Find relevant recipients
        let mut recipients = Vec::new();

        for target in other_tasks {
            if target.task_id == source_task.task_id {
                continue;
            }

            let relationship = self.determine_relationship(source_task, target);
            let relevance = self.assess_relevance(learning, target);
            let urgency = self.assess_urgency(learning);

            // Check if we should relay based on thresholds
            if !self.should_relay(urgency, relationship) {
                continue;
            }

            // Check rate limit
            if !self.check_rate_limit(&target.task_id) {
                continue;
            }

            // Create sync message
            let message = self.format_message(learning, target);
            recipients.push(
                SyncMessage::new(
                    source_task.task_id.clone(),
                    target.task_id.clone(),
                    &learning.content,
                    message,
                )
                .with_relevance(relevance)
                .with_urgency(urgency),
            );
        }

        if recipients.is_empty() {
            return SyncResult::skipped(source_task.task_id.clone(), &learning.content, "No relevant recipients");
        }

        // Mark as relayed
        self.relayed_learnings.insert(learning_key, Utc::now());

        // Update message counts
        for msg in &recipients {
            self.record_message(&msg.target_task);
        }

        SyncResult::with_recipients(source_task.task_id.clone(), &learning.content, recipients)
    }

    /// Determine the relationship between two tasks.
    fn determine_relationship(&self, source: &TaskSummary, target: &TaskSummary) -> TaskRelationship {
        // Check same parent
        if let (Some(p1), Some(p2)) = (&source.parent_task, &target.parent_task)
            && p1 == p2
        {
            return TaskRelationship::SameParent;
        }

        // Check same directory
        if let (Some(d1), Some(d2)) = (&source.working_directory, &target.working_directory)
            && d1 == d2
        {
            return TaskRelationship::SameDirectory;
        }

        // Check shared tags
        let shared_tags = source.tags.iter().any(|t| target.tags.contains(t));
        if shared_tags {
            return TaskRelationship::SharedTags;
        }

        TaskRelationship::None
    }

    /// Assess relevance of a learning to a target task.
    fn assess_relevance(&self, learning: &Knowledge, target: &TaskSummary) -> SyncRelevance {
        // Check if learning tags match target tags
        let tag_overlap = learning.tags.iter().any(|t| target.tags.contains(t));

        // Check if learning mentions target's focus
        let focus_match = target
            .current_focus
            .split_whitespace()
            .any(|word| learning.content.to_lowercase().contains(&word.to_lowercase()));

        if tag_overlap && focus_match {
            SyncRelevance::High
        } else if tag_overlap || focus_match {
            SyncRelevance::Medium
        } else {
            SyncRelevance::Low
        }
    }

    /// Assess urgency of a learning.
    fn assess_urgency(&self, learning: &Knowledge) -> SyncUrgency {
        let content = learning.content.to_lowercase();

        // Check for blocking keywords
        if content.contains("error")
            || content.contains("panic")
            || content.contains("failed")
            || content.contains("breaking")
        {
            return SyncUrgency::Blocking;
        }

        // Check for helpful keywords
        if content.contains("found")
            || content.contains("discovered")
            || content.contains("solution")
            || content.contains("works")
        {
            return SyncUrgency::Helpful;
        }

        SyncUrgency::Fyi
    }

    /// Check if we should relay based on urgency and relationship.
    fn should_relay(&self, urgency: SyncUrgency, relationship: TaskRelationship) -> bool {
        match urgency {
            SyncUrgency::Blocking => {
                matches!(self.config.blocking_threshold, RelayThreshold::Always)
                    || (matches!(self.config.blocking_threshold, RelayThreshold::RelatedOnly)
                        && relationship != TaskRelationship::None)
                    || (matches!(self.config.blocking_threshold, RelayThreshold::SameParentOnly)
                        && relationship == TaskRelationship::SameParent)
            }
            SyncUrgency::Helpful => {
                matches!(self.config.helpful_threshold, RelayThreshold::Always)
                    || (matches!(self.config.helpful_threshold, RelayThreshold::RelatedOnly)
                        && relationship != TaskRelationship::None)
                    || (matches!(self.config.helpful_threshold, RelayThreshold::SameParentOnly)
                        && relationship == TaskRelationship::SameParent)
            }
            SyncUrgency::Fyi => {
                // FYI only to same parent
                relationship == TaskRelationship::SameParent
            }
        }
    }

    /// Check rate limit for a target task.
    fn check_rate_limit(&self, task_id: &TaskId) -> bool {
        let now = Utc::now();
        let minute_ago = now - chrono::Duration::minutes(1);

        if let Some(counts) = self.message_counts.get(task_id) {
            let recent_count = counts.iter().filter(|&t| *t > minute_ago).count();
            recent_count < self.config.max_messages_per_task_per_minute
        } else {
            true
        }
    }

    /// Record a message sent to a task.
    fn record_message(&mut self, task_id: &TaskId) {
        let now = Utc::now();
        self.message_counts.entry(task_id.clone()).or_default().push(now);

        // Clean up old entries
        let minute_ago = now - chrono::Duration::minutes(1);
        if let Some(counts) = self.message_counts.get_mut(task_id) {
            counts.retain(|&t| t > minute_ago);
        }
    }

    /// Format a message for a recipient.
    fn format_message(&self, learning: &Knowledge, _target: &TaskSummary) -> String {
        format!("FYI from another task working on related code: {}", learning.content)
    }

    /// Clear relayed learning history.
    pub fn clear_history(&mut self) {
        self.relayed_learnings.clear();
        self.message_counts.clear();
    }
}

impl Default for Syncer {
    fn default() -> Self {
        Self::new(SyncerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::KnowledgeKind;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    fn make_task_summary(id: &str, tags: Vec<&str>, parent: Option<&str>) -> TaskSummary {
        TaskSummary {
            task_id: task_id(id),
            description: format!("Task {}", id),
            tags: tags.into_iter().map(String::from).collect(),
            current_focus: "working on files".to_string(),
            recent_learnings: Vec::new(),
            working_directory: Some("/project".to_string()),
            parent_task: parent.map(task_id),
        }
    }

    fn make_learning(content: &str, tags: Vec<&str>) -> Knowledge {
        let tag_strings: Vec<String> = tags.into_iter().map(String::from).collect();
        Knowledge::new(KnowledgeKind::Learning, content, content).with_tags(tag_strings)
    }

    #[test]
    fn test_syncer_config_default() {
        let config = SyncerConfig::default();
        assert_eq!(config.interval_secs, 15);
        assert!(config.enabled);
        assert_eq!(config.max_messages_per_task_per_minute, 5);
    }

    #[test]
    fn test_sync_message_creation() {
        let msg = SyncMessage::new(task_id("source"), task_id("target"), "Found a bug", "Fix the bug in X")
            .with_relevance(SyncRelevance::High)
            .with_urgency(SyncUrgency::Blocking);

        assert_eq!(msg.source_task.0, "source");
        assert_eq!(msg.target_task.0, "target");
        assert_eq!(msg.relevance, SyncRelevance::High);
        assert_eq!(msg.urgency, SyncUrgency::Blocking);
    }

    #[test]
    fn test_sync_message_format() {
        let msg = SyncMessage::new(task_id("task-a"), task_id("task-b"), "Learning", "Check the API")
            .with_urgency(SyncUrgency::Helpful);

        let formatted = msg.format_for_injection();
        assert!(formatted.contains("task-a"));
        assert!(formatted.contains("Helpful"));
        assert!(formatted.contains("Check the API"));
    }

    #[test]
    fn test_syncer_evaluate_learning_no_recipients() {
        let mut syncer = Syncer::default();
        let source = make_task_summary("task-a", vec!["auth"], None);
        let learning = make_learning("Found something", vec!["auth"]);

        // No other tasks
        let result = syncer.evaluate_learning(&source, &learning, &[]);
        assert!(result.recipients.is_empty());
        assert!(result.skip_reason.is_some());
    }

    #[test]
    fn test_syncer_evaluate_learning_with_recipients() {
        let mut syncer = Syncer::default();
        let source = make_task_summary("task-a", vec!["auth"], Some("parent"));
        let target = make_task_summary("task-b", vec!["auth"], Some("parent"));
        let learning = make_learning("API error handling changed", vec!["auth", "api"]);

        let result = syncer.evaluate_learning(&source, &learning, std::slice::from_ref(&target));
        assert!(!result.recipients.is_empty());
        assert!(result.skip_reason.is_none());
    }

    #[test]
    fn test_syncer_rate_limiting() {
        let mut syncer = Syncer::new(SyncerConfig {
            max_messages_per_task_per_minute: 2,
            ..Default::default()
        });

        let source = make_task_summary("task-a", vec!["auth"], Some("parent"));
        let target = make_task_summary("task-b", vec!["auth"], Some("parent"));

        // Send first learning
        let learning1 = make_learning("Learning 1", vec!["auth"]);
        let result1 = syncer.evaluate_learning(&source, &learning1, std::slice::from_ref(&target));
        assert_eq!(result1.recipients.len(), 1);

        // Send second learning
        let learning2 = make_learning("Learning 2", vec!["auth"]);
        let result2 = syncer.evaluate_learning(&source, &learning2, std::slice::from_ref(&target));
        assert_eq!(result2.recipients.len(), 1);

        // Third should be rate limited
        let learning3 = make_learning("Learning 3", vec!["auth"]);
        let result3 = syncer.evaluate_learning(&source, &learning3, std::slice::from_ref(&target));
        assert!(result3.recipients.is_empty());
    }

    #[test]
    fn test_syncer_cooldown() {
        let mut syncer = Syncer::default();
        let source = make_task_summary("task-a", vec!["auth"], Some("parent"));
        let target = make_task_summary("task-b", vec!["auth"], Some("parent"));
        let learning = make_learning("Same learning", vec!["auth"]);

        // First time should work
        let result1 = syncer.evaluate_learning(&source, &learning, std::slice::from_ref(&target));
        assert!(!result1.recipients.is_empty());

        // Same learning immediately should be skipped (cooldown)
        let result2 = syncer.evaluate_learning(&source, &learning, std::slice::from_ref(&target));
        assert!(result2.recipients.is_empty());
        assert!(result2.skip_reason.as_ref().unwrap().contains("cooldown"));
    }

    #[test]
    fn test_syncer_relationship_detection() {
        let syncer = Syncer::default();

        // Same parent
        let t1 = make_task_summary("t1", vec![], Some("parent"));
        let t2 = make_task_summary("t2", vec![], Some("parent"));
        assert_eq!(syncer.determine_relationship(&t1, &t2), TaskRelationship::SameParent);

        // Shared tags (different directories to avoid SameDirectory match)
        let mut t1 = make_task_summary("t1", vec!["auth"], None);
        let mut t2 = make_task_summary("t2", vec!["auth"], None);
        t1.working_directory = Some("/project/a".to_string());
        t2.working_directory = Some("/project/b".to_string());
        assert_eq!(syncer.determine_relationship(&t1, &t2), TaskRelationship::SharedTags);

        // No relationship
        let t1 = make_task_summary("t1", vec!["auth"], None);
        let mut t2 = make_task_summary("t2", vec!["ui"], None);
        t2.working_directory = Some("/other".to_string());
        assert_eq!(syncer.determine_relationship(&t1, &t2), TaskRelationship::None);
    }

    #[test]
    fn test_syncer_urgency_assessment() {
        let syncer = Syncer::default();

        let blocking = make_learning("API error handling failed", vec![]);
        assert_eq!(syncer.assess_urgency(&blocking), SyncUrgency::Blocking);

        let helpful = make_learning("Found the solution in utils", vec![]);
        assert_eq!(syncer.assess_urgency(&helpful), SyncUrgency::Helpful);

        let fyi = make_learning("The config format is YAML", vec![]);
        assert_eq!(syncer.assess_urgency(&fyi), SyncUrgency::Fyi);
    }

    #[test]
    fn test_syncer_clear_history() {
        let mut syncer = Syncer::default();
        let source = make_task_summary("task-a", vec!["auth"], Some("parent"));
        let target = make_task_summary("task-b", vec!["auth"], Some("parent"));
        let learning = make_learning("Learning", vec!["auth"]);

        // Relay once
        syncer.evaluate_learning(&source, &learning, std::slice::from_ref(&target));

        // Clear history
        syncer.clear_history();

        // Should be able to relay again
        let result = syncer.evaluate_learning(&source, &learning, std::slice::from_ref(&target));
        assert!(!result.recipients.is_empty());
    }
}
