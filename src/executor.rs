//! Task execution engine.
//!
//! Manages execution of tasks through the agentic loop, handling:
//! - Task spawning and monitoring
//! - User input routing
//! - Execution state persistence
//! - Concurrency limits

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agentic::{AgenticConfig, AgenticLoop, AnthropicClient, IterationResult, LlmClient};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::task::{Task, TaskId};

/// Configuration for the task executor.
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Maximum concurrent tasks.
    pub max_concurrent: usize,
    /// Base directory for task data.
    pub data_dir: PathBuf,
    /// Default model to use.
    pub default_model: String,
    /// Maximum iterations per task.
    pub max_iterations: u32,
    /// Maximum tokens per task.
    pub max_tokens: u64,
    /// Maximum cost per task in USD.
    pub max_cost: f64,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 5,
            data_dir: PathBuf::from("."),
            default_model: "claude-sonnet-4-20250514".to_string(),
            max_iterations: 100,
            max_tokens: 1_000_000,
            max_cost: 10.0,
        }
    }
}

impl From<&Config> for ExecutorConfig {
    fn from(config: &Config) -> Self {
        Self {
            max_concurrent: config.max_concurrent_tasks,
            data_dir: config.data_dir.clone(),
            default_model: config.api.default_model.clone(),
            ..Default::default()
        }
    }
}

/// Status of a task execution.
#[derive(Debug, Clone)]
pub enum ExecutionStatus {
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
}

/// State of a running task execution.
#[derive(Debug)]
pub struct ExecutionState {
    /// Task ID.
    pub task_id: TaskId,
    /// Current status.
    pub status: ExecutionStatus,
    /// When execution started.
    pub started_at: DateTime<Utc>,
    /// Last activity time.
    pub last_activity: DateTime<Utc>,
}

/// Current activity of the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    /// Calling the LLM API.
    Thinking,
    /// Streaming text response.
    Streaming,
    /// Executing a tool.
    ExecutingTool { name: String },
    /// Waiting for external response (web search, etc).
    WaitingForTool { name: String },
    /// Idle between iterations.
    Idle,
}

impl std::fmt::Display for Activity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Activity::Thinking => write!(f, "Thinking..."),
            Activity::Streaming => write!(f, "Writing..."),
            Activity::ExecutingTool { name } => write!(f, "Running {}...", name),
            Activity::WaitingForTool { name } => write!(f, "Waiting for {}...", name),
            Activity::Idle => write!(f, "Idle"),
        }
    }
}

/// Event from task execution.
#[derive(Debug, Clone)]
pub enum ExecutionEvent {
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
    /// Activity changed (for UX updates).
    ActivityChanged { activity: Activity },
    /// Tool execution started.
    ToolStarted { name: String },
    /// Tool execution completed.
    ToolCompleted { name: String, result: String },
}

/// Handle to a running task.
struct RunningTask {
    /// Join handle for the task.
    handle: JoinHandle<ExecutionResult>,
    /// Channel to send user input.
    input_tx: mpsc::Sender<String>,
    /// Channel to receive events.
    event_rx: mpsc::Receiver<ExecutionEvent>,
    /// Current state (mutable, updated from events).
    state: ExecutionState,
    /// Buffer of recent events for replay to slow pollers.
    event_buffer: std::collections::VecDeque<ExecutionEvent>,
    /// Index tracking where client last read from buffer.
    last_poll_idx: usize,
}

/// Result of task execution.
#[derive(Debug)]
pub struct ExecutionResult {
    /// Task ID.
    pub task_id: TaskId,
    /// Final status.
    pub status: ExecutionStatus,
    /// Total iterations.
    pub iterations: u32,
    /// Total tokens used.
    pub tokens_used: u64,
    /// Total cost.
    pub cost: f64,
}

/// Task executor manages running tasks through the agentic loop.
pub struct TaskExecutor {
    config: ExecutorConfig,
    llm: Arc<dyn LlmClient + Send + Sync>,
    running_tasks: HashMap<TaskId, RunningTask>,
}

impl TaskExecutor {
    /// Create a new task executor.
    pub fn new(config: ExecutorConfig) -> Result<Self> {
        let llm = Arc::new(AnthropicClient::new(None, None)?);

        Ok(Self {
            config,
            llm,
            running_tasks: HashMap::new(),
        })
    }

