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
pub mod tools;

use std::path::PathBuf;

use crate::error::Result;
use crate::task::Task;

pub use anthropic::AnthropicClient;
pub use conversation::{Conversation, Message, MessageRole};
pub use llm::{LlmClient, LlmConfig, LlmResponse};
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
}

impl<L: LlmClient> AgenticLoop<L> {
    /// Create a new agentic loop.
    pub fn new(config: AgenticConfig, llm: L) -> Result<Self> {
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
        })
    }

    /// Load an existing conversation.
    pub fn load(config: AgenticConfig, llm: L) -> Result<Self> {
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
        })
    }

    /// Run a single iteration of the loop.
    pub async fn iterate(&mut self, task: &Task) -> Result<IterationResult> {
        self.iteration += 1;

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

        // Call the LLM
        let response = self
            .llm
            .complete(&self.config.model, &messages, self.tools.available_tools())
            .await?;

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
            for tool_call in &response.tool_calls {
                let result = self.tools.execute(tool_call).await?;

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
        let mut prompt = String::new();

        prompt.push_str("You are an AI assistant executing a task.\n\n");
        prompt.push_str(&format!("## Task\n{}\n\n", task.description));

        if let Some(context) = &task.context {
            prompt.push_str(&format!("## Context\n{}\n\n", context));
        }

        prompt.push_str(&format!(
            "## Working Directory\n{}\n\n",
            self.config.working_dir.display()
        ));

        prompt.push_str("## Instructions\n");
        prompt.push_str("- Use the available tools to complete the task\n");
        prompt.push_str("- Be thorough and verify your work\n");
        prompt.push_str("- If you need user input, use the ask_user tool\n");
        prompt.push_str("- When complete, use the complete_task tool\n");
        prompt.push_str("- If you encounter an unrecoverable error, use the fail_task tool\n");

        prompt
    }

    /// Save the conversation.
    pub fn save(&self) -> Result<()> {
        self.conversation.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agentic_config_default() {
        let config = AgenticConfig::default();
        assert_eq!(config.max_iterations, 100);
        assert_eq!(config.max_tokens, 1_000_000);
        assert_eq!(config.max_cost, 10.0);
    }
}
