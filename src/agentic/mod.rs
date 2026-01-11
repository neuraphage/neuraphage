//! Agentic loop implementation.
//!
//! The agentic loop is the core execution engine for tasks. It follows these phases:
//! 1. Task Initialization - Set up context and conversation
//! 2. Context Assembly - Build the prompt with relevant context
//! 3. LLM Request/Response - Call the model
//! 4. Tool Execution - Execute any tool calls
//! 5. User Input Handling - Handle user messages
//! 6. Task Termination - Complete or fail the task

pub mod anthropic;
pub mod conversation;
pub mod llm;
mod prompt;
mod tools;

use std::path::PathBuf;

use log::{debug, info};
use tokio::sync::mpsc;

use crate::error::Result;
use crate::executor::ExecutionEvent;
use crate::task::Task;

pub use anthropic::AnthropicClient;
pub use conversation::{Conversation, Message, MessageRole};
pub use llm::{LlmClient, LlmConfig, LlmResponse, StreamChunk};
pub use prompt::{PromptContext, SystemPrompt};
pub use tools::{Tool, ToolCall, ToolExecutor, ToolResult};

/// Configuration for the agentic loop.
#[derive(Debug, Clone)]
pub struct AgenticConfig {
    /// Maximum iterations before forcing termination.
    pub max_iterations: u32,
    /// Maximum tokens per task.
    pub max_tokens: u64,
    /// Maximum cost per task in USD.
    pub max_cost: f64,
    /// Model to use.
    pub model: String,
    /// Working directory for the task.
    pub working_dir: PathBuf,
    /// Path to store conversation.
    pub conversation_path: PathBuf,
}

impl Default for AgenticConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            max_tokens: 1_000_000,
            max_cost: 10.0,
            model: "claude-sonnet-4-20250514".to_string(),
            working_dir: std::env::current_dir().unwrap_or_default(),
            conversation_path: PathBuf::from("."),
        }
    }
}

/// Result of a single iteration.
#[derive(Debug)]
pub enum IterationResult {
    /// Continue with next iteration.
    Continue,
    /// Task completed successfully.
    Completed { reason: String },
    /// Task failed.
    Failed { error: String },
    /// Waiting for user input.
    WaitingForUser { prompt: String },
    /// Blocked on resource or dependency.
    Blocked { reason: String },
    /// Hit iteration/token/cost limit.
    LimitReached { limit: String },
}

/// The agentic loop executor.
pub struct AgenticLoop<L: LlmClient> {
    config: AgenticConfig,
    llm: L,
    tools: ToolExecutor,
    conversation: Conversation,
    iteration: u32,
    tokens_used: u64,
    cost: f64,
    /// Optional event sender for streaming text deltas.
    event_tx: Option<mpsc::Sender<ExecutionEvent>>,
}

impl<L: LlmClient> AgenticLoop<L> {
    /// Create a new agentic loop.
    pub fn new(config: AgenticConfig, llm: L, event_tx: Option<mpsc::Sender<ExecutionEvent>>) -> Result<Self> {
        let conversation = Conversation::new(&config.conversation_path)?;
        let tools = ToolExecutor::new(config.working_dir.clone());

        Ok(Self {
            config,
            llm,
            tools,
            conversation,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
            event_tx,
        })
    }

    /// Load an existing conversation.
    pub fn load(config: AgenticConfig, llm: L, event_tx: Option<mpsc::Sender<ExecutionEvent>>) -> Result<Self> {
        let conversation = Conversation::load(&config.conversation_path)?;
        let tools = ToolExecutor::new(config.working_dir.clone());

        Ok(Self {
            config,
            llm,
            tools,
            conversation,
            iteration: 0,
            tokens_used: 0,
            cost: 0.0,
            event_tx,
        })
    }