    /// Create executor with a custom LLM client (for testing).
    pub fn with_llm(config: ExecutorConfig, llm: Arc<dyn LlmClient + Send + Sync>) -> Self {
        Self {
            config,
            llm,
            running_tasks: HashMap::new(),
        }
    }

    /// Check if we can start more tasks.
    pub fn can_start_task(&self) -> bool {
        self.running_tasks.len() < self.config.max_concurrent
    }

    /// Get number of running tasks.
    pub fn running_count(&self) -> usize {
        self.running_tasks.len()
    }

    /// Start executing a task.
    pub fn start_task(&mut self, task: Task) -> Result<()> {
        if !self.can_start_task() {
            return Err(Error::Validation(format!(
                "Cannot start task: at capacity ({} running)",
                self.config.max_concurrent
            )));
        }

        let task_id = task.id.clone();

        // Check if already running
        if self.running_tasks.contains_key(&task_id) {
            return Err(Error::Validation(format!("Task {} is already running", task_id.0)));
        }

        // Create channels for communication
        let (input_tx, input_rx) = mpsc::channel::<String>(1);
        let (event_tx, event_rx) = mpsc::channel::<ExecutionEvent>(100);

        // Build agentic config
        let working_dir = std::env::current_dir().unwrap_or_default();
        let conversation_path = self
            .config
            .data_dir
            .join("conversations")
            .join(format!("{}.json", task_id.0));

        let agentic_config = AgenticConfig {
            max_iterations: self.config.max_iterations,
            max_tokens: self.config.max_tokens,
            max_cost: self.config.max_cost,
            model: self.config.default_model.clone(),
            working_dir,
            conversation_path,
        };

        // Spawn the execution task
        let llm = Arc::clone(&self.llm);
        let task_clone = task.clone();
        let handle =
            tokio::spawn(async move { execute_task(task_clone, agentic_config, llm, input_rx, event_tx).await });

        let state = ExecutionState {
            task_id: task_id.clone(),
            status: ExecutionStatus::Running {
                iteration: 0,
                tokens_used: 0,
                cost: 0.0,
            },
            started_at: Utc::now(),
            last_activity: Utc::now(),
        };

        self.running_tasks.insert(
            task_id,
            RunningTask {
                handle,
                input_tx,
                event_rx,
                state,
                event_buffer: std::collections::VecDeque::with_capacity(100),
                last_poll_idx: 0,
            },
        );

        Ok(())
    }

    /// Provide user input to a waiting task.
    pub async fn provide_input(&mut self, task_id: &TaskId, input: String) -> Result<()> {
        let running = self
            .running_tasks
            .get(task_id)
            .ok_or_else(|| Error::TaskNotFound { id: task_id.0.clone() })?;

        running
            .input_tx
            .send(input)
            .await
            .map_err(|_| Error::Daemon("Task is not accepting input".to_string()))?;

        Ok(())
    }

    /// Get execution state for a task.
    pub fn get_state(&self, task_id: &TaskId) -> Option<&ExecutionState> {
        self.running_tasks.get(task_id).map(|r| &r.state)
    }

    /// Poll for events from a task.
    ///
    /// This drains new events from the channel, updates state from them,
    /// buffers them for replay, and returns events since last poll.
    pub fn poll_events(&mut self, task_id: &TaskId) -> Result<Vec<ExecutionEvent>> {
        let running = self
            .running_tasks
            .get_mut(task_id)
            .ok_or_else(|| Error::TaskNotFound { id: task_id.0.clone() })?;

        // Drain new events from channel and process them
        while let Ok(event) = running.event_rx.try_recv() {
            // Update state from events
            running.state.last_activity = Utc::now();
            match &event {
                ExecutionEvent::IterationComplete {
                    iteration,
                    tokens_used,
                    cost,
                } => {
                    running.state.status = ExecutionStatus::Running {
                        iteration: *iteration,
                        tokens_used: *tokens_used,
                        cost: *cost,
                    };
                }
                ExecutionEvent::WaitingForUser { prompt } => {
                    running.state.status = ExecutionStatus::WaitingForUser { prompt: prompt.clone() };
                }
                ExecutionEvent::Completed { reason } => {
                    running.state.status = ExecutionStatus::Completed { reason: reason.clone() };
                }
                ExecutionEvent::Failed { error } => {
                    running.state.status = ExecutionStatus::Failed { error: error.clone() };
                }
                _ => {}
            }

            // Buffer the event for replay
            running.event_buffer.push_back(event);

            // Keep buffer bounded (max 100 events)
            if running.event_buffer.len() > 100 {
                running.event_buffer.pop_front();
                // Adjust last_poll_idx if events were dropped
                if running.last_poll_idx > 0 {
                    running.last_poll_idx -= 1;
                }
            }
        }

        // Return events since last poll
        let events: Vec<ExecutionEvent> = running
            .event_buffer
            .iter()
            .skip(running.last_poll_idx)
            .cloned()
            .collect();

        // Update last poll index
        running.last_poll_idx = running.event_buffer.len();

        Ok(events)
    }

