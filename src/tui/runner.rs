//! TUI runner for parallel workstream monitoring.
//!
//! Connects the ParallelTuiApp to the daemon via polling and handles
//! the event loop for input, rendering, and state updates.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::Config;
use crate::daemon::{DaemonClient, DaemonConfig, DaemonRequest, DaemonResponse, ExecutionEventDto, ExecutionStatusDto};
use crate::error::{Error, Result};
use crate::task::{Task, TaskStatus};
use crate::tui::multi::{
    BudgetStatus, ConfirmDialog, ConnectionStatus, LayoutMode, OutputLine, ParallelTuiApp, Workstream,
};

/// Frame interval for 30 FPS rendering.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Interval for polling daemon events.
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Interval for refreshing full task list.
const TASK_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum reconnection backoff.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(30);

/// Minimum terminal width.
const MIN_TERMINAL_WIDTH: u16 = 40;

/// Minimum terminal height.
const MIN_TERMINAL_HEIGHT: u16 = 10;

/// TUI runner that manages the event loop and daemon connection.
pub struct TuiRunner {
    /// Daemon client for communication.
    client: Option<DaemonClient>,
    /// Daemon configuration for reconnection.
    daemon_config: DaemonConfig,
    /// TUI application state.
    app: ParallelTuiApp,
    /// Terminal for rendering.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Last time we polled for events.
    last_event_poll: Instant,
    /// Last time we refreshed the task list.
    last_task_refresh: Instant,
    /// Last time we attempted to reconnect.
    last_reconnect_attempt: Option<Instant>,
    /// Whether the runner should exit.
    should_exit: bool,
    /// Reconnection backoff (exponential).
    reconnect_backoff: Duration,
    /// Number of reconnect attempts.
    reconnect_attempts: u8,
    /// Signal flag for clean shutdown (SIGHUP/SIGTERM).
    shutdown_signal: Arc<AtomicBool>,
}

impl TuiRunner {
    /// Create a new TUI runner connected to the daemon.
    pub async fn new(client: DaemonClient, config: &Config) -> Result<Self> {
        // Initialize TUI app from config
        let app = ParallelTuiApp::from_settings(&config.tui);

        // Get daemon config for reconnection (use default paths)
        let daemon_config = DaemonConfig::default();

        // Set up terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;

        // Set up signal handler for clean shutdown
        let shutdown_signal = Arc::new(AtomicBool::new(false));
        let signal_clone = shutdown_signal.clone();

        // Handle SIGHUP (SSH disconnect) and SIGTERM
        #[cfg(unix)]
        {
            let _ = ctrlc::set_handler(move || {
                signal_clone.store(true, Ordering::SeqCst);
            });
        }

        // Suppress unused warning for non-unix platforms
        let _ = &config.daemon;

        Ok(Self {
            client: Some(client),
            daemon_config,
            app,
            terminal,
            last_event_poll: Instant::now(),
            last_task_refresh: Instant::now(),
            last_reconnect_attempt: None,
            should_exit: false,
            reconnect_backoff: Duration::from_secs(1),
            reconnect_attempts: 0,
            shutdown_signal,
        })
    }

    /// Apply layout mode override.
    pub fn set_layout_mode(&mut self, mode: LayoutMode) {
        self.app.state.layout_mode = mode;
    }

    /// Apply task ID filter.
    pub fn set_task_filter(&mut self, task_ids: Vec<String>) {
        self.app.state.filter.task_ids = task_ids;
    }

