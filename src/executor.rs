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
use tokio::sync::{Mutex, mpsc};
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
}

/// Handle to a running task.
struct RunningTask {
    /// Join handle for the task.
    handle: JoinHandle<ExecutionResult>,
    /// Channel to send user input.
    input_tx: mpsc::Sender<String>,
    /// Channel to receive events.
    event_rx: Arc<Mutex<mpsc::Receiver<ExecutionEvent>>>,
    /// Current state.
    state: ExecutionState,
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
                event_rx: Arc::new(Mutex::new(event_rx)),
                state,
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
    pub async fn poll_events(&self, task_id: &TaskId) -> Result<Vec<ExecutionEvent>> {
        let running = self
            .running_tasks
            .get(task_id)
            .ok_or_else(|| Error::TaskNotFound { id: task_id.0.clone() })?;

        let mut events = Vec::new();
        let mut rx = running.event_rx.lock().await;

        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        Ok(events)
    }

    /// Check all running tasks and collect completed ones.
    pub async fn poll_completed(&mut self) -> Vec<ExecutionResult> {
        let mut completed = Vec::new();
        let mut to_remove = Vec::new();

        for (task_id, running) in &mut self.running_tasks {
            // Check if task finished
            if running.handle.is_finished() {
                to_remove.push(task_id.clone());
            } else {
                // Update state from events
                let mut rx = running.event_rx.lock().await;
                while let Ok(event) = rx.try_recv() {
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
    // Create or load the agentic loop
    let loop_result = if config.conversation_path.exists() {
        AgenticLoop::load(config.clone(), LlmClientWrapper(llm))
    } else {
        AgenticLoop::new(config.clone(), LlmClientWrapper(llm))
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
}
