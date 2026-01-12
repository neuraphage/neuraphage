//! SupervisedExecutor for task supervision.
//!
//! Wraps TaskExecutor with Watcher and Syncer supervision loops
//! for real-time task health monitoring and cross-task learning.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::coordination::{EventBus, KnowledgeStore};
use crate::coordination::{Knowledge, KnowledgeKind};
use crate::error::{Error, Result};
use crate::executor::{
    ExecutionEvent, ExecutionStatus, ExecutorConfig, InjectedMessage, NudgeSeverity, SyncUrgency, TaskExecutor,
};
use crate::personas::{
    SyncMessage, Syncer, SyncerConfig, TaskSnapshot, TaskSummary, WatchResult, Watcher, WatcherConfig,
    WatcherRecommendation,
};
use crate::task::{Task, TaskId};

/// Configuration for supervised execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisedExecutorConfig {
    /// Base executor config.
    #[serde(flatten)]
    pub executor: ExecutorConfig,
    /// Watcher configuration.
    #[serde(default)]
    pub watcher: WatcherConfig,
    /// Syncer configuration.
    #[serde(default)]
    pub syncer: SyncerConfig,
    /// Enable supervision (can be disabled for testing).
    #[serde(default = "default_supervision_enabled")]
    pub supervision_enabled: bool,
    /// Minimum task age before including in supervision (debounce).
    #[serde(default = "default_min_task_age")]
    pub min_task_age_secs: u64,
}

fn default_supervision_enabled() -> bool {
    true
}

fn default_min_task_age() -> u64 {
    5
}

impl Default for SupervisedExecutorConfig {
    fn default() -> Self {
        Self {
            executor: ExecutorConfig::default(),
            watcher: WatcherConfig::default(),
            syncer: SyncerConfig::default(),
            supervision_enabled: default_supervision_enabled(),
            min_task_age_secs: default_min_task_age(),
        }
    }
}

/// Task health status from heartbeat tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeartbeatFreshness {
    /// < 2 min - active.
    Fresh,
    /// 2-5 min - may need nudge.
    Stale,
    /// > 5 min - likely stuck.
    VeryStale,
    /// No heartbeat recorded.
    Unknown,
}

/// Task heartbeat state for freshness tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskHeartbeat {
    /// Last activity timestamp.
    pub last_activity: DateTime<Utc>,
    /// Last iteration completed.
    pub last_iteration: u32,
    /// Last tool executed.
    pub last_tool: Option<String>,
    /// Consecutive nudges sent without progress.
    pub nudge_count: u32,
    /// Last health status.
    pub last_health: crate::personas::TaskHealth,
}

impl Default for TaskHeartbeat {
    fn default() -> Self {
        Self {
            last_activity: Utc::now(),
            last_iteration: 0,
            last_tool: None,
            nudge_count: 0,
            last_health: crate::personas::TaskHealth::Healthy,
        }
    }
}

/// Supervision health state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SupervisionHealth {
    /// Watcher loop running.
    pub watcher_healthy: bool,
    /// Syncer loop running.
    pub syncer_healthy: bool,
    /// Last successful watcher cycle.
    pub last_watcher_cycle: Option<DateTime<Utc>>,
    /// Last successful syncer cycle.
    pub last_syncer_cycle: Option<DateTime<Utc>>,
    /// In degraded mode.
    pub degraded_mode: bool,
    /// Total watcher cycles.
    pub watcher_cycles: u64,
    /// Total syncer cycles.
    pub syncer_cycles: u64,
    /// Total nudges sent.
    pub nudges_sent: u64,
    /// Total syncs relayed.
    pub syncs_relayed: u64,
    /// Total tasks paused.
    pub tasks_paused: u64,
}

/// Metadata for a running task.
#[derive(Debug, Clone)]
pub struct TaskMetadata {
    /// Task description.
    pub description: String,
    /// Task tags.
    pub tags: Vec<String>,
    /// Working directory.
    pub working_dir: Option<std::path::PathBuf>,
    /// Parent task if subtask.
    pub parent_task: Option<TaskId>,
}

