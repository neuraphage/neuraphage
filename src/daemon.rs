//! Neuraphage daemon for concurrent task execution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::coordination::EventBus;
use crate::error::{Error, Result};
use crate::executor::{Activity, ExecutionEvent, ExecutionStatus, ExecutorConfig};
use crate::git::{MergeQueue, MergeRequest, WorktreeInfo};
use crate::supervised::{SupervisedExecutor, SupervisedExecutorConfig};
use crate::task::{Task, TaskId, TaskStatus};
use crate::task_manager::TaskManager;

/// DTO for worktree info (serializable version of WorktreeInfo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfoDto {
    /// Task ID this worktree belongs to.
    pub task_id: String,
    /// Path to the worktree.
    pub path: String,
    /// Branch name.
    pub branch: String,
    /// Path to the main repository.
    pub repo_path: String,
    /// When the worktree was created.
    pub created_at: String,
}

impl From<&WorktreeInfo> for WorktreeInfoDto {
    fn from(info: &WorktreeInfo) -> Self {
        Self {
            task_id: info.task_id.0.clone(),
            path: info.path.to_string_lossy().to_string(),
            branch: info.branch.clone(),
            repo_path: info.repo_path.to_string_lossy().to_string(),
            created_at: info.created_at.to_rfc3339(),
        }
    }
}

/// DTO for recoverable task info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverableTaskDto {
    /// Task ID.
    pub task_id: String,
    /// Iteration the task was on.
    pub iteration: u32,
    /// Tokens used when checkpointed.
    pub tokens_used: u64,
    /// Cost when checkpointed.
    pub cost: f64,
    /// Phase of execution.
    pub phase: String,
    /// Working directory.
    pub working_dir: String,
    /// When the checkpoint was taken.
    pub checkpoint_at: String,
    /// Why the task was started.
    pub started_reason: String,
}

/// Daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Path to the Unix socket.
    pub socket_path: PathBuf,
    /// Path to the PID file.
    pub pid_path: PathBuf,
    /// Path to the data directory (for engram store).
    pub data_path: PathBuf,
    /// Enable supervised execution with Watcher and Syncer.
    #[serde(default = "default_supervision_enabled")]
    pub supervision_enabled: bool,
}

fn default_supervision_enabled() -> bool {
    true
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let base = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("neuraphage");

        Self {
            socket_path: base.join("neuraphage.sock"),
            pid_path: base.join("neuraphage.pid"),
            data_path: base.clone(),
            supervision_enabled: true,
        }
    }
}

impl DaemonConfig {
    /// Create config from a data directory path.
    pub fn from_path(path: &Path) -> Self {
        Self {
            socket_path: path.join("neuraphage.sock"),
            pid_path: path.join("neuraphage.pid"),
            data_path: path.to_path_buf(),
            supervision_enabled: true,
        }
    }
}

/// Request to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Ping to check if daemon is alive.
    Ping,
    /// Create a new task.
    CreateTask {
        description: String,
        priority: u8,
        tags: Vec<String>,
        context: Option<String>,
        working_dir: Option<String>,
    },
    /// Create a new task with automatic worktree setup.
    CreateTaskWithWorktree {
        description: String,
        priority: u8,
        tags: Vec<String>,
        context: Option<String>,
        repo_path: String,
    },
    /// Get a task by ID.
    GetTask { id: String },
    /// List tasks, optionally filtered.
    ListTasks { status: Option<String> },
    /// Get ready tasks.
    ReadyTasks,
    /// Get blocked tasks.
    BlockedTasks,
    /// Set task status.
    SetStatus { id: String, status: String },
    /// Close a task.
    CloseTask {
        id: String,
        status: String,
        reason: Option<String>,
    },
    /// Add dependency.
    AddDependency { blocked_id: String, blocker_id: String },
    /// Remove dependency.
    RemoveDependency { blocked_id: String, blocker_id: String },
    /// Get task counts.
    TaskCounts,
    /// Start executing a task.
    StartTask { id: String, working_dir: Option<String> },
    /// Provide user input to a waiting task.
    ProvideInput { id: String, input: String },
    /// Attach to a task to receive execution updates.
    AttachTask { id: String },
    /// Get execution status for a task.
    GetExecutionStatus { id: String },
    /// Cancel a running task.
    CancelTask { id: String },
    /// Get worktree info for a task.
    GetWorktreeInfo { id: String },
    /// List all active worktrees.
    ListWorktrees,
    /// Get merge queue status.
    MergeQueueStatus,
    /// Enqueue a task for merging.
    EnqueueMerge { id: String, target_branch: Option<String> },
    /// Get recovery report (tasks that were running when daemon crashed).
    GetRecoveryReport,
    /// Resume a task from its last checkpoint.
    ResumeTask { id: String },
    /// Shutdown daemon.
    Shutdown,
}