    /// Check all running tasks and collect completed ones.
    ///
    /// This also drains events from all tasks to keep state updated.
    pub async fn poll_completed(&mut self) -> Vec<ExecutionResult> {
        let mut completed = Vec::new();
        let mut to_remove = Vec::new();

        for (task_id, running) in &mut self.running_tasks {
            // Check if task finished
            if running.handle.is_finished() {
                to_remove.push(task_id.clone());
            }

            // Always drain events to update state (even for finished tasks)
            while let Ok(event) = running.event_rx.try_recv() {
                running.state.last_activity = Utc::now();
                match &event {
                    ExecutionEvent::IterationComplete {
                        iteration,
                        tokens_used,
                        cost,
                    } => {
                        running.state.status = ExecutionStatus::Running {
                            iteration: *iteration,
                            tokens_used: *tokens_used,
                            cost: *cost,
                        };
                    }
                    ExecutionEvent::WaitingForUser { prompt } => {
                        running.state.status = ExecutionStatus::WaitingForUser { prompt: prompt.clone() };
                    }
                    ExecutionEvent::Completed { reason } => {
                        running.state.status = ExecutionStatus::Completed { reason: reason.clone() };
                    }
                    ExecutionEvent::Failed { error } => {
                        running.state.status = ExecutionStatus::Failed { error: error.clone() };
                    }
                    _ => {}
                }

                // Buffer the event
                running.event_buffer.push_back(event);
                if running.event_buffer.len() > 100 {
                    running.event_buffer.pop_front();
                    if running.last_poll_idx > 0 {
                        running.last_poll_idx -= 1;
                    }
                }
            }
        }

        // Collect results from finished tasks
        for task_id in to_remove {
            if let Some(running) = self.running_tasks.remove(&task_id) {
                match running.handle.await {
                    Ok(result) => completed.push(result),
                    Err(e) => {
                        completed.push(ExecutionResult {
                            task_id,
                            status: ExecutionStatus::Failed {
                                error: format!("Task panicked: {}", e),
                            },
                            iterations: 0,
                            tokens_used: 0,
                            cost: 0.0,
                        });
                    }
                }
            }
        }

        completed
    }

    /// Cancel a running task.
    pub fn cancel_task(&mut self, task_id: &TaskId) -> Result<()> {
        let running = self
            .running_tasks
            .remove(task_id)
            .ok_or_else(|| Error::TaskNotFound { id: task_id.0.clone() })?;

        running.handle.abort();
        Ok(())
    }

    /// Get IDs of all running tasks.
    pub fn running_task_ids(&self) -> Vec<TaskId> {
        self.running_tasks.keys().cloned().collect()
    }
}