    /// Run the main event loop.
    pub async fn run(&mut self) -> Result<()> {
        // Fetch initial data
        self.fetch_initial_data().await?;

        // Main event loop
        let mut last_frame = Instant::now();
        loop {
            // Check for shutdown signal (SIGHUP/SIGTERM)
            if self.shutdown_signal.load(Ordering::SeqCst) {
                self.should_exit = true;
            }

            // Handle terminal input (non-blocking)
            if event::poll(Duration::from_millis(1))? {
                let evt = event::read()?;
                match evt {
                    Event::Key(key) => {
                        // Handle Ctrl+C globally
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            self.initiate_quit();
                        } else {
                            // Delegate to app's event handler
                            self.app.handle_event(Event::Key(key));
                        }
                    }
                    Event::Resize(width, height) => {
                        // Check minimum terminal size
                        if width < MIN_TERMINAL_WIDTH || height < MIN_TERMINAL_HEIGHT {
                            // Terminal too small - will show warning in render
                        }
                    }
                    _ => {}
                }
            }

            // Check if app wants to quit
            if self.app.should_quit || self.should_exit {
                break;
            }

            // If disconnected, attempt reconnection
            if self.client.is_none() {
                self.try_reconnect().await;
            }

            // Only poll if connected
            if self.client.is_some() {
                // Poll daemon for events periodically
                if self.last_event_poll.elapsed() >= EVENT_POLL_INTERVAL {
                    self.poll_daemon_events().await;
                    self.last_event_poll = Instant::now();
                }

                // Refresh full task list periodically
                if self.last_task_refresh.elapsed() >= TASK_REFRESH_INTERVAL {
                    self.refresh_tasks().await;
                    self.last_task_refresh = Instant::now();
                }
            }

            // Render at frame interval
            if last_frame.elapsed() >= FRAME_INTERVAL {
                self.app.tick();
                self.render()?;
                last_frame = Instant::now();
            }

            // Small sleep to prevent busy loop
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        Ok(())
    }

    /// Attempt to reconnect to the daemon with exponential backoff.
    async fn try_reconnect(&mut self) {
        // Check if enough time has passed since last attempt
        if let Some(last_attempt) = self.last_reconnect_attempt
            && last_attempt.elapsed() < self.reconnect_backoff
        {
            return;
        }

        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        self.last_reconnect_attempt = Some(Instant::now());
        self.app.state.connection_status = ConnectionStatus::Reconnecting(self.reconnect_attempts);

        // Attempt to connect
        match DaemonClient::connect(&self.daemon_config).await {
            Ok(client) => {
                self.client = Some(client);
                self.app.state.connection_status = ConnectionStatus::Connected;
                self.reconnect_backoff = Duration::from_secs(1); // Reset backoff
                self.reconnect_attempts = 0;

                // Refresh data after reconnect
                self.refresh_tasks().await;
                self.refresh_statuses().await;
                self.refresh_budget().await;
            }
            Err(_) => {
                self.app.state.connection_status = ConnectionStatus::Disconnected;
                // Exponential backoff, capped at MAX_RECONNECT_BACKOFF
                self.reconnect_backoff = (self.reconnect_backoff * 2).min(MAX_RECONNECT_BACKOFF);
            }
        }
    }

    /// Fetch initial data from daemon (tasks, status, budget).
    async fn fetch_initial_data(&mut self) -> Result<()> {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => {
                self.app.state.connection_status = ConnectionStatus::Disconnected;
                return Err(Error::Daemon("Not connected to daemon".into()));
            }
        };

        // Ping to verify connection
        match client.request(DaemonRequest::Ping).await {
            Ok(DaemonResponse::Pong) => {
                self.app.state.connection_status = ConnectionStatus::Connected;
            }
            Ok(_) => {
                return Err(Error::Daemon("Unexpected response to Ping".into()));
            }
            Err(e) => {
                self.app.state.connection_status = ConnectionStatus::Disconnected;
                return Err(e);
            }
        }

        // Fetch task list
        self.refresh_tasks().await;

        // Fetch all execution statuses
        self.refresh_statuses().await;

        // Fetch budget status
        self.refresh_budget().await;

