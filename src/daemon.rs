//! Neuraphage daemon for concurrent task execution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::error::{Error, Result};
use crate::task::{Task, TaskId, TaskStatus};
use crate::task_manager::TaskManager;

/// Daemon configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Path to the Unix socket.
    pub socket_path: PathBuf,
    /// Path to the PID file.
    pub pid_path: PathBuf,
    /// Path to the data directory (for engram store).
    pub data_path: PathBuf,
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
    /// Shutdown acknowledgment.
    Shutdown,
}

/// The neuraphage daemon.
pub struct Daemon {
    config: DaemonConfig,
    manager: Arc<Mutex<TaskManager>>,
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

        let (shutdown, _) = tokio::sync::broadcast::channel(1);

        Ok(Self {
            config,
            manager: Arc::new(Mutex::new(manager)),
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
                            let shutdown_tx = self.shutdown.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, manager, shutdown_tx).await {
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
}

/// Handle a single client connection.
async fn handle_connection(
    stream: UnixStream,
    manager: Arc<Mutex<TaskManager>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let request: DaemonRequest = serde_json::from_str(&line)?;
        let response = process_request(request, &manager, &shutdown_tx).await;

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
    shutdown_tx: &tokio::sync::broadcast::Sender<()>,
) -> DaemonResponse {
    match request {
        DaemonRequest::Ping => DaemonResponse::Pong,

        DaemonRequest::CreateTask {
            description,
            priority,
            tags,
            context,
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