/// Execute a task through the agentic loop.
async fn execute_task(
    task: Task,
    config: AgenticConfig,
    llm: Arc<dyn LlmClient + Send + Sync>,
    mut input_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<ExecutionEvent>,
) -> ExecutionResult {
    // Create or load the agentic loop (passing event_tx for streaming)
    let loop_result = if config.conversation_path.exists() {
        AgenticLoop::load(config.clone(), LlmClientWrapper(llm), Some(event_tx.clone()))
    } else {
        AgenticLoop::new(config.clone(), LlmClientWrapper(llm), Some(event_tx.clone()))
    };

    let mut agentic_loop = match loop_result {
        Ok(l) => l,
        Err(e) => {
            let _ = event_tx.send(ExecutionEvent::Failed { error: e.to_string() }).await;
            return ExecutionResult {
                task_id: task.id,
                status: ExecutionStatus::Failed { error: e.to_string() },
                iterations: 0,
                tokens_used: 0,
                cost: 0.0,
            };
        }
    };

    // Add initial user message with task description
    if agentic_loop.iteration() == 0
        && let Err(e) = agentic_loop.add_user_message(&task.description)
    {
        let _ = event_tx.send(ExecutionEvent::Failed { error: e.to_string() }).await;
        return ExecutionResult {
            task_id: task.id,
            status: ExecutionStatus::Failed { error: e.to_string() },
            iterations: 0,
            tokens_used: 0,
            cost: 0.0,
        };
    }

    // Run the loop
    loop {
        let result = agentic_loop.iterate(&task).await;

        // Send iteration event
        let _ = event_tx
            .send(ExecutionEvent::IterationComplete {
                iteration: agentic_loop.iteration(),
                tokens_used: agentic_loop.tokens_used(),
                cost: agentic_loop.cost(),
            })
            .await;

        match result {
            Ok(IterationResult::Continue) => {
                // Save conversation state
                let _ = agentic_loop.save();
                continue;
            }
            Ok(IterationResult::Completed { reason }) => {
                let _ = agentic_loop.save();
                let _ = event_tx
                    .send(ExecutionEvent::Completed { reason: reason.clone() })
                    .await;
                return ExecutionResult {
                    task_id: task.id,
                    status: ExecutionStatus::Completed { reason },
                    iterations: agentic_loop.iteration(),
                    tokens_used: agentic_loop.tokens_used(),
                    cost: agentic_loop.cost(),
                };
            }
            Ok(IterationResult::Failed { error }) => {
                let _ = agentic_loop.save();
                let _ = event_tx.send(ExecutionEvent::Failed { error: error.clone() }).await;
                return ExecutionResult {
                    task_id: task.id,
                    status: ExecutionStatus::Failed { error },
                    iterations: agentic_loop.iteration(),
                    tokens_used: agentic_loop.tokens_used(),
                    cost: agentic_loop.cost(),
                };
            }
            Ok(IterationResult::WaitingForUser { prompt }) => {
                let _ = agentic_loop.save();
                let _ = event_tx
                    .send(ExecutionEvent::WaitingForUser { prompt: prompt.clone() })
                    .await;

                // Wait for user input
                match input_rx.recv().await {
                    Some(input) => {
                        if let Err(e) = agentic_loop.add_user_message(&input) {
                            let _ = event_tx.send(ExecutionEvent::Failed { error: e.to_string() }).await;
                            return ExecutionResult {
                                task_id: task.id,
                                status: ExecutionStatus::Failed { error: e.to_string() },
                                iterations: agentic_loop.iteration(),
                                tokens_used: agentic_loop.tokens_used(),
                                cost: agentic_loop.cost(),
                            };
                        }
                    }
                    None => {
                        // Channel closed, task cancelled
                        return ExecutionResult {
                            task_id: task.id,
                            status: ExecutionStatus::Cancelled,
                            iterations: agentic_loop.iteration(),
                            tokens_used: agentic_loop.tokens_used(),
                            cost: agentic_loop.cost(),
                        };
                    }
                }
            }
            Ok(IterationResult::LimitReached { limit }) => {
                let _ = agentic_loop.save();
                let error = format!("Limit reached: {}", limit);
                let _ = event_tx.send(ExecutionEvent::Failed { error: error.clone() }).await;
                return ExecutionResult {
                    task_id: task.id,
                    status: ExecutionStatus::Failed { error },
                    iterations: agentic_loop.iteration(),
                    tokens_used: agentic_loop.tokens_used(),
                    cost: agentic_loop.cost(),
                };
            }
            Ok(IterationResult::Blocked { reason }) => {
                let _ = agentic_loop.save();
                let error = format!("Task blocked: {}", reason);
                let _ = event_tx.send(ExecutionEvent::Failed { error: error.clone() }).await;
                return ExecutionResult {
                    task_id: task.id,
                    status: ExecutionStatus::Failed { error },
                    iterations: agentic_loop.iteration(),
                    tokens_used: agentic_loop.tokens_used(),
                    cost: agentic_loop.cost(),
                };
            }
            Err(e) => {
                let _ = agentic_loop.save();
                let _ = event_tx.send(ExecutionEvent::Failed { error: e.to_string() }).await;
                return ExecutionResult {
                    task_id: task.id,
                    status: ExecutionStatus::Failed { error: e.to_string() },
                    iterations: agentic_loop.iteration(),
                    tokens_used: agentic_loop.tokens_used(),
                    cost: agentic_loop.cost(),
                };
            }
        }
    }
}