/// SupervisedExecutor wraps TaskExecutor with Watcher and Syncer supervision.
pub struct SupervisedExecutor {
    /// The underlying task executor.
    executor: Arc<Mutex<TaskExecutor>>,
    /// Watcher for stuck detection.
    watcher: Watcher,
    /// Syncer for cross-task learning.
    syncer: Arc<Mutex<Syncer>>,
    /// Knowledge store for learnings.
    knowledge: Arc<KnowledgeStore>,
    /// Event bus for notifications.
    event_bus: Arc<EventBus>,
    /// Configuration.
    config: SupervisedExecutorConfig,
    /// Heartbeat state per task.
    heartbeats: Arc<RwLock<HashMap<TaskId, TaskHeartbeat>>>,
    /// Snapshot cache for tracking changes.
    last_snapshots: Arc<RwLock<HashMap<TaskId, TaskSnapshot>>>,
    /// Supervision health state.
    health: Arc<RwLock<SupervisionHealth>>,
    /// Task start times for debouncing.
    task_start_times: Arc<RwLock<HashMap<TaskId, DateTime<Utc>>>>,
    /// Task metadata for syncer summaries.
    task_metadata: Arc<RwLock<HashMap<TaskId, TaskMetadata>>>,
}

impl SupervisedExecutor {
    /// Create a new supervised executor.
    pub fn new(config: SupervisedExecutorConfig, event_bus: Arc<EventBus>) -> Result<Self> {
        let executor = TaskExecutor::new(config.executor.clone())?;

        Ok(Self {
            executor: Arc::new(Mutex::new(executor)),
            watcher: Watcher::new(config.watcher.clone()),
            syncer: Arc::new(Mutex::new(Syncer::new(config.syncer.clone()))),
            knowledge: Arc::new(KnowledgeStore::new()),
            event_bus,
            config,
            heartbeats: Arc::new(RwLock::new(HashMap::new())),
            last_snapshots: Arc::new(RwLock::new(HashMap::new())),
            health: Arc::new(RwLock::new(SupervisionHealth::default())),
            task_start_times: Arc::new(RwLock::new(HashMap::new())),
            task_metadata: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create with existing components (for testing).
    pub fn with_components(
        executor: TaskExecutor,
        watcher: Watcher,
        syncer: Syncer,
        knowledge: KnowledgeStore,
        event_bus: Arc<EventBus>,
        config: SupervisedExecutorConfig,
    ) -> Self {
        Self {
            executor: Arc::new(Mutex::new(executor)),
            watcher,
            syncer: Arc::new(Mutex::new(syncer)),
            knowledge: Arc::new(knowledge),
            event_bus,
            config,
            heartbeats: Arc::new(RwLock::new(HashMap::new())),
            last_snapshots: Arc::new(RwLock::new(HashMap::new())),
            health: Arc::new(RwLock::new(SupervisionHealth::default())),
            task_start_times: Arc::new(RwLock::new(HashMap::new())),
            task_metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the supervision health state.
    pub async fn health(&self) -> SupervisionHealth {
        self.health.read().await.clone()
    }

    /// Get the underlying executor (for direct access).
    pub fn executor(&self) -> &Arc<Mutex<TaskExecutor>> {
        &self.executor
    }

    /// Get the knowledge store.
    pub fn knowledge(&self) -> &Arc<KnowledgeStore> {
        &self.knowledge
    }

    /// Get the event bus.
    pub fn event_bus(&self) -> &Arc<EventBus> {
        &self.event_bus
    }

    /// Start a task through the supervised executor.
    pub async fn start_task(&self, task: Task, working_dir: Option<std::path::PathBuf>) -> Result<()> {
        let task_id = task.id.clone();
        let description = task.description.clone();
        let tags = task.tags.clone();

        // Start the task in the underlying executor
        {
            let mut executor = self.executor.lock().await;
            executor.start_task(task, working_dir.clone())?;
        }

        // Track start time for debouncing
        {
            let mut start_times = self.task_start_times.write().await;
            start_times.insert(task_id.clone(), Utc::now());
        }

        // Initialize heartbeat
        {
            let mut heartbeats = self.heartbeats.write().await;
            heartbeats.insert(task_id.clone(), TaskHeartbeat::default());
        }

        // Store task metadata for syncer
        {
            let mut metadata = self.task_metadata.write().await;
            metadata.insert(
                task_id,
                TaskMetadata {
                    description,
                    tags,
                    working_dir,
                    parent_task: None,
                },
            );
        }

        Ok(())
    }

    /// Spawn the watcher supervision loop.
    pub fn spawn_watcher_loop(self: &Arc<Self>) -> JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(this.watcher.interval());
            loop {
                interval.tick().await;

                if !this.config.supervision_enabled {
                    continue;
                }

                if let Err(e) = this.run_watcher_cycle_safe().await {
                    error!("Watcher cycle error: {}", e);
                }
            }
        })
    }

    /// Run watcher cycle with degraded mode handling.
    async fn run_watcher_cycle_safe(&self) -> Result<()> {
        match self.run_watcher_cycle().await {
            Ok(()) => {
                let mut health = self.health.write().await;
                health.watcher_healthy = true;
                health.last_watcher_cycle = Some(Utc::now());
                health.watcher_cycles += 1;
                health.degraded_mode = false;
                Ok(())
            }
            Err(e) => {
                warn!("Watcher cycle failed, entering degraded mode: {}", e);
                let mut health = self.health.write().await;
                health.degraded_mode = true;

                // Try degraded cycle
                self.run_degraded_watcher_cycle().await
            }
        }
    }

    /// Run a full watcher cycle.
    async fn run_watcher_cycle(&self) -> Result<()> {
        // Get all running task IDs
        let task_ids = {
            let executor = self.executor.lock().await;
            executor.running_task_ids()
        };

        debug!("Watcher cycle: checking {} running tasks", task_ids.len());

        // Filter out tasks that are too young (debounce)
        let min_age = Duration::from_secs(self.config.min_task_age_secs);
        let now = Utc::now();
        let eligible_tasks: Vec<TaskId> = {
            let start_times = self.task_start_times.read().await;
            task_ids
                .into_iter()
                .filter(|id| {
                    start_times.get(id).is_none_or(|start| {
                        let age = now.signed_duration_since(*start);
                        age.to_std().unwrap_or(Duration::ZERO) >= min_age
                    })
                })
                .collect()
        };

        // Collect snapshots and evaluate each task
        for task_id in eligible_tasks {
            // Skip if task completed during iteration
            let snapshot = match self.collect_task_snapshot(&task_id).await {
                Ok(s) => s,
                Err(Error::TaskNotFound { .. }) => continue,
                Err(e) => {
                    warn!("Failed to collect snapshot for {}: {}", task_id.0, e);
                    continue;
                }
            };

            // Evaluate with Watcher
            let result = self.watcher.evaluate(&snapshot);
            debug!(
                "Watcher evaluated {}: {:?} ({:?})",
                task_id.0, result.status, result.recommendation
            );

            // Update heartbeat health
            {
                let mut heartbeats = self.heartbeats.write().await;
                if let Some(hb) = heartbeats.get_mut(&task_id) {
                    hb.last_health = result.status;
                }
            }

            // Act on recommendation
            if let Err(e) = self.handle_intervention(&task_id, &result).await {
                warn!("Failed to handle intervention for {}: {}", task_id.0, e);
            }

            // Cache snapshot
            {
                let mut snapshots = self.last_snapshots.write().await;
                snapshots.insert(task_id, snapshot);
            }
        }

        Ok(())
    }

    /// Degraded mode: mechanical checks only, no heuristics.
    async fn run_degraded_watcher_cycle(&self) -> Result<()> {
        let task_ids = {
            let executor = self.executor.lock().await;
            executor.running_task_ids()
        };

        for task_id in task_ids {
            let freshness = self.heartbeat_freshness(&task_id).await;

            match freshness {
                HeartbeatFreshness::VeryStale => {
                    warn!("Task {} very stale in degraded mode, pausing", task_id.0);
                    let _ = self
                        .pause_task(&task_id, "Degraded mode: no activity for >5 minutes")
                        .await;
                }
                HeartbeatFreshness::Stale => {
                    let _ = self
                        .inject_nudge(
                            &task_id,
                            "System check: are you making progress?".into(),
                            NudgeSeverity::Hint,
                        )
                        .await;
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Collect a task snapshot for watcher evaluation.
    pub async fn collect_task_snapshot(&self, task_id: &TaskId) -> Result<TaskSnapshot> {
        let executor = self.executor.lock().await;

        // Get execution state
        let state = executor
            .get_state(task_id)
            .ok_or_else(|| Error::TaskNotFound { id: task_id.0.clone() })?;

        // Get iteration and token info from status
        let (iteration, tokens_used) = match &state.status {
            ExecutionStatus::Running {
                iteration, tokens_used, ..
            } => (*iteration as usize, *tokens_used as usize),
            _ => (0, 0),
        };

        // Get heartbeat for tool info
        let heartbeat = self.heartbeats.read().await;
        let hb = heartbeat.get(task_id);

        // Calculate time since progress
        let time_since_progress = Utc::now()
            .signed_duration_since(state.last_activity)
            .to_std()
            .unwrap_or(Duration::from_secs(0));

        // Build recent tool calls from heartbeat
        let recent_tool_calls = if let Some(hb) = hb {
            if let Some(tool_name) = &hb.last_tool {
                vec![crate::personas::ToolCallSummary {
                    name: tool_name.clone(),
                    success: true,
                    duration: Duration::from_millis(100),
                    result_summary: String::new(),
                }]
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Check for file changes (placeholder - would need event tracking)
        let has_file_changes = false;

        Ok(TaskSnapshot {
            task_id: task_id.clone(),
            iteration,
            tokens_used,
            recent_tool_calls,
            time_since_progress,
            recent_conversation: String::new(),
            has_file_changes,
        })
    }

    /// Handle Watcher intervention based on recommendation.
    async fn handle_intervention(&self, task_id: &TaskId, result: &WatchResult) -> Result<()> {
        let nudge_count = {
            let heartbeats = self.heartbeats.read().await;
            heartbeats.get(task_id).map(|h| h.nudge_count).unwrap_or(0)
        };

        match result.recommendation {
            WatcherRecommendation::Continue => {}

            WatcherRecommendation::Nudge => {
                // Escalate severity based on nudge count
                let severity = match nudge_count {
                    0 => NudgeSeverity::Hint,
                    1..=2 => NudgeSeverity::Warning,
                    _ => NudgeSeverity::Critical,
                };

                // After 5 nudges, escalate to pause
                if nudge_count >= 5 {
                    self.pause_task(task_id, "Unresponsive after 5 nudges").await?;
                    return Ok(());
                }

                if let Some(msg) = &result.nudge_message {
                    self.inject_nudge(task_id, msg.clone(), severity).await?;
                }

                // Increment nudge counter
                {
                    let mut heartbeats = self.heartbeats.write().await;
                    if let Some(hb) = heartbeats.get_mut(task_id) {
                        hb.nudge_count += 1;
                    }
                }
            }

            WatcherRecommendation::Escalate => {
                // For now, treat as critical nudge
                self.inject_nudge(
                    task_id,
                    "This task needs escalation. Consider asking the user for help.".into(),
                    NudgeSeverity::Critical,
                )
                .await?;
            }

            WatcherRecommendation::Pause => {
                self.pause_task(task_id, &result.diagnosis).await?;
            }

            WatcherRecommendation::Abort => {
                self.abort_task(task_id, &result.diagnosis).await?;
            }
        }

        Ok(())
    }

    /// Check heartbeat freshness for a task.
    pub async fn heartbeat_freshness(&self, task_id: &TaskId) -> HeartbeatFreshness {
        let heartbeats = self.heartbeats.read().await;
        let Some(heartbeat) = heartbeats.get(task_id) else {
            return HeartbeatFreshness::Unknown;
        };

        let age = Utc::now().signed_duration_since(heartbeat.last_activity);
        let minutes = age.num_minutes();

        match minutes {
            0..=2 => HeartbeatFreshness::Fresh,
            3..=5 => HeartbeatFreshness::Stale,
            _ => HeartbeatFreshness::VeryStale,
        }
    }

    /// Inject a nudge message into a task.
    pub async fn inject_nudge(&self, task_id: &TaskId, message: String, severity: NudgeSeverity) -> Result<()> {
        info!("Injecting {:?} nudge to {}: {}", severity, task_id.0, message);

        let executor = self.executor.lock().await;
        executor
            .inject_message(task_id, InjectedMessage::Nudge { message, severity })
            .await?;

        // Update stats
        {
            let mut health = self.health.write().await;
            health.nudges_sent += 1;
        }

        Ok(())
    }

    /// Inject a sync message into a task.
    pub async fn inject_sync(&self, task_id: &TaskId, msg: &SyncMessage) -> Result<()> {
        info!(
            "Injecting sync from {} to {}: {}",
            msg.source_task.0, task_id.0, msg.learning_summary
        );

        let executor = self.executor.lock().await;
        executor
            .inject_message(
                task_id,
                InjectedMessage::Sync {
                    source_task: msg.source_task.clone(),
                    message: msg.message.clone(),
                    urgency: match msg.urgency {
                        crate::personas::SyncUrgency::Blocking => SyncUrgency::Blocking,
                        crate::personas::SyncUrgency::Helpful => SyncUrgency::Helpful,
                        crate::personas::SyncUrgency::Fyi => SyncUrgency::Fyi,
                    },
                },
            )
            .await?;

        // Update stats
        {
            let mut health = self.health.write().await;
            health.syncs_relayed += 1;
        }

        Ok(())
    }

    /// Pause a task.
    pub async fn pause_task(&self, task_id: &TaskId, reason: &str) -> Result<()> {
        info!("Pausing task {}: {}", task_id.0, reason);

        let executor = self.executor.lock().await;
        executor
            .inject_message(
                task_id,
                InjectedMessage::Pause {
                    reason: reason.to_string(),
                },
            )
            .await?;

        // Update stats
        {
            let mut health = self.health.write().await;
            health.tasks_paused += 1;
        }

        Ok(())
    }

    /// Abort a task (for now, same as pause).
    pub async fn abort_task(&self, task_id: &TaskId, reason: &str) -> Result<()> {
        warn!("Aborting task {}: {}", task_id.0, reason);
        self.pause_task(task_id, &format!("Aborted: {}", reason)).await
    }

    /// Update heartbeat from execution events.
    pub async fn update_heartbeat(&self, task_id: &TaskId, events: &[ExecutionEvent]) {
        let mut heartbeats = self.heartbeats.write().await;
        let heartbeat = heartbeats.entry(task_id.clone()).or_default();

        for event in events {
            match event {
                ExecutionEvent::IterationComplete { iteration, .. } => {
                    heartbeat.last_activity = Utc::now();
                    heartbeat.last_iteration = *iteration;
                    heartbeat.nudge_count = 0; // Reset on progress
                }
                ExecutionEvent::ToolCompleted { name, .. } => {
                    heartbeat.last_tool = Some(name.clone());
                }
                _ => {}
            }
        }
    }

    /// Spawn the syncer supervision loop.
    pub fn spawn_syncer_loop(self: &Arc<Self>) -> JoinHandle<()> {
        let this = Arc::clone(self);
        let syncer_config = this.config.syncer.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(syncer_config.interval());
            loop {
                interval.tick().await;

                if !this.config.supervision_enabled || !syncer_config.enabled {
                    continue;
                }

                if let Err(e) = this.run_syncer_cycle_safe().await {
                    error!("Syncer cycle error: {}", e);
                }
            }
        })
    }

    /// Run syncer cycle with error handling.
    async fn run_syncer_cycle_safe(&self) -> Result<()> {
        match self.run_syncer_cycle().await {
            Ok(()) => {
                let mut health = self.health.write().await;
                health.syncer_healthy = true;
                health.last_syncer_cycle = Some(Utc::now());
                health.syncer_cycles += 1;
                Ok(())
            }
            Err(e) => {
                warn!("Syncer cycle failed: {}", e);
                Ok(()) // Syncer failures are non-fatal
            }
        }
    }

    /// Run a full syncer cycle.
    async fn run_syncer_cycle(&self) -> Result<()> {
        // Get all running task IDs
        let task_ids = {
            let executor = self.executor.lock().await;
            executor.running_task_ids()
        };

        if task_ids.len() < 2 {
            // Need at least 2 tasks for syncing
            return Ok(());
        }

        debug!("Syncer cycle: {} running tasks", task_ids.len());

        // Build task summaries
        let task_summaries = self.build_task_summaries(&task_ids).await;

        // Process learnings from the knowledge store
        // Get recent learnings (using search with empty query to get all)
        let learnings = self.knowledge.search("").await;

        // Only process recent learnings (last 10)
        for learning in learnings.into_iter().take(10) {
            if let Some(source_task_id) = &learning.source_task {
                let source_summary = task_summaries.iter().find(|t| &t.task_id == source_task_id);

                if let Some(source) = source_summary {
                    // Evaluate who should receive this learning
                    let mut syncer = self.syncer.lock().await;
                    let result = syncer.evaluate_learning(source, &learning, &task_summaries);

                    // Deliver to recipients
                    for msg in result.recipients {
                        if let Err(e) = self.inject_sync(&msg.target_task, &msg).await {
                            warn!("Failed to inject sync to {}: {}", msg.target_task.0, e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Build task summaries for syncer evaluation.
    async fn build_task_summaries(&self, task_ids: &[TaskId]) -> Vec<TaskSummary> {
        let metadata = self.task_metadata.read().await;
        let heartbeats = self.heartbeats.read().await;

        task_ids
            .iter()
            .filter_map(|id| {
                let meta = metadata.get(id)?;
                let hb = heartbeats.get(id);

                Some(TaskSummary {
                    task_id: id.clone(),
                    description: meta.description.clone(),
                    tags: meta.tags.clone(),
                    current_focus: hb.map(|h| h.last_tool.clone().unwrap_or_default()).unwrap_or_default(),
                    recent_learnings: Vec::new(), // Populated separately
                    working_directory: meta.working_dir.as_ref().map(|p| p.display().to_string()),
                    parent_task: meta.parent_task.clone(),
                })
            })
            .collect()
    }

    /// Extract learnings from tool results.
    ///
    /// Looks for patterns that indicate discoveries:
    /// - File operations that reveal structure
    /// - Error messages that indicate gotchas
    /// - Success patterns that could help others
    pub fn extract_learnings(&self, task_id: &TaskId, events: &[ExecutionEvent]) -> Vec<Knowledge> {
        let mut learnings = Vec::new();

        for event in events {
            match event {
                ExecutionEvent::ToolCompleted { name, result } => {
                    // Extract learnings from specific tool results
                    if let Some(learning) = self.extract_learning_from_tool(task_id, name, result) {
                        learnings.push(learning);
                    }
                }
                ExecutionEvent::Failed { error } => {
                    // Errors are often valuable learnings
                    let learning = Knowledge::new(
                        KnowledgeKind::ErrorResolution,
                        format!("Task encountered error: {}", error),
                        error,
                    )
                    .from_task(task_id.clone())
                    .with_relevance(0.8);
                    learnings.push(learning);
                }
                _ => {}
            }
        }

        learnings
    }

    /// Extract learning from a specific tool result.
    fn extract_learning_from_tool(&self, task_id: &TaskId, tool_name: &str, result: &str) -> Option<Knowledge> {
        // Look for significant patterns
        match tool_name {
            "read_file" | "glob" | "grep" => {
                // File discovery - potentially useful to others
                if result.contains("found") || result.contains("matches") {
                    let learning = Knowledge::new(
                        KnowledgeKind::Fact,
                        format!("File discovery via {}: {}", tool_name, truncate(result, 100)),
                        result,
                    )
                    .from_task(task_id.clone())
                    .with_tags(vec!["file-discovery".to_string()]);
                    return Some(learning);
                }
            }
            "bash" => {
                // Command results - errors are especially valuable
                if result.contains("error") || result.contains("Error") || result.contains("failed") {
                    let learning = Knowledge::new(
                        KnowledgeKind::ErrorResolution,
                        format!("Command error: {}", truncate(result, 100)),
                        result,
                    )
                    .from_task(task_id.clone())
                    .with_relevance(0.9)
                    .with_tags(vec!["command-error".to_string()]);
                    return Some(learning);
                }
            }
            "write_file" | "edit_file" => {
                // File changes - note what was modified
                let learning = Knowledge::new(
                    KnowledgeKind::Fact,
                    format!("File modification via {}", tool_name),
                    result,
                )
                .from_task(task_id.clone())
                .with_tags(vec!["file-change".to_string()]);
                return Some(learning);
            }
            _ => {}
        }

        None
    }

    /// Record a learning from a task.
    pub async fn record_learning(&self, learning: Knowledge) {
        let _ = self.knowledge.store(learning).await;
    }

    /// Process and record learnings from events.
    pub async fn process_events_for_learnings(&self, task_id: &TaskId, events: &[ExecutionEvent]) {
        let learnings = self.extract_learnings(task_id, events);
        for learning in learnings {
            self.record_learning(learning).await;
        }
    }

    /// Clean up after task completion.
    pub async fn cleanup_task(&self, task_id: &TaskId) {
        // Remove heartbeat
        {
            let mut heartbeats = self.heartbeats.write().await;
            heartbeats.remove(task_id);
        }

        // Remove snapshot cache
        {
            let mut snapshots = self.last_snapshots.write().await;
            snapshots.remove(task_id);
        }

        // Remove start time
        {
            let mut start_times = self.task_start_times.write().await;
            start_times.remove(task_id);
        }

        // Remove task metadata
        {
            let mut metadata = self.task_metadata.write().await;
            metadata.remove(task_id);
        }
    }
}

/// Truncate a string for display.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len { s } else { &s[..max_len] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::EventBus;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    #[test]
    fn test_supervised_executor_config_default() {
        let config = SupervisedExecutorConfig::default();
        assert!(config.supervision_enabled);
        assert_eq!(config.min_task_age_secs, 5);
    }

    #[test]
    fn test_heartbeat_freshness() {
        // Fresh: 0-2 minutes
        // Stale: 3-5 minutes
        // VeryStale: >5 minutes
    }

    #[test]
    fn test_task_heartbeat_default() {
        let hb = TaskHeartbeat::default();
        assert_eq!(hb.nudge_count, 0);
        assert!(hb.last_tool.is_none());
    }

    #[test]
    fn test_supervision_health_default() {
        let health = SupervisionHealth::default();
        assert!(!health.watcher_healthy);
        assert!(!health.syncer_healthy);
        assert!(!health.degraded_mode);
        assert_eq!(health.nudges_sent, 0);
    }

    #[tokio::test]
    async fn test_supervised_executor_new() {
        let config = SupervisedExecutorConfig::default();
        let event_bus = Arc::new(EventBus::new());
        let executor = SupervisedExecutor::new(config, event_bus);
        assert!(executor.is_ok());
    }

    #[tokio::test]
    async fn test_heartbeat_freshness_unknown() {
        let config = SupervisedExecutorConfig::default();
        let event_bus = Arc::new(EventBus::new());
        let executor = SupervisedExecutor::new(config, event_bus).unwrap();

        // Unknown task should return Unknown freshness
        let freshness = executor.heartbeat_freshness(&task_id("unknown")).await;
        assert_eq!(freshness, HeartbeatFreshness::Unknown);
    }

    #[tokio::test]
    async fn test_update_heartbeat() {
        let config = SupervisedExecutorConfig::default();
        let event_bus = Arc::new(EventBus::new());
        let executor = Arc::new(SupervisedExecutor::new(config, event_bus).unwrap());

        let tid = task_id("test-task");

        // Initialize heartbeat
        {
            let mut heartbeats = executor.heartbeats.write().await;
            heartbeats.insert(tid.clone(), TaskHeartbeat::default());
        }

        // Update with events
        executor
            .update_heartbeat(
                &tid,
                &[
                    ExecutionEvent::IterationComplete {
                        iteration: 5,
                        tokens_used: 1000,
                        cost: 0.01,
                    },
                    ExecutionEvent::ToolCompleted {
                        name: "read_file".to_string(),
                        result: "ok".to_string(),
                    },
                ],
            )
            .await;

        // Verify heartbeat was updated
        let heartbeats = executor.heartbeats.read().await;
        let hb = heartbeats.get(&tid).unwrap();
        assert_eq!(hb.last_iteration, 5);
        assert_eq!(hb.last_tool, Some("read_file".to_string()));
        assert_eq!(hb.nudge_count, 0); // Reset on progress
    }

    #[tokio::test]
    async fn test_cleanup_task() {
        let config = SupervisedExecutorConfig::default();
        let event_bus = Arc::new(EventBus::new());
        let executor = Arc::new(SupervisedExecutor::new(config, event_bus).unwrap());

        let tid = task_id("test-task");

        // Add some state
        {
            let mut heartbeats = executor.heartbeats.write().await;
            heartbeats.insert(tid.clone(), TaskHeartbeat::default());
        }
        {
            let mut start_times = executor.task_start_times.write().await;
            start_times.insert(tid.clone(), Utc::now());
        }

        // Clean up
        executor.cleanup_task(&tid).await;

        // Verify cleaned up
        assert!(executor.heartbeats.read().await.get(&tid).is_none());
        assert!(executor.task_start_times.read().await.get(&tid).is_none());
    }
}