        Ok(())
    }

    /// Refresh the task list from daemon.
    async fn refresh_tasks(&mut self) {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => return,
        };

        let request = DaemonRequest::ListTasks { status: None };
        match client.request(request).await {
            Ok(DaemonResponse::Tasks(tasks)) => {
                self.app.state.connection_status = ConnectionStatus::Connected;
                self.reconnect_backoff = Duration::from_secs(1);
                self.reconnect_attempts = 0;
                self.sync_workstreams(&tasks);
            }
            Ok(DaemonResponse::Error { .. }) => {
                // Log errors but don't disconnect
            }
            Err(_) => {
                self.handle_disconnect();
            }
            _ => {}
        }
    }

    /// Refresh execution statuses for all tasks.
    async fn refresh_statuses(&mut self) {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => return,
        };

        let request = DaemonRequest::GetAllExecutionStatus;
        match client.request(request).await {
            Ok(DaemonResponse::AllExecutionStatus { statuses }) => {
                for (task_id, status) in statuses {
                    self.apply_status_to_workstream(&task_id, &status);
                }
            }
            Ok(DaemonResponse::Error { .. }) => {
                // Log errors but don't disconnect
            }
            Err(_) => {
                self.handle_disconnect();
            }
            _ => {}
        }
    }

    /// Refresh budget status.
    async fn refresh_budget(&mut self) {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => return,
        };

        let request = DaemonRequest::GetBudgetStatus;
        match client.request(request).await {
            Ok(DaemonResponse::BudgetStatusResponse {
                daily_used,
                daily_limit,
                monthly_used,
                monthly_limit,
            }) => {
                // Map from daemon response to TUI budget status
                let limit = daily_limit.or(monthly_limit).unwrap_or(100.0);
                let current = if daily_limit.is_some() { daily_used } else { monthly_used };
                self.app.state.budget = BudgetStatus::new(current, limit);
            }
            Ok(DaemonResponse::Error { .. }) => {
                // Log errors but don't disconnect
            }
            Err(_) => {
                // Don't disconnect just for budget errors
            }
            _ => {}
        }
    }

    /// Poll daemon for new events.
    async fn poll_daemon_events(&mut self) {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => return,
        };

        let request = DaemonRequest::SubscribeAllTasks;
        match client.request(request).await {
            Ok(DaemonResponse::AllExecutionEvents { events }) => {
                self.app.state.connection_status = ConnectionStatus::Connected;
                for (task_id, event) in events {
                    self.handle_execution_event(&task_id, &event);
                }
            }
            Ok(DaemonResponse::Error { .. }) => {
                // Log errors but don't disconnect
            }
            Err(_) => {
                self.handle_disconnect();
            }
            _ => {}
        }
    }

    /// Sync workstreams from task list.
    fn sync_workstreams(&mut self, tasks: &[Task]) {
        // Get filter task IDs
        let filter_ids = &self.app.state.filter.task_ids;

        // Build set of current task IDs
        let task_ids: std::collections::HashSet<_> = tasks.iter().map(|t| t.id.as_ref()).collect();

        // Remove workstreams for tasks that no longer exist
        self.app
            .state
            .workstreams
            .retain(|ws| task_ids.contains(ws.task_id.as_ref()));

        // Add or update workstreams
        for task in tasks {
            // Apply filter if set
            let task_id_str: &str = task.id.as_ref();
            if !filter_ids.is_empty() && !filter_ids.iter().any(|f| f == task_id_str) {
                continue;
            }

            if let Some(ws) = self
                .app
                .state
                .workstreams
                .iter_mut()
                .find(|ws| ws.task_id.as_ref() == task_id_str)
            {
                // Update existing workstream
                ws.status = task.status;
            } else {
                // Add new workstream
                let ws = Workstream::new(task.id.clone(), &task.description, task.status);
                self.app.state.workstreams.push(ws);
            }
        }

        // Update stats
        self.app.state.stats.running_count = self
            .app
            .state
            .workstreams
            .iter()
            .filter(|ws| ws.status == TaskStatus::Running)
            .count();
        self.app.state.stats.waiting_count = self
            .app
            .state
            .workstreams
            .iter()
            .filter(|ws| ws.status == TaskStatus::WaitingForUser)
            .count();
        self.app.state.stats.queued_count = self
            .app
            .state
            .workstreams
            .iter()
            .filter(|ws| ws.status == TaskStatus::Queued)
            .count();
    }

    /// Apply execution status to a workstream.
    fn apply_status_to_workstream(&mut self, task_id: &str, status: &ExecutionStatusDto) {
        if let Some(ws) = self
            .app
            .state
            .workstreams
            .iter_mut()
            .find(|ws| ws.task_id.as_ref() == task_id)
        {
            apply_execution_status(ws, status);
        }
    }

    /// Handle an execution event for a task.
    fn handle_execution_event(&mut self, task_id: &str, event: &ExecutionEventDto) {
        if let Some(ws) = self
            .app
            .state
            .workstreams
            .iter_mut()
            .find(|ws| ws.task_id.as_ref() == task_id)
        {
            apply_event(ws, event);
        }
    }

    /// Handle disconnect from daemon.
    fn handle_disconnect(&mut self) {
        self.client = None;
        self.app.state.connection_status = ConnectionStatus::Disconnected;
        // Reconnection will be attempted in the main loop via try_reconnect()
    }

    /// Initiate quit (may show confirmation dialog).
    fn initiate_quit(&mut self) {
        // Check if any tasks are running
        let running = self
            .app
            .state
            .workstreams
            .iter()
            .any(|ws| ws.status == TaskStatus::Running);

        if running {
            // Show confirmation dialog
            self.app.state.show_confirm(ConfirmDialog::quit_with_running());
        } else {
            self.should_exit = true;
        }
    }

    /// Render the current frame.
    fn render(&mut self) -> Result<()> {
        self.terminal.draw(|frame| {
            self.app.render(frame);
        })?;
        Ok(())
    }
}