/// Response from the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// Pong response.
    Pong,
    /// Success with optional message.
    Ok { message: Option<String> },
    /// Single task response.
    Task(Option<Task>),
    /// Multiple tasks response.
    Tasks(Vec<Task>),
    /// Task counts response.
    Counts {
        queued: usize,
        running: usize,
        waiting: usize,
        blocked: usize,
        paused: usize,
        completed: usize,
        failed: usize,
        cancelled: usize,
    },
    /// Error response.
    Error { message: String },
    /// Execution update (for attached clients) - single event.
    ExecutionUpdate { task_id: String, event: ExecutionEventDto },
    /// Execution events (for attached clients) - multiple events.
    ExecutionEvents {
        task_id: String,
        events: Vec<ExecutionEventDto>,
    },
    /// Task waiting for input.
    WaitingForInput { task_id: String, prompt: String },
    /// Execution status response.
    ExecutionStatusResponse {
        task_id: String,
        status: ExecutionStatusDto,
    },
    /// Task started executing.
    TaskStarted { task_id: String },
    /// Worktree info response.
    WorktreeInfo(Option<WorktreeInfoDto>),
    /// List of active worktrees.
    Worktrees(Vec<WorktreeInfoDto>),
    /// Merge queue status response.
    MergeQueueStatusResponse { pending: usize, active: Option<String> },
    /// Task enqueued for merge.
    MergeEnqueued { task_id: String },
    /// Recovery report response.
    RecoveryReport { tasks: Vec<RecoverableTaskDto> },
    /// Task resumed from checkpoint.
    TaskResumed { task_id: String },
    /// Shutdown acknowledgment.
    Shutdown,
}

/// DTO for activity (serializable version of Activity).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ActivityDto {
    /// Calling the LLM API.
    Thinking,
    /// Streaming text response.
    Streaming,
    /// Executing a tool.
    ExecutingTool { name: String },
    /// Waiting for external response.
    WaitingForTool { name: String },
    /// Idle between iterations.
    Idle,
}

impl From<Activity> for ActivityDto {
    fn from(activity: Activity) -> Self {
        match activity {
            Activity::Thinking => ActivityDto::Thinking,
            Activity::Streaming => ActivityDto::Streaming,
            Activity::ExecutingTool { name } => ActivityDto::ExecutingTool { name },
            Activity::WaitingForTool { name } => ActivityDto::WaitingForTool { name },
            Activity::Idle => ActivityDto::Idle,
        }
    }
}

/// DTO for execution events (serializable version of ExecutionEvent).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionEventDto {
    /// Iteration completed.
    IterationComplete {
        iteration: u32,
        tokens_used: u64,
        cost: f64,
    },
    /// Tool was called.
    ToolCalled { name: String, result: String },
    /// LLM response received.
    LlmResponse { content: String },
    /// Streaming text delta from LLM.
    TextDelta { content: String },
    /// Waiting for user input.
    WaitingForUser { prompt: String },
    /// Task completed.
    Completed { reason: String },
    /// Task failed.
    Failed { error: String },
    /// Activity changed.
    ActivityChanged { activity: ActivityDto },
    /// Tool execution started.
    ToolStarted { name: String },
    /// Tool execution completed.
    ToolCompleted { name: String, result: String },
}

impl From<ExecutionEvent> for ExecutionEventDto {
    fn from(event: ExecutionEvent) -> Self {
        match event {
            ExecutionEvent::IterationComplete {
                iteration,
                tokens_used,
                cost,
            } => ExecutionEventDto::IterationComplete {
                iteration,
                tokens_used,
                cost,
            },
            ExecutionEvent::ToolCalled { name, result } => ExecutionEventDto::ToolCalled { name, result },
            ExecutionEvent::LlmResponse { content } => ExecutionEventDto::LlmResponse { content },
            ExecutionEvent::TextDelta { content } => ExecutionEventDto::TextDelta { content },
            ExecutionEvent::WaitingForUser { prompt } => ExecutionEventDto::WaitingForUser { prompt },
            ExecutionEvent::Completed { reason } => ExecutionEventDto::Completed { reason },
            ExecutionEvent::Failed { error } => ExecutionEventDto::Failed { error },
            ExecutionEvent::ActivityChanged { activity } => ExecutionEventDto::ActivityChanged {
                activity: activity.into(),
            },
            ExecutionEvent::ToolStarted { name } => ExecutionEventDto::ToolStarted { name },
            ExecutionEvent::ToolCompleted { name, result } => ExecutionEventDto::ToolCompleted { name, result },
        }
    }
}

