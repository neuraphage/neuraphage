//! Tool system for the agentic loop.
//!
//! Tools allow the agent to interact with the environment (filesystem, shell, etc.)

mod bash;
mod control;
mod edit;
mod filesystem;
mod grep;
mod user;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::Result;

pub use bash::BashTool;
pub use control::ControlTools;
pub use edit::EditTool;
pub use filesystem::{GlobTool, ListDirectoryTool, ReadFileTool, WriteFileTool};
pub use grep::GrepTool;
pub use user::AskUserTool;

/// Definition of a tool available to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Tool name (used in tool calls).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON schema for the tool's parameters.
    pub parameters: Value,
}

/// A tool call made by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this call.
    pub id: String,
    /// Name of the tool to invoke.
    pub name: String,
    /// Arguments as JSON.
    pub arguments: Value,
}

/// Result of executing a tool.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Output from the tool (shown to the model).
    pub output: String,
    /// Whether this result indicates task completion.
    pub is_completion: bool,
    /// Whether this result indicates task failure.
    pub is_failure: bool,
    /// Whether user input is required to continue.
    pub requires_user_input: bool,
}

impl ToolResult {
    /// Create a successful result.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_completion: false,
            is_failure: false,
            requires_user_input: false,
        }
    }

    /// Create an error result.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            output: message.into(),
            is_completion: false,
            is_failure: false,
            requires_user_input: false,
        }
    }

    /// Create a completion result.
    pub fn completion(reason: impl Into<String>) -> Self {
        Self {
            output: reason.into(),
            is_completion: true,
            is_failure: false,
            requires_user_input: false,
        }
    }

    /// Create a failure result.
    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            output: error.into(),
            is_completion: false,
            is_failure: true,
            requires_user_input: false,
        }
    }

    /// Create a result requiring user input.
    pub fn needs_user_input(prompt: impl Into<String>) -> Self {
        Self {
            output: prompt.into(),
            is_completion: false,
            is_failure: false,
            requires_user_input: true,
        }
    }
}

/// Context shared across tool executions.
pub struct ToolContext {
    /// Working directory for file operations.
    pub working_dir: PathBuf,
    /// Files that have been read in this session (for Edit validation).
    pub read_files: Arc<Mutex<HashSet<PathBuf>>>,
}

impl ToolContext {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            working_dir,
            read_files: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Track that a file was read.
    pub async fn track_read(&self, path: &Path) {
        let mut read_files = self.read_files.lock().await;
        read_files.insert(path.to_path_buf());
    }

    /// Check if a file was previously read.
    pub async fn was_read(&self, path: &Path) -> bool {
        let read_files = self.read_files.lock().await;
        read_files.contains(path)
    }
}

/// Executor for tools.
pub struct ToolExecutor {
    /// Shared context.
    context: ToolContext,
    /// Available tools.
    tools: Vec<Tool>,
}

impl ToolExecutor {
    /// Create a new tool executor.
    pub fn new(working_dir: PathBuf) -> Self {
        let context = ToolContext::new(working_dir);
        let tools = Self::default_tools();
        Self { context, tools }
    }

    /// Get available tools.
    pub fn available_tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Execute a tool call.
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        match call.name.as_str() {
            "read_file" => ReadFileTool::execute(&self.context, &call.arguments).await,
            "write_file" => WriteFileTool::execute(&self.context, &call.arguments).await,
            "list_directory" => ListDirectoryTool::execute(&self.context, &call.arguments).await,
            "glob" => GlobTool::execute(&self.context, &call.arguments).await,
            "grep" => GrepTool::execute(&self.context, &call.arguments).await,
            "edit" => EditTool::execute(&self.context, &call.arguments).await,
            "run_command" => BashTool::execute(&self.context, &call.arguments).await,
            "ask_user" => AskUserTool::execute(&call.arguments).await,
            "complete_task" => ControlTools::complete(&call.arguments).await,
            "fail_task" => ControlTools::fail(&call.arguments).await,
            _ => Ok(ToolResult::error(format!("Unknown tool: {}", call.name))),
        }
    }

    /// Define default tools.
    fn default_tools() -> Vec<Tool> {
        vec![
            ReadFileTool::definition(),
            WriteFileTool::definition(),
            ListDirectoryTool::definition(),
            GlobTool::definition(),
            GrepTool::definition(),
            EditTool::definition(),
            BashTool::definition(),
            AskUserTool::definition(),
            ControlTools::complete_definition(),
            ControlTools::fail_definition(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ToolExecutor) {
        let temp = TempDir::new().unwrap();
        let executor = ToolExecutor::new(temp.path().to_path_buf());
        (temp, executor)
    }

    #[test]
    fn test_default_tools() {
        let (_temp, executor) = setup();
        let tools = executor.available_tools();
        assert!(tools.iter().any(|t| t.name == "read_file"));
        assert!(tools.iter().any(|t| t.name == "write_file"));
        assert!(tools.iter().any(|t| t.name == "list_directory"));
        assert!(tools.iter().any(|t| t.name == "glob"));
        assert!(tools.iter().any(|t| t.name == "grep"));
        assert!(tools.iter().any(|t| t.name == "edit"));
        assert!(tools.iter().any(|t| t.name == "run_command"));
        assert!(tools.iter().any(|t| t.name == "ask_user"));
        assert!(tools.iter().any(|t| t.name == "complete_task"));
        assert!(tools.iter().any(|t| t.name == "fail_task"));
    }

    #[test]
    fn test_tool_result_constructors() {
        let success = ToolResult::success("ok");
        assert!(!success.is_completion);
        assert!(!success.is_failure);
        assert!(!success.requires_user_input);

        let error = ToolResult::error("failed");
        assert!(!error.is_completion);
        assert!(!error.is_failure);
        assert!(!error.requires_user_input);

        let completion = ToolResult::completion("done");
        assert!(completion.is_completion);
        assert!(!completion.is_failure);

        let failure = ToolResult::failure("crashed");
        assert!(!failure.is_completion);
        assert!(failure.is_failure);

        let needs_input = ToolResult::needs_user_input("question?");
        assert!(needs_input.requires_user_input);
    }

    #[tokio::test]
    async fn test_tool_context_tracking() {
        let context = ToolContext::new(PathBuf::from("/tmp"));
        let path = PathBuf::from("/tmp/test.txt");

        assert!(!context.was_read(&path).await);
        context.track_read(&path).await;
        assert!(context.was_read(&path).await);
    }
}