    /// Run a single iteration of the loop.
    pub async fn iterate(&mut self, task: &Task) -> Result<IterationResult> {
        self.iteration += 1;
        info!("Starting iteration {} for task {}", self.iteration, task.id);

        // Check limits
        if self.iteration > self.config.max_iterations {
            return Ok(IterationResult::LimitReached {
                limit: format!("max iterations ({})", self.config.max_iterations),
            });
        }

        if self.tokens_used > self.config.max_tokens {
            return Ok(IterationResult::LimitReached {
                limit: format!("max tokens ({})", self.config.max_tokens),
            });
        }

        if self.cost > self.config.max_cost {
            return Ok(IterationResult::LimitReached {
                limit: format!("max cost (${})", self.config.max_cost),
            });
        }

        // Build the context
        let messages = self.build_context(task)?;

        // Signal we're about to call the LLM
        if let Some(event_tx) = &self.event_tx {
            let _ = event_tx
                .send(ExecutionEvent::ActivityChanged {
                    activity: crate::executor::Activity::Thinking,
                })
                .await;
        }

        // Call the LLM (streaming or non-streaming based on event_tx)
        let response = if let Some(event_tx) = &self.event_tx {
            // Use streaming - forward text deltas as they arrive
            let (chunk_tx, mut chunk_rx) = mpsc::channel::<StreamChunk>(100);
            let event_tx_clone = event_tx.clone();
            let event_tx_activity = event_tx.clone();

            // Track if we've sent the streaming activity
            let mut first_chunk = true;

            // Spawn a task to forward text deltas
            let forward_task = tokio::spawn(async move {
                let mut chunk_count = 0;
                while let Some(chunk) = chunk_rx.recv().await {
                    chunk_count += 1;
                    debug!("Received chunk {}: {:?}", chunk_count, chunk);
                    if let StreamChunk::TextDelta(text) = chunk {
                        // Signal streaming started on first chunk
                        if first_chunk {
                            debug!("First text delta, sending Streaming activity");
                            let _ = event_tx_activity
                                .send(ExecutionEvent::ActivityChanged {
                                    activity: crate::executor::Activity::Streaming,
                                })
                                .await;
                            first_chunk = false;
                        }
                        debug!("Forwarding TextDelta: {} bytes", text.len());
                        let _ = event_tx_clone.send(ExecutionEvent::TextDelta { content: text }).await;
                    }
                }
                debug!("Forward task done, processed {} chunks", chunk_count);
            });

            // Call streaming API
            let response = self
                .llm
                .stream(&self.config.model, &messages, self.tools.available_tools(), chunk_tx)
                .await?;

            // Wait for forwarding to complete
            let _ = forward_task.await;

            response
        } else {
            // Non-streaming call
            self.llm
                .complete(&self.config.model, &messages, self.tools.available_tools())
                .await?
        };

        // Signal we're idle after LLM response
        if let Some(event_tx) = &self.event_tx {
            let _ = event_tx
                .send(ExecutionEvent::ActivityChanged {
                    activity: crate::executor::Activity::Idle,
                })
                .await;
        }

        // Update usage
        self.tokens_used += response.tokens_used;
        self.cost += response.cost;

        // Add assistant message to conversation
        self.conversation.add_message(Message {
            role: MessageRole::Assistant,
            content: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
            tool_call_id: None,
        })?;

        // If there are tool calls, execute them
        if !response.tool_calls.is_empty() {
            info!("Executing {} tool calls", response.tool_calls.len());
            for tool_call in &response.tool_calls {
                info!("Executing tool: {} (id: {})", tool_call.name, tool_call.id);

                // Signal tool execution started
                if let Some(event_tx) = &self.event_tx {
                    let _ = event_tx
                        .send(ExecutionEvent::ActivityChanged {
                            activity: crate::executor::Activity::ExecutingTool {
                                name: tool_call.name.clone(),
                            },
                        })
                        .await;
                    let _ = event_tx
                        .send(ExecutionEvent::ToolStarted {
                            name: tool_call.name.clone(),
                        })
                        .await;
                }

                let result = self.tools.execute(tool_call).await?;
                debug!("Tool result (truncated): {:.200}", result.output);

                // Signal tool completed
                if let Some(event_tx) = &self.event_tx {
                    let _ = event_tx
                        .send(ExecutionEvent::ToolCompleted {
                            name: tool_call.name.clone(),
                            result: if result.output.len() > 200 {
                                format!("{}...", &result.output[..200])
                            } else {
                                result.output.clone()
                            },
                        })
                        .await;
                    let _ = event_tx
                        .send(ExecutionEvent::ActivityChanged {
                            activity: crate::executor::Activity::Idle,
                        })
                        .await;
                }

                // Add tool result to conversation
                self.conversation.add_message(Message {
                    role: MessageRole::Tool,
                    content: result.output.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: Some(tool_call.id.clone()),
                })?;

                // Check for special tool results
                if result.is_completion {
                    return Ok(IterationResult::Completed { reason: result.output });
                }

                if result.is_failure {
                    return Ok(IterationResult::Failed { error: result.output });
                }

                if result.requires_user_input {
                    return Ok(IterationResult::WaitingForUser { prompt: result.output });
                }
            }

            return Ok(IterationResult::Continue);
        }

        // No tool calls - check if the response indicates completion
        info!(
            "No tool calls. stop_reason={:?}, content_len={}",
            response.stop_reason,
            response.content.len()
        );
        debug!("Response content (truncated): {:.500}", response.content);

        if response.stop_reason == Some("end_turn".to_string()) {
            // The model ended its turn without tool calls
            // This could mean it's done or waiting for input
            if response.content.contains("TASK_COMPLETE") {
                return Ok(IterationResult::Completed {
                    reason: response.content,
                });
            }
        }

        Ok(IterationResult::Continue)
    }