/// DTO for execution status (serializable version of ExecutionStatus).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionStatusDto {
    /// Task is currently running.
    Running {
        iteration: u32,
        tokens_used: u64,
        cost: f64,
    },
    /// Task is waiting for user input.
    WaitingForUser { prompt: String },
    /// Task completed successfully.
    Completed { reason: String },
    /// Task failed.
    Failed { error: String },
    /// Task was cancelled.
    Cancelled,
    /// Task is not currently executing.
    NotRunning,
}

impl From<&ExecutionStatus> for ExecutionStatusDto {
    fn from(status: &ExecutionStatus) -> Self {
        match status {
            ExecutionStatus::Running {
                iteration,
                tokens_used,
                cost,
            } => ExecutionStatusDto::Running {
                iteration: *iteration,
                tokens_used: *tokens_used,
                cost: *cost,
            },
            ExecutionStatus::WaitingForUser { prompt } => ExecutionStatusDto::WaitingForUser { prompt: prompt.clone() },
            ExecutionStatus::Completed { reason } => ExecutionStatusDto::Completed { reason: reason.clone() },
            ExecutionStatus::Failed { error } => ExecutionStatusDto::Failed { error: error.clone() },
            ExecutionStatus::Cancelled => ExecutionStatusDto::Cancelled,
        }
    }
}

/// The neuraphage daemon.
pub struct Daemon {
    config: DaemonConfig,
    manager: Arc<Mutex<TaskManager>>,
    supervised_executor: Arc<SupervisedExecutor>,
    #[allow(dead_code)]
    event_bus: Arc<EventBus>,
    merge_queue: Arc<Mutex<MergeQueue>>,
    shutdown: tokio::sync::broadcast::Sender<()>,
}

impl Daemon {
    /// Create a new daemon with the given configuration.
    pub fn new(config: DaemonConfig) -> Result<Self> {
        // Ensure data directory exists
        std::fs::create_dir_all(&config.data_path)?;

        // Initialize or open task manager
        let manager = if config.data_path.join(".engram").exists() {
            TaskManager::open(&config.data_path)?
        } else {
            TaskManager::init(&config.data_path)?
        };

        // Initialize event bus
        let event_bus = Arc::new(EventBus::new());

        // Initialize supervised executor
        let supervised_config = SupervisedExecutorConfig {
            executor: ExecutorConfig {
                data_dir: config.data_path.clone(),
                ..Default::default()
            },
            supervision_enabled: config.supervision_enabled,
            ..Default::default()
        };
        let supervised_executor = Arc::new(SupervisedExecutor::new(supervised_config, Arc::clone(&event_bus))?);

        // Initialize merge queue
        let merge_queue = MergeQueue::new();

        let (shutdown, _) = tokio::sync::broadcast::channel(1);

        Ok(Self {
            config,
            manager: Arc::new(Mutex::new(manager)),
            supervised_executor,
            event_bus,
            merge_queue: Arc::new(Mutex::new(merge_queue)),
            shutdown,
        })
    }

