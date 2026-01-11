//! Tool system for the agentic loop.
//!
//! Tools allow the agent to interact with the environment (filesystem, shell, etc.)

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

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

/// Executor for tools.
pub struct ToolExecutor {
    /// Working directory for file operations.
    working_dir: PathBuf,
    /// Available tools.
    tools: Vec<Tool>,
}

impl ToolExecutor {
    /// Create a new tool executor.
    pub fn new(working_dir: PathBuf) -> Self {
        let tools = Self::default_tools();
        Self { working_dir, tools }
    }

    /// Get available tools.
    pub fn available_tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Execute a tool call.
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult> {
        match call.name.as_str() {
            "read_file" => self.execute_read_file(&call.arguments).await,
            "write_file" => self.execute_write_file(&call.arguments).await,
            "list_directory" => self.execute_list_directory(&call.arguments).await,
            "run_command" => self.execute_run_command(&call.arguments).await,
            "ask_user" => self.execute_ask_user(&call.arguments).await,
            "complete_task" => self.execute_complete_task(&call.arguments).await,
            "fail_task" => self.execute_fail_task(&call.arguments).await,
            _ => Ok(ToolResult::error(format!("Unknown tool: {}", call.name))),
        }
    }

    /// Define default tools.
    fn default_tools() -> Vec<Tool> {
        vec![
            Tool {
                name: "read_file".to_string(),
                description: "Read the contents of a file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to read (relative to working directory)"
                        }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "write_file".to_string(),
                description: "Write content to a file.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to write (relative to working directory)"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write to the file"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
            Tool {
                name: "list_directory".to_string(),
                description: "List the contents of a directory.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the directory (relative to working directory)"
                        }
                    },
                    "required": ["path"]
                }),
            },
            Tool {
                name: "run_command".to_string(),
                description: "Run a shell command.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command to run"
                        }
                    },
                    "required": ["command"]
                }),
            },
            Tool {
                name: "ask_user".to_string(),
                description: "Ask the user a question and wait for their response.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question to ask the user"
                        }
                    },
                    "required": ["question"]
                }),
            },
            Tool {
                name: "complete_task".to_string(),
                description: "Mark the task as successfully completed.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "Summary of what was accomplished"
                        }
                    },
                    "required": ["summary"]
                }),
            },
            Tool {
                name: "fail_task".to_string(),
                description: "Mark the task as failed due to an unrecoverable error.".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reason": {
                            "type": "string",
                            "description": "Reason for failure"
                        }
                    },
                    "required": ["reason"]
                }),
            },
        ]
    }

    /// Read a file.
    async fn execute_read_file(&self, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let full_path = self.working_dir.join(path);

        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => Ok(ToolResult::success(content)),
            Err(e) => Ok(ToolResult::error(format!("Failed to read file: {}", e))),
        }
    }

    /// Write a file.
    async fn execute_write_file(&self, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'content' argument".to_string()))?;

        let full_path = self.working_dir.join(path);

        // Ensure parent directory exists
        if let Some(parent) = full_path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(ToolResult::error(format!("Failed to create directory: {}", e)));
        }

        match tokio::fs::write(&full_path, content).await {
            Ok(()) => Ok(ToolResult::success(format!(
                "Wrote {} bytes to {}",
                content.len(),
                path
            ))),
            Err(e) => Ok(ToolResult::error(format!("Failed to write file: {}", e))),
        }
    }

    /// List directory contents.
    async fn execute_list_directory(&self, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let full_path = self.working_dir.join(path);

        match tokio::fs::read_dir(&full_path).await {
            Ok(mut entries) => {
                let mut names = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let file_type = entry.file_type().await.ok();
                    let suffix = if file_type.map(|t| t.is_dir()).unwrap_or(false) { "/" } else { "" };
                    names.push(format!("{}{}", name, suffix));
                }
                names.sort();
                Ok(ToolResult::success(names.join("\n")))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to list directory: {}", e))),
        }
    }

    /// Run a shell command.
    async fn execute_run_command(&self, args: &Value) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'command' argument".to_string()))?;

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.working_dir)
            .output()
            .await;

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let result = if output.status.success() {
                    format!("Exit code: 0\n\nstdout:\n{}\n\nstderr:\n{}", stdout, stderr)
                } else {
                    format!(
                        "Exit code: {}\n\nstdout:\n{}\n\nstderr:\n{}",
                        output.status.code().unwrap_or(-1),
                        stdout,
                        stderr
                    )
                };
                Ok(ToolResult::success(result))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to run command: {}", e))),
        }
    }

    /// Ask the user a question.
    async fn execute_ask_user(&self, args: &Value) -> Result<ToolResult> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'question' argument".to_string()))?;

        Ok(ToolResult::needs_user_input(question.to_string()))
    }

    /// Complete the task.
    async fn execute_complete_task(&self, args: &Value) -> Result<ToolResult> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'summary' argument".to_string()))?;

        Ok(ToolResult::completion(summary.to_string()))
    }

    /// Fail the task.
    async fn execute_fail_task(&self, args: &Value) -> Result<ToolResult> {
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'reason' argument".to_string()))?;

        Ok(ToolResult::failure(reason.to_string()))
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
        assert!(tools.iter().any(|t| t.name == "run_command"));
        assert!(tools.iter().any(|t| t.name == "ask_user"));
        assert!(tools.iter().any(|t| t.name == "complete_task"));
        assert!(tools.iter().any(|t| t.name == "fail_task"));
    }

    #[tokio::test]
    async fn test_read_file() {
        let (temp, executor) = setup();

        // Create a test file
        let test_file = temp.path().join("test.txt");
        std::fs::write(&test_file, "Hello, World!").unwrap();

        let call = ToolCall {
            id: "test".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "test.txt"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert_eq!(result.output, "Hello, World!");
        assert!(!result.is_completion);
        assert!(!result.is_failure);
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "nonexistent.txt"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Failed to read file"));
    }

    #[tokio::test]
    async fn test_write_file() {
        let (temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": "output.txt",
                "content": "Test content"
            }),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Wrote"));

        // Verify file was written
        let content = std::fs::read_to_string(temp.path().join("output.txt")).unwrap();
        assert_eq!(content, "Test content");
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let (temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "write_file".to_string(),
            arguments: serde_json::json!({
                "path": "nested/dir/file.txt",
                "content": "Nested content"
            }),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Wrote"));

        // Verify file was written
        let content = std::fs::read_to_string(temp.path().join("nested/dir/file.txt")).unwrap();
        assert_eq!(content, "Nested content");
    }

    #[tokio::test]
    async fn test_list_directory() {
        let (temp, executor) = setup();

        // Create some files
        std::fs::write(temp.path().join("a.txt"), "").unwrap();
        std::fs::write(temp.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(temp.path().join("subdir")).unwrap();

        let call = ToolCall {
            id: "test".to_string(),
            name: "list_directory".to_string(),
            arguments: serde_json::json!({"path": "."}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("a.txt"));
        assert!(result.output.contains("b.txt"));
        assert!(result.output.contains("subdir/"));
    }

    #[tokio::test]
    async fn test_run_command() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "run_command".to_string(),
            arguments: serde_json::json!({"command": "echo hello"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Exit code: 0"));
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_run_command_failure() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "run_command".to_string(),
            arguments: serde_json::json!({"command": "exit 1"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_ask_user() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "ask_user".to_string(),
            arguments: serde_json::json!({"question": "What is your name?"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.requires_user_input);
        assert_eq!(result.output, "What is your name?");
    }

    #[tokio::test]
    async fn test_complete_task() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "complete_task".to_string(),
            arguments: serde_json::json!({"summary": "All done!"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.is_completion);
        assert_eq!(result.output, "All done!");
    }

    #[tokio::test]
    async fn test_fail_task() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "fail_task".to_string(),
            arguments: serde_json::json!({"reason": "Something went wrong"}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.is_failure);
        assert_eq!(result.output, "Something went wrong");
    }

    #[tokio::test]
    async fn test_unknown_tool() {
        let (_temp, executor) = setup();

        let call = ToolCall {
            id: "test".to_string(),
            name: "nonexistent_tool".to_string(),
            arguments: serde_json::json!({}),
        };

        let result = executor.execute(&call).await.unwrap();
        assert!(result.output.contains("Unknown tool"));
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
}