/// Wrapper to adapt Arc<dyn LlmClient> to owned LlmClient.
struct LlmClientWrapper(Arc<dyn LlmClient + Send + Sync>);

#[async_trait::async_trait]
impl LlmClient for LlmClientWrapper {
    async fn complete(
        &self,
        model: &str,
        messages: &[crate::agentic::Message],
        tools: &[crate::agentic::Tool],
    ) -> Result<crate::agentic::LlmResponse> {
        self.0.complete(model, messages, tools).await
    }

    async fn stream(
        &self,
        model: &str,
        messages: &[crate::agentic::Message],
        tools: &[crate::agentic::Tool],
        chunk_tx: mpsc::Sender<crate::agentic::StreamChunk>,
    ) -> Result<crate::agentic::LlmResponse> {
        self.0.stream(model, messages, tools, chunk_tx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentic::llm::LlmResponse;
    use std::sync::Mutex as StdMutex;

    struct MockLlm {
        responses: StdMutex<Vec<LlmResponse>>,
    }

    impl MockLlm {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses: StdMutex::new(responses),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for MockLlm {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[crate::agentic::Message],
            _tools: &[crate::agentic::Tool],
        ) -> Result<LlmResponse> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(LlmResponse::text("No more responses"))
            } else {
                Ok(responses.remove(0))
            }
        }
    }

    #[test]
    fn test_executor_config_default() {
        let config = ExecutorConfig::default();
        assert_eq!(config.max_concurrent, 5);
        assert_eq!(config.max_iterations, 100);
    }

    #[test]
    fn test_can_start_task() {
        let config = ExecutorConfig {
            max_concurrent: 2,
            ..Default::default()
        };

        // Create mock LLM
        let llm = Arc::new(MockLlm::new(vec![]));
        let executor = TaskExecutor::with_llm(config, llm);

        assert!(executor.can_start_task());
        assert_eq!(executor.running_count(), 0);
    }