    /// Run the daemon.
    pub async fn run(&self) -> Result<()> {
        // Remove existing socket if present
        if self.config.socket_path.exists() {
            std::fs::remove_file(&self.config.socket_path)?;
        }

        // Write PID file
        std::fs::write(&self.config.pid_path, std::process::id().to_string())?;

        // Check for recoverable tasks from previous session
        let _ = self.recover().await;

        // Spawn supervision loops if enabled
        let _watcher_handle = if self.config.supervision_enabled {
            log::info!("Starting supervision loops (watcher + syncer)");
            Some(self.supervised_executor.spawn_watcher_loop())
        } else {
            None
        };

        let _syncer_handle = if self.config.supervision_enabled {
            Some(self.supervised_executor.spawn_syncer_loop())
        } else {
            None
        };

        // Spawn completion handler loop
        let _completion_handle = self.spawn_completion_loop();

        // Spawn checkpoint loop for crash recovery
        let _checkpoint_handle = self.spawn_checkpoint_loop();

        // Bind to socket
        let listener = UnixListener::bind(&self.config.socket_path)?;
        log::info!("Daemon listening on {:?}", self.config.socket_path);

        let mut shutdown_rx = self.shutdown.subscribe();

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _)) => {
                            let manager = Arc::clone(&self.manager);
                            let supervised_executor = Arc::clone(&self.supervised_executor);
                            let merge_queue = Arc::clone(&self.merge_queue);
                            let shutdown_tx = self.shutdown.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, manager, supervised_executor, merge_queue, shutdown_tx).await {
                                    log::error!("Connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            log::error!("Accept error: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    log::info!("Shutdown signal received");
                    break;
                }
            }
        }

        // Cleanup
        self.cleanup()?;
        Ok(())
    }

    /// Clean up daemon resources.
    fn cleanup(&self) -> Result<()> {
        if self.config.socket_path.exists() {
            std::fs::remove_file(&self.config.socket_path)?;
        }
        if self.config.pid_path.exists() {
            std::fs::remove_file(&self.config.pid_path)?;
        }
        Ok(())
    }

    /// Check for recoverable tasks on startup.
    ///
    /// Returns the list of tasks that were running when the daemon crashed.
    /// These tasks can be resumed using the ResumeTask request.
    pub async fn recover(&self) -> Result<Vec<RecoverableTaskDto>> {
        let exec = self.supervised_executor.executor().lock().await;
        let states = exec.state_store().list_all().await?;

        let tasks: Vec<RecoverableTaskDto> = states
            .into_iter()
            .map(|s| RecoverableTaskDto {
                task_id: s.task_id,
                iteration: s.iteration,
                tokens_used: s.tokens_used,
                cost: s.cost,
                phase: s.phase,
                working_dir: s.working_dir.to_string_lossy().to_string(),
                checkpoint_at: s.checkpoint_at.to_rfc3339(),
                started_reason: s.started_reason,
            })
            .collect();

        if !tasks.is_empty() {
            log::warn!("Found {} recoverable task(s) from previous session:", tasks.len());
            for task in &tasks {
                log::warn!(
                    "  - {} (iteration {}, phase {}, checkpoint {})",
                    task.task_id,
                    task.iteration,
                    task.phase,
                    task.checkpoint_at
                );
            }
            log::warn!("Use 'np recover' to see recovery report and 'np resume <id>' to resume");
        }

        Ok(tasks)
    }

    /// Spawn the completion handler loop.
    ///
    /// Polls for completed tasks and updates TaskManager status and removes
    /// execution state. This keeps the daemon as the coordinator between
    /// TaskManager (engram) and Executor (runtime) without coupling them directly.
    fn spawn_completion_loop(&self) -> tokio::task::JoinHandle<()> {
        let manager = Arc::clone(&self.manager);
        let supervised_executor = Arc::clone(&self.supervised_executor);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));

            loop {
                interval.tick().await;

                // Poll for completed tasks
                let completed = {
                    let mut exec = supervised_executor.executor().lock().await;
                    exec.poll_completed().await
                };

                // Handle each completed task
                for result in completed {
                    let task_id = result.task_id.clone();
                    log::info!("Task {} completed with status: {:?}", task_id.0, result.status);

                    // Update TaskManager status
                    let new_status = match &result.status {
                        ExecutionStatus::Completed { reason: _ } => TaskStatus::Completed,
                        ExecutionStatus::Failed { error: _ } => TaskStatus::Failed,
                        ExecutionStatus::Cancelled => TaskStatus::Cancelled,
                        _ => TaskStatus::Running, // shouldn't happen
                    };

                    // Close the task in TaskManager
                    {
                        let mut mgr = manager.lock().await;
                        if let Err(e) = mgr.close_task(&task_id, new_status, None) {
                            log::error!("Failed to update task status for {}: {}", task_id.0, e);
                        }
                    }

                    // Remove execution state (for crash recovery)
                    {
                        let exec = supervised_executor.executor().lock().await;
                        if let Err(e) = exec.remove_execution_state(&task_id).await {
                            log::warn!("Failed to remove execution state for {}: {}", task_id.0, e);
                        }
                    }

                    // Clean up worktree if one was created
                    {
                        let mut exec = supervised_executor.executor().lock().await;
                        if let Err(e) = exec.cleanup_task(&task_id).await {
                            log::warn!("Failed to cleanup task {}: {}", task_id.0, e);
                        }
                    }
                }
            }
        })
    }

    /// Spawn the checkpoint loop for crash recovery.
    ///
    /// Periodically checkpoints running tasks so they can be recovered
    /// if the daemon crashes.
    fn spawn_checkpoint_loop(&self) -> tokio::task::JoinHandle<()> {
        let supervised_executor = Arc::clone(&self.supervised_executor);

        tokio::spawn(async move {
            // Checkpoint every 30 seconds
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));

            loop {
                interval.tick().await;

                // Get all running task IDs
                let task_ids: Vec<TaskId> = {
                    let exec = supervised_executor.executor().lock().await;
                    exec.running_task_ids()
                };

                // Checkpoint each running task
                for task_id in task_ids {
                    let exec = supervised_executor.executor().lock().await;
                    if let Err(e) = exec.checkpoint_task(&task_id).await {
                        log::warn!("Failed to checkpoint task {}: {}", task_id.0, e);
                    }
                }
            }
        })
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: UnixStream,
    manager: Arc<Mutex<TaskManager>>,
    supervised_executor: Arc<SupervisedExecutor>,
    merge_queue: Arc<Mutex<MergeQueue>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let request: DaemonRequest = serde_json::from_str(&line)?;
        let response = process_request(request, &manager, &supervised_executor, &merge_queue, &shutdown_tx).await;

        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        if matches!(response, DaemonResponse::Shutdown) {
            break;
        }

        line.clear();
    }

    Ok(())
}