    /// Add a user message to the conversation.
    pub fn add_user_message(&mut self, content: &str) -> Result<()> {
        self.conversation.add_message(Message {
            role: MessageRole::User,
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
    }

    /// Add a system message to the conversation.
    pub fn add_system_message(&mut self, content: &str) -> Result<()> {
        self.conversation.add_message(Message {
            role: MessageRole::System,
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        })
    }

    /// Get the current iteration count.
    pub fn iteration(&self) -> u32 {
        self.iteration
    }

    /// Get total tokens used.
    pub fn tokens_used(&self) -> u64 {
        self.tokens_used
    }

    /// Get total cost.
    pub fn cost(&self) -> f64 {
        self.cost
    }

    /// Build the context messages for the LLM.
    fn build_context(&self, task: &Task) -> Result<Vec<Message>> {
        let mut messages = Vec::new();

        // System prompt
        messages.push(Message {
            role: MessageRole::System,
            content: self.build_system_prompt(task),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });

        // Add conversation history
        messages.extend(self.conversation.messages().cloned());

        Ok(messages)
    }

    /// Build the system prompt for the task.
    fn build_system_prompt(&self, task: &Task) -> String {
        let context = PromptContext::from_environment(&self.config.working_dir);
        let prompt_builder = SystemPrompt::new(context);
        prompt_builder.build(task)
    }

    /// Save the conversation.
    pub fn save(&self) -> Result<()> {
        self.conversation.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentic::llm::{LlmResponse, MockLlmClient};
    use crate::task::{Task, TaskId, TaskStatus};
    use tempfile::TempDir;

    fn make_test_task(id: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: TaskId(id.to_string()),
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
        }
    }

    #[test]
    fn test_agentic_config_default() {
        let config = AgenticConfig::default();
        assert_eq!(config.max_iterations, 100);
        assert_eq!(config.max_tokens, 1_000_000);
        assert_eq!(config.max_cost, 10.0);
    }

    #[tokio::test]
    async fn test_iterate_with_streaming_forwards_text_deltas() {
        // Create a mock LLM that returns a simple text response
        let mock_llm = MockLlmClient::new(vec![LlmResponse {
            content: "Hello from streaming test!".to_string(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".to_string()),
            tokens_used: 50,
            cost: 0.001,
        }]);

        // Create temp directory for conversation
        let temp_dir = TempDir::new().unwrap();
        let conv_path = temp_dir.path().join("test_conv.json");

        let config = AgenticConfig {
            max_iterations: 10,
            max_tokens: 10000,
            max_cost: 1.0,
            model: "test-model".to_string(),
            working_dir: temp_dir.path().to_path_buf(),
            conversation_path: conv_path,
        };

        // Create event channel
        let (event_tx, mut event_rx) = mpsc::channel::<ExecutionEvent>(100);

        // Create agentic loop with event sender
        let mut agentic_loop = AgenticLoop::new(config, mock_llm, Some(event_tx)).unwrap();

        // Add initial user message
        agentic_loop.add_user_message("Test message").unwrap();

        // Run one iteration
        let task = make_test_task("test-task-1");
        let result = agentic_loop.iterate(&task).await.unwrap();

        // Should continue (no tool calls, no TASK_COMPLETE)
        assert!(matches!(result, IterationResult::Continue));

        // Collect events from channel
        let mut text_deltas = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let ExecutionEvent::TextDelta { content } = event {
                text_deltas.push(content);
            }
        }

        // Should have received text delta(s) containing the response
        let all_text: String = text_deltas.concat();
        assert!(
            all_text.contains("Hello from streaming test!"),
            "Expected text deltas to contain response, got: {}",
            all_text
        );
    }

    #[tokio::test]
    async fn test_iterate_without_streaming_works() {
        // Create a mock LLM
        let mock_llm = MockLlmClient::new(vec![LlmResponse {
            content: "Non-streaming response".to_string(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".to_string()),
            tokens_used: 30,
            cost: 0.0005,
        }]);

        let temp_dir = TempDir::new().unwrap();
        let conv_path = temp_dir.path().join("test_conv2.json");

        let config = AgenticConfig {
            max_iterations: 10,
            max_tokens: 10000,
            max_cost: 1.0,
            model: "test-model".to_string(),
            working_dir: temp_dir.path().to_path_buf(),
            conversation_path: conv_path,
        };

        // Create agentic loop WITHOUT event sender (None)
        let mut agentic_loop = AgenticLoop::new(config, mock_llm, None).unwrap();
        agentic_loop.add_user_message("Test").unwrap();

        // Should work without streaming
        let task = make_test_task("test-task-2");
        let result = agentic_loop.iterate(&task).await.unwrap();
        assert!(matches!(result, IterationResult::Continue));

        // Verify tokens and cost were tracked
        assert_eq!(agentic_loop.tokens_used(), 30);
        assert!((agentic_loop.cost() - 0.0005).abs() < 0.0001);
    }

    #[tokio::test]
    async fn test_conversation_history_preserved_after_streaming() {
        let mock_llm = MockLlmClient::new(vec![LlmResponse {
            content: "Response to be saved".to_string(),
            tool_calls: vec![],
            stop_reason: Some("end_turn".to_string()),
            tokens_used: 20,
            cost: 0.0003,
        }]);

        let temp_dir = TempDir::new().unwrap();
        let conv_path = temp_dir.path().join("test_conv3.json");

        let config = AgenticConfig {
            max_iterations: 10,
            max_tokens: 10000,
            max_cost: 1.0,
            model: "test-model".to_string(),
            working_dir: temp_dir.path().to_path_buf(),
            conversation_path: conv_path.clone(),
        };

        let (event_tx, _event_rx) = mpsc::channel::<ExecutionEvent>(100);
        let mut agentic_loop = AgenticLoop::new(config.clone(), mock_llm, Some(event_tx)).unwrap();
        agentic_loop.add_user_message("User input").unwrap();

        let task = make_test_task("test-task-3");
        let _ = agentic_loop.iterate(&task).await.unwrap();
        agentic_loop.save().unwrap();

        // Load conversation and verify it contains the assistant response
        let loaded_conv = Conversation::load(&conv_path).unwrap();
        let messages: Vec<_> = loaded_conv.messages().collect();

        // Should have user message and assistant response
        assert!(messages.len() >= 2);

        // Find assistant message
        let assistant_msg = messages
            .iter()
            .find(|m| m.role == MessageRole::Assistant)
            .expect("Should have assistant message");
        assert_eq!(assistant_msg.content, "Response to be saved");
    }
}