impl Drop for TuiRunner {
    fn drop(&mut self) {
        // Restore terminal state
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Apply execution status to update workstream state.
fn apply_execution_status(ws: &mut Workstream, status: &ExecutionStatusDto) {
    match status {
        ExecutionStatusDto::Running {
            iteration,
            tokens_used,
            cost,
        } => {
            ws.iteration = *iteration;
            ws.tokens = *tokens_used;
            ws.cost = *cost;
            ws.needs_attention = false;
            ws.attention_reason = None;
            ws.last_activity = chrono::Utc::now();
        }
        ExecutionStatusDto::WaitingForUser { prompt } => {
            ws.needs_attention = true;
            ws.attention_reason = Some(prompt.clone());
        }
        ExecutionStatusDto::Completed { .. } => {
            ws.needs_attention = false;
            ws.set_progress(1.0);
        }
        ExecutionStatusDto::Failed { error } => {
            ws.needs_attention = true;
            ws.attention_reason = Some(format!("Failed: {}", error));
        }
        ExecutionStatusDto::Cancelled => {
            ws.needs_attention = false;
        }
        ExecutionStatusDto::NotRunning => {
            ws.needs_attention = false;
        }
    }
}

/// Apply an execution event to update workstream state.
fn apply_event(ws: &mut Workstream, event: &ExecutionEventDto) {
    ws.last_activity = chrono::Utc::now();
    ws.idle_duration = std::time::Duration::ZERO;

    match event {
        ExecutionEventDto::IterationComplete {
            iteration,
            tokens_used,
            cost,
        } => {
            ws.iteration = *iteration;
            ws.tokens = *tokens_used;
            ws.cost = *cost;
        }
        ExecutionEventDto::TextDelta { content } => {
            ws.add_output(OutputLine::new(content.clone()));
        }
        ExecutionEventDto::ToolCalled { name, result } => {
            ws.add_output(OutputLine::new(format!("{}: {}", name, result)));
        }
        ExecutionEventDto::ToolStarted { name } => {
            ws.add_output(OutputLine::new(format!("→ {}", name)));
        }
        ExecutionEventDto::ToolCompleted { name, result } => {
            // Truncate long results
            let result_preview = if result.len() > 100 {
                format!("{}...", &result[..100])
            } else {
                result.clone()
            };
            ws.add_output(OutputLine::new(format!("✓ {}: {}", name, result_preview)));
        }
        ExecutionEventDto::WaitingForUser { prompt } => {
            ws.needs_attention = true;
            ws.attention_reason = Some(prompt.clone());
        }
        ExecutionEventDto::Completed { reason } => {
            ws.set_progress(1.0);
            ws.add_output(OutputLine::new(format!("Completed: {}", reason)));
        }
        ExecutionEventDto::Failed { error } => {
            ws.needs_attention = true;
            ws.attention_reason = Some(format!("Failed: {}", error));
            ws.add_output(OutputLine::error(error.clone()));
        }
        ExecutionEventDto::LlmResponse { content } => {
            for line in content.lines() {
                ws.add_output(OutputLine::new(line.to_string()));
            }
        }
        ExecutionEventDto::ActivityChanged { activity } => {
            let _ = activity; // Ignore for now
        }
        ExecutionEventDto::BudgetWarning {
            budget_type,
            current,
            limit,
            ..
        } => {
            ws.add_output(OutputLine::new(format!(
                "⚠ Budget warning: {} at ${:.2}/${:.2}",
                budget_type, current, limit
            )));
        }
        ExecutionEventDto::BudgetExceeded {
            budget_type,
            current,
            limit,
        } => {
            ws.needs_attention = true;
            ws.attention_reason = Some(format!("Budget exceeded: {}", budget_type));
            ws.add_output(OutputLine::error(format!(
                "Budget exceeded: {} at ${:.2}/${:.2}",
                budget_type, current, limit
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskId;

    fn make_test_task(id: &str, description: &str, status: TaskStatus) -> Task {
        Task {
            id: TaskId::from_engram_id(id),
            description: description.to_string(),
            context: None,
            status,
            priority: 5,
            tags: vec![],
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

    #[test]
    fn test_apply_execution_status_running() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_execution_status(
            &mut ws,
            &ExecutionStatusDto::Running {
                iteration: 5,
                tokens_used: 1000,
                cost: 0.05,
            },
        );

        assert_eq!(ws.iteration, 5);
        assert_eq!(ws.tokens, 1000);
        assert!((ws.cost - 0.05).abs() < f64::EPSILON);
        assert!(!ws.needs_attention);
    }

    #[test]
    fn test_apply_execution_status_waiting() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_execution_status(
            &mut ws,
            &ExecutionStatusDto::WaitingForUser {
                prompt: "Choose an option".to_string(),
            },
        );

        assert!(ws.needs_attention);
        assert_eq!(ws.attention_reason, Some("Choose an option".to_string()));
    }

    #[test]
    fn test_apply_event_iteration_complete() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::IterationComplete {
                iteration: 10,
                tokens_used: 5000,
                cost: 0.25,
            },
        );

        assert_eq!(ws.iteration, 10);
        assert_eq!(ws.tokens, 5000);
        assert!((ws.cost - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn test_apply_event_text_delta() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::TextDelta {
                content: "Hello world".to_string(),
            },
        );

        assert_eq!(ws.output.len(), 1);
    }

    #[test]
    fn test_apply_event_failed() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::Failed {
                error: "Something went wrong".to_string(),
            },
        );

        assert!(ws.needs_attention);
        assert!(ws.attention_reason.unwrap().contains("Failed"));
        assert_eq!(ws.output.len(), 1);
        assert!(ws.output[0].is_error);
    }

    #[test]
    fn test_apply_execution_status_completed() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);
        ws.needs_attention = true; // Set to true first

        apply_execution_status(
            &mut ws,
            &ExecutionStatusDto::Completed {
                reason: "Task finished".to_string(),
            },
        );

        assert!(!ws.needs_attention);
        assert!((ws.progress - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_apply_event_tool_completed_truncation() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        // Create a long result that should be truncated
        let long_result = "x".repeat(200);
        apply_event(
            &mut ws,
            &ExecutionEventDto::ToolCompleted {
                name: "read_file".to_string(),
                result: long_result,
            },
        );

        assert_eq!(ws.output.len(), 1);
        // Result should be truncated to 100 chars + "..."
        assert!(ws.output[0].text.contains("..."));
        assert!(ws.output[0].text.len() < 150);
    }

    #[test]
    fn test_apply_event_budget_warning() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::BudgetWarning {
                budget_type: "daily".to_string(),
                current: 8.50,
                limit: 10.0,
                threshold: 80.0,
            },
        );

        assert_eq!(ws.output.len(), 1);
        assert!(ws.output[0].text.contains("Budget warning"));
        assert!(!ws.needs_attention); // Warning doesn't require attention
    }

    #[test]
    fn test_apply_event_budget_exceeded() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::BudgetExceeded {
                budget_type: "daily".to_string(),
                current: 12.50,
                limit: 10.0,
            },
        );

        assert!(ws.needs_attention);
        assert!(ws.attention_reason.as_ref().unwrap().contains("Budget exceeded"));
        assert_eq!(ws.output.len(), 1);
        assert!(ws.output[0].is_error);
    }

    #[test]
    fn test_apply_event_llm_response_multiline() {
        let task = make_test_task("task-123", "Test task", TaskStatus::Running);
        let mut ws = Workstream::new(task.id.clone(), &task.description, task.status);

        apply_event(
            &mut ws,
            &ExecutionEventDto::LlmResponse {
                content: "Line 1\nLine 2\nLine 3".to_string(),
            },
        );

        // Should create 3 separate output lines
        assert_eq!(ws.output.len(), 3);
    }
}