/// Process a daemon request.
async fn process_request(
    request: DaemonRequest,
    manager: &Arc<Mutex<TaskManager>>,
    supervised_executor: &Arc<SupervisedExecutor>,
    merge_queue: &Arc<Mutex<MergeQueue>>,
    shutdown_tx: &tokio::sync::broadcast::Sender<()>,
) -> DaemonResponse {
    match request {
        DaemonRequest::Ping => DaemonResponse::Pong,

        DaemonRequest::CreateTask {
            description,
            priority,
            tags,
            context,
            ..
        } => {
            let tags_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            let mut mgr = manager.lock().await;
            match mgr.create_task(&description, priority, &tags_refs, context.as_deref()) {
                Ok(task) => DaemonResponse::Task(Some(task)),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::GetTask { id } => {
            let mgr = manager.lock().await;
            let task_id = TaskId::from_engram_id(&id);
            match mgr.get_task(&task_id) {
                Ok(task) => DaemonResponse::Task(task),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::ListTasks { status } => {
            let mgr = manager.lock().await;
            let engram_status = status.and_then(|s| parse_engram_status(&s));
            match mgr.list_tasks(engram_status) {
                Ok(tasks) => DaemonResponse::Tasks(tasks),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::ReadyTasks => {
            let mgr = manager.lock().await;
            match mgr.ready_tasks() {
                Ok(tasks) => DaemonResponse::Tasks(tasks),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::BlockedTasks => {
            let mgr = manager.lock().await;
            match mgr.blocked_tasks() {
                Ok(tasks) => DaemonResponse::Tasks(tasks),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::SetStatus { id, status } => {
            let Some(task_status) = parse_task_status(&status) else {
                return DaemonResponse::Error {
                    message: format!("Invalid status: {}", status),
                };
            };

            let mut mgr = manager.lock().await;
            let task_id = TaskId::from_engram_id(&id);
            match mgr.set_status(&task_id, task_status) {
                Ok(task) => DaemonResponse::Task(Some(task)),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::CloseTask { id, status, reason } => {
            let Some(task_status) = parse_task_status(&status) else {
                return DaemonResponse::Error {
                    message: format!("Invalid status: {}", status),
                };
            };

            let mut mgr = manager.lock().await;
            let task_id = TaskId::from_engram_id(&id);
            match mgr.close_task(&task_id, task_status, reason.as_deref()) {
                Ok(task) => DaemonResponse::Task(Some(task)),
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::AddDependency { blocked_id, blocker_id } => {
            let mut mgr = manager.lock().await;
            let blocked = TaskId::from_engram_id(&blocked_id);
            let blocker = TaskId::from_engram_id(&blocker_id);
            match mgr.add_dependency(&blocked, &blocker) {
                Ok(()) => DaemonResponse::Ok { message: None },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::RemoveDependency { blocked_id, blocker_id } => {
            let mut mgr = manager.lock().await;
            let blocked = TaskId::from_engram_id(&blocked_id);
            let blocker = TaskId::from_engram_id(&blocker_id);
            match mgr.remove_dependency(&blocked, &blocker) {
                Ok(()) => DaemonResponse::Ok { message: None },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::TaskCounts => {
            let mgr = manager.lock().await;
            match mgr.task_counts() {
                Ok(counts) => DaemonResponse::Counts {
                    queued: counts.queued,
                    running: counts.running,
                    waiting: counts.waiting,
                    blocked: counts.blocked,
                    paused: counts.paused,
                    completed: counts.completed,
                    failed: counts.failed,
                    cancelled: counts.cancelled,
                },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::StartTask { id, working_dir } => {
            let task_id = TaskId::from_engram_id(&id);

            // Get the task from the manager
            let task = {
                let mgr = manager.lock().await;
                match mgr.get_task(&task_id) {
                    Ok(Some(task)) => task,
                    Ok(None) => {
                        return DaemonResponse::Error {
                            message: format!("Task not found: {}", id),
                        };
                    }
                    Err(e) => {
                        return DaemonResponse::Error { message: e.to_string() };
                    }
                }
            };

            // Parse working directory
            let working_dir = working_dir.map(std::path::PathBuf::from);
            let working_dir_path = working_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            // Start execution through supervised executor
            match supervised_executor.start_task(task, working_dir).await {
                Ok(()) => {
                    // Update task status to Running
                    let mut mgr = manager.lock().await;
                    let _ = mgr.set_status(&task_id, TaskStatus::Running);

                    // Save execution state for crash recovery
                    {
                        let exec = supervised_executor.executor().lock().await;
                        if let Err(e) = exec
                            .save_execution_state(&task_id, &working_dir_path, None, "user_request")
                            .await
                        {
                            log::warn!("Failed to save execution state for {}: {}", id, e);
                        }
                    }

                    DaemonResponse::TaskStarted { task_id: id }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::ProvideInput { id, input } => {
            let task_id = TaskId::from_engram_id(&id);
            let mut exec = supervised_executor.executor().lock().await;
            match exec.provide_input(&task_id, input).await {
                Ok(()) => DaemonResponse::Ok {
                    message: Some("Input provided".to_string()),
                },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::AttachTask { id } => {
            let task_id = TaskId::from_engram_id(&id);
            let mut exec = supervised_executor.executor().lock().await;

            // Get current events (poll_events now updates state and buffers events)
            match exec.poll_events(&task_id) {
                Ok(events) => {
                    // Update heartbeat from events for supervision
                    supervised_executor.update_heartbeat(&task_id, &events).await;

                    // Get the current state (always fresh since poll_events updates it)
                    let state = exec.get_state(&task_id);

                    if events.is_empty() {
                        // No new events - return current status
                        if let Some(state) = state {
                            DaemonResponse::ExecutionStatusResponse {
                                task_id: id,
                                status: (&state.status).into(),
                            }
                        } else {
                            DaemonResponse::ExecutionStatusResponse {
                                task_id: id,
                                status: ExecutionStatusDto::NotRunning,
                            }
                        }
                    } else {
                        // Process events for learnings
                        supervised_executor
                            .process_events_for_learnings(&task_id, &events)
                            .await;

                        // Return ALL events since last poll (not just last one)
                        DaemonResponse::ExecutionEvents {
                            task_id: id,
                            events: events.into_iter().map(|e| e.into()).collect(),
                        }
                    }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::GetExecutionStatus { id } => {
            let task_id = TaskId::from_engram_id(&id);
            let exec = supervised_executor.executor().lock().await;

            if let Some(state) = exec.get_state(&task_id) {
                DaemonResponse::ExecutionStatusResponse {
                    task_id: id,
                    status: (&state.status).into(),
                }
            } else {
                DaemonResponse::ExecutionStatusResponse {
                    task_id: id,
                    status: ExecutionStatusDto::NotRunning,
                }
            }
        }

        DaemonRequest::CancelTask { id } => {
            let task_id = TaskId::from_engram_id(&id);
            let mut exec = supervised_executor.executor().lock().await;
            match exec.cancel_task(&task_id) {
                Ok(()) => {
                    // Clean up supervision state
                    drop(exec);
                    supervised_executor.cleanup_task(&task_id).await;

                    // Update task status to Cancelled
                    let mut mgr = manager.lock().await;
                    let _ = mgr.set_status(&task_id, TaskStatus::Cancelled);
                    DaemonResponse::Ok {
                        message: Some("Task cancelled".to_string()),
                    }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::CreateTaskWithWorktree {
            description,
            priority,
            tags,
            context,
            repo_path,
        } => {
            // Create the task first
            let tags_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            let mut mgr = manager.lock().await;
            let task = match mgr.create_task(&description, priority, &tags_refs, context.as_deref()) {
                Ok(task) => task,
                Err(e) => {
                    return DaemonResponse::Error { message: e.to_string() };
                }
            };
            drop(mgr);

            // Start the task with worktree setup
            let repo = PathBuf::from(&repo_path);
            {
                let mut exec = supervised_executor.executor().lock().await;
                match exec.start_task_with_worktree(task.clone(), Some(repo.clone())).await {
                    Ok(()) => {
                        // Update task status to Running
                        let mut mgr = manager.lock().await;
                        let _ = mgr.set_status(&task.id, TaskStatus::Running);

                        // Get worktree path if one was created
                        let worktree_path = exec.get_running_worktree_path(&task.id).cloned();

                        // Save execution state for crash recovery
                        let working_dir = worktree_path.as_ref().unwrap_or(&repo);
                        if let Err(e) = exec
                            .save_execution_state(&task.id, working_dir, worktree_path.as_deref(), "user_request")
                            .await
                        {
                            log::warn!("Failed to save execution state for {}: {}", task.id.0, e);
                        }

                        DaemonResponse::Task(Some(task))
                    }
                    Err(e) => DaemonResponse::Error { message: e.to_string() },
                }
            }
        }

        DaemonRequest::GetWorktreeInfo { id } => {
            let task_id = TaskId::from_engram_id(&id);
            let exec = supervised_executor.executor().lock().await;
            match exec.get_worktree(&task_id) {
                Some(info) => DaemonResponse::WorktreeInfo(Some(info.into())),
                None => DaemonResponse::WorktreeInfo(None),
            }
        }

        DaemonRequest::ListWorktrees => {
            let exec = supervised_executor.executor().lock().await;
            let worktrees: Vec<WorktreeInfoDto> = exec.git().list_worktrees().iter().map(|w| (*w).into()).collect();
            DaemonResponse::Worktrees(worktrees)
        }

        DaemonRequest::MergeQueueStatus => {
            let queue = merge_queue.lock().await;
            DaemonResponse::MergeQueueStatusResponse {
                pending: queue.len(),
                active: queue.active().map(|r| r.task_id.0.clone()),
            }
        }

        DaemonRequest::EnqueueMerge { id, target_branch } => {
            let task_id = TaskId::from_engram_id(&id);
            let exec = supervised_executor.executor().lock().await;

            // Get worktree info to determine branch and repo
            let Some(worktree) = exec.get_worktree(&task_id) else {
                return DaemonResponse::Error {
                    message: format!("No worktree found for task {}", id),
                };
            };

            let request = MergeRequest {
                task_id: task_id.clone(),
                branch: worktree.branch.clone(),
                target: target_branch.unwrap_or_else(|| "main".to_string()),
                repo_path: worktree.repo_path.clone(),
                completed_at: chrono::Utc::now(),
            };
            drop(exec);

            let mut queue = merge_queue.lock().await;
            queue.enqueue(request);

            DaemonResponse::MergeEnqueued { task_id: id }
        }

        DaemonRequest::GetRecoveryReport => {
            // List all persisted execution states (tasks that were running when daemon crashed)
            let exec = supervised_executor.executor().lock().await;
            match exec.state_store().list_all().await {
                Ok(states) => {
                    let tasks: Vec<RecoverableTaskDto> = states
                        .into_iter()
                        .map(|s| RecoverableTaskDto {
                            task_id: s.task_id,
                            iteration: s.iteration,
                            tokens_used: s.tokens_used,
                            cost: s.cost,
                            phase: s.phase,
                            working_dir: s.working_dir.to_string_lossy().to_string(),
                            checkpoint_at: s.checkpoint_at.to_rfc3339(),
                            started_reason: s.started_reason,
                        })
                        .collect();
                    DaemonResponse::RecoveryReport { tasks }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::ResumeTask { id } => {
            let task_id = TaskId::from_engram_id(&id);

            // Check if there's persisted state for this task
            let exec = supervised_executor.executor().lock().await;
            let state = match exec.state_store().load(&id).await {
                Ok(Some(state)) => state,
                Ok(None) => {
                    return DaemonResponse::Error {
                        message: format!("No checkpoint found for task: {}", id),
                    };
                }
                Err(e) => {
                    return DaemonResponse::Error { message: e.to_string() };
                }
            };
            drop(exec);

            // Get the task from TaskManager
            let task = {
                let mgr = manager.lock().await;
                match mgr.get_task(&task_id) {
                    Ok(Some(task)) => task,
                    Ok(None) => {
                        return DaemonResponse::Error {
                            message: format!("Task not found in TaskManager: {}", id),
                        };
                    }
                    Err(e) => {
                        return DaemonResponse::Error { message: e.to_string() };
                    }
                }
            };

            // Resume the task from its working directory
            let working_dir = Some(state.working_dir);

            // Start execution through supervised executor
            match supervised_executor.start_task(task, working_dir).await {
                Ok(()) => {
                    // Update task status to Running
                    let mut mgr = manager.lock().await;
                    let _ = mgr.set_status(&task_id, TaskStatus::Running);
                    DaemonResponse::TaskResumed { task_id: id }
                }
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        DaemonRequest::Shutdown => {
            let _ = shutdown_tx.send(());
            DaemonResponse::Shutdown
        }
    }
}

/// Parse a task status from string.
fn parse_task_status(s: &str) -> Option<TaskStatus> {
    match s.to_lowercase().as_str() {
        "queued" => Some(TaskStatus::Queued),
        "running" => Some(TaskStatus::Running),
        "waiting" | "waitingforuser" => Some(TaskStatus::WaitingForUser),
        "blocked" => Some(TaskStatus::Blocked),
        "paused" => Some(TaskStatus::Paused),
        "completed" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        "cancelled" => Some(TaskStatus::Cancelled),
        _ => None,
    }
}

/// Parse an engram status from string.
fn parse_engram_status(s: &str) -> Option<engram::Status> {
    match s.to_lowercase().as_str() {
        "open" => Some(engram::Status::Open),
        "inprogress" | "in_progress" => Some(engram::Status::InProgress),
        "blocked" => Some(engram::Status::Blocked),
        "closed" => Some(engram::Status::Closed),
        _ => None,
    }
}

/// Check if the daemon is running.
pub fn is_daemon_running(config: &DaemonConfig) -> bool {
    if !config.pid_path.exists() {
        return false;
    }

    // Read PID and check if process exists
    if let Ok(pid_str) = std::fs::read_to_string(&config.pid_path)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        // Check if process exists (kill with signal 0)
        unsafe {
            return libc::kill(pid, 0) == 0;
        }
    }

    false
}

/// Client for connecting to the daemon.
pub struct DaemonClient {
    stream: UnixStream,
}

impl DaemonClient {
    /// Connect to the daemon.
    pub async fn connect(config: &DaemonConfig) -> Result<Self> {
        let stream = UnixStream::connect(&config.socket_path).await.map_err(|e| {
            Error::Daemon(format!(
                "Failed to connect to daemon at {:?}: {}",
                config.socket_path, e
            ))
        })?;
        Ok(Self { stream })
    }

    /// Send a request and receive a response.
    pub async fn request(&mut self, request: DaemonRequest) -> Result<DaemonResponse> {
        let request_json = serde_json::to_string(&request)?;

        let (reader, mut writer) = self.stream.split();

        writer.write_all(request_json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let response: DaemonResponse = serde_json::from_str(&line)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert!(config.socket_path.to_string_lossy().contains("neuraphage.sock"));
        assert!(config.pid_path.to_string_lossy().contains("neuraphage.pid"));
    }

    #[test]
    fn test_daemon_config_from_path() {
        let path = Path::new("/tmp/test");
        let config = DaemonConfig::from_path(path);
        assert_eq!(config.socket_path, path.join("neuraphage.sock"));
        assert_eq!(config.pid_path, path.join("neuraphage.pid"));
        assert_eq!(config.data_path, path);
    }

    #[test]
    fn test_parse_task_status() {
        assert_eq!(parse_task_status("queued"), Some(TaskStatus::Queued));
        assert_eq!(parse_task_status("RUNNING"), Some(TaskStatus::Running));
        assert_eq!(parse_task_status("Completed"), Some(TaskStatus::Completed));
        assert_eq!(parse_task_status("invalid"), None);
    }

    #[test]
    fn test_parse_engram_status() {
        assert_eq!(parse_engram_status("open"), Some(engram::Status::Open));
        assert_eq!(parse_engram_status("closed"), Some(engram::Status::Closed));
        assert_eq!(parse_engram_status("invalid"), None);
    }

    #[test]
    fn test_request_serialization() {
        let request = DaemonRequest::CreateTask {
            description: "Test task".to_string(),
            priority: 2,
            tags: vec!["test".to_string()],
            context: None,
            working_dir: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();

        if let DaemonRequest::CreateTask {
            description, priority, ..
        } = parsed
        {
            assert_eq!(description, "Test task");
            assert_eq!(priority, 2);
        } else {
            panic!("Wrong request type");
        }
    }

    #[test]
    fn test_response_serialization() {
        let response = DaemonResponse::Counts {
            queued: 1,
            running: 2,
            waiting: 0,
            blocked: 1,
            paused: 0,
            completed: 5,
            failed: 1,
            cancelled: 0,
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: DaemonResponse = serde_json::from_str(&json).unwrap();

        if let DaemonResponse::Counts { queued, running, .. } = parsed {
            assert_eq!(queued, 1);
            assert_eq!(running, 2);
        } else {
            panic!("Wrong response type");
        }
    }
}