    #[tokio::test]
    async fn test_execution_status_variants() {
        // Just test that status variants work
        let status = ExecutionStatus::Running {
            iteration: 1,
            tokens_used: 100,
            cost: 0.01,
        };
        match status {
            ExecutionStatus::Running { iteration, .. } => assert_eq!(iteration, 1),
            _ => panic!("Wrong variant"),
        }

        let status = ExecutionStatus::WaitingForUser {
            prompt: "test".to_string(),
        };
        match status {
            ExecutionStatus::WaitingForUser { prompt } => assert_eq!(prompt, "test"),
            _ => panic!("Wrong variant"),
        }

        let status = ExecutionStatus::Completed {
            reason: "done".to_string(),
        };
        match status {
            ExecutionStatus::Completed { reason } => assert_eq!(reason, "done"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_execution_event_variants() {
        let event = ExecutionEvent::IterationComplete {
            iteration: 5,
            tokens_used: 500,
            cost: 0.05,
        };
        match event {
            ExecutionEvent::IterationComplete { iteration, .. } => assert_eq!(iteration, 5),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_running_task_event_buffer() {
        use std::collections::VecDeque;

        // Test that event buffer maintains bounded size
        let mut buffer: VecDeque<ExecutionEvent> = VecDeque::with_capacity(100);
        let mut last_poll_idx: usize = 0;

        // Add more than 100 events
        for i in 0..150 {
            buffer.push_back(ExecutionEvent::IterationComplete {
                iteration: i,
                tokens_used: (i * 10) as u64,
                cost: i as f64 * 0.001,
            });

            // Keep buffer bounded
            if buffer.len() > 100 {
                buffer.pop_front();
                last_poll_idx = last_poll_idx.saturating_sub(1);
            }
        }

        // Buffer should be at capacity
        assert_eq!(buffer.len(), 100);

        // Events since last poll
        let events: Vec<_> = buffer.iter().skip(last_poll_idx).cloned().collect();
        assert_eq!(events.len(), 100);

        // Update poll index
        last_poll_idx = buffer.len();

        // No events since last poll
        let events: Vec<_> = buffer.iter().skip(last_poll_idx).cloned().collect();
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_state_update_from_iteration_complete() {
        // Test that ExecutionState is updated correctly from IterationComplete events
        let mut state = ExecutionState {
            task_id: TaskId("test".to_string()),
            status: ExecutionStatus::Running {
                iteration: 0,
                tokens_used: 0,
                cost: 0.0,
            },
            started_at: Utc::now(),
            last_activity: Utc::now(),
        };

        // Simulate processing an IterationComplete event
        let event = ExecutionEvent::IterationComplete {
            iteration: 5,
            tokens_used: 1234,
            cost: 0.0567,
        };

        if let ExecutionEvent::IterationComplete {
            iteration,
            tokens_used,
            cost,
        } = &event
        {
            state.status = ExecutionStatus::Running {
                iteration: *iteration,
                tokens_used: *tokens_used,
                cost: *cost,
            };
            state.last_activity = Utc::now();
        }

        // Verify state was updated
        match state.status {
            ExecutionStatus::Running {
                iteration,
                tokens_used,
                cost,
            } => {
                assert_eq!(iteration, 5);
                assert_eq!(tokens_used, 1234);
                assert!((cost - 0.0567).abs() < 0.0001);
            }
            _ => panic!("Expected Running status"),
        }
    }

    #[tokio::test]
    async fn test_full_event_flow_with_streaming_mock() {
        use crate::agentic::llm::MockLlmClient;
        use crate::task::{Task, TaskStatus};
        use tempfile::TempDir;

        // Create mock LLM that sends streaming chunks
        let mock_llm = MockLlmClient::new(vec![LlmResponse {
            content: "Test streaming response".to_string(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".to_string()),
            tokens_used: 100,
            cost: 0.01,
        }]);

        let temp_dir = TempDir::new().unwrap();
        let config = ExecutorConfig {
            max_concurrent: 5,
            data_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };

        // Create executor with mock LLM
        let mut executor = TaskExecutor::with_llm(config, Arc::new(mock_llm));

        // Create a test task
        let now = chrono::Utc::now();
        let task = Task {
            id: TaskId("test-task".to_string()),
            description: "Test task".to_string(),
            context: None,
            status: TaskStatus::Running,
            priority: 5,
            tags: vec![],
            created_at: now,
            updated_at: now,
            closed_at: None,
            close_reason: None,
            parent_id: None,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
        };

        // Start the task
        executor.start_task(task).unwrap();

        // Give the task time to execute (mock is fast)
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Poll for events
        let task_id = TaskId("test-task".to_string());
        let events = executor.poll_events(&task_id).unwrap();

        // We should have at least some events:
        // - ActivityChanged (Thinking)
        // - TextDelta events (from mock streaming)
        // - ActivityChanged (Idle)
        // - IterationComplete
        assert!(
            !events.is_empty(),
            "Expected events but got none. Buffer should contain events from task execution."
        );

        // Verify we have TextDelta events
        let text_deltas: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ExecutionEvent::TextDelta { .. }))
            .collect();
        assert!(
            !text_deltas.is_empty(),
            "Expected TextDelta events but found none. Events: {:?}",
            events
        );

        // Verify we have IterationComplete event
        let iteration_completes: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ExecutionEvent::IterationComplete { .. }))
            .collect();
        assert!(
            !iteration_completes.is_empty(),
            "Expected IterationComplete events but found none. Events: {:?}",
            events
        );

        // Get state - should be updated from IterationComplete events
        let state = executor.get_state(&task_id);
        assert!(state.is_some(), "Task state should exist");
    }

    #[test]
    fn test_event_buffer_replay() {
        use std::collections::VecDeque;

        // Simulate slow poller scenario
        let mut buffer: VecDeque<ExecutionEvent> = VecDeque::with_capacity(100);
        let mut last_poll_idx = 0;

        // Send 10 events before first poll
        for i in 0..10 {
            buffer.push_back(ExecutionEvent::TextDelta {
                content: format!("chunk{}", i),
            });
        }

        // First poll should get all 10 events
        let events: Vec<_> = buffer.iter().skip(last_poll_idx).cloned().collect();
        assert_eq!(events.len(), 10);
        last_poll_idx = buffer.len();

        // Send 5 more events
        for i in 10..15 {
            buffer.push_back(ExecutionEvent::TextDelta {
                content: format!("chunk{}", i),
            });
        }

        // Second poll should get only the new 5 events
        let events: Vec<_> = buffer.iter().skip(last_poll_idx).cloned().collect();
        assert_eq!(events.len(), 5);
        last_poll_idx = buffer.len();

        // Third poll with no new events
        let events: Vec<_> = buffer.iter().skip(last_poll_idx).cloned().collect();
        assert_eq!(events.len(), 0);
    }
}
