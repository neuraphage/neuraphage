//! Todo tool: task list management.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// TodoWrite tool - manages a task list during the session.
pub struct TodoWriteTool;

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// The task content.
    pub content: String,
    /// Current status: pending, in_progress, or completed.
    pub status: TodoStatus,
    /// Active form of the task (e.g., "Running tests" for "Run tests").
    pub active_form: String,
}

/// Status of a todo item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Completed => write!(f, "completed"),
        }
    }
}

impl TodoWriteTool {
    pub fn definition() -> Tool {
        Tool {
            name: "todo_write".to_string(),
            description: "Update the task list to track progress on complex tasks. \
                          Use this to plan multi-step work and show progress to the user."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "description": "The updated todo list",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {
                                    "type": "string",
                                    "description": "The task description (imperative form)"
                                },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"],
                                    "description": "Current status of the task"
                                },
                                "active_form": {
                                    "type": "string",
                                    "description": "Present continuous form (e.g., 'Running tests')"
                                }
                            },
                            "required": ["content", "status", "active_form"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        }
    }

    pub async fn execute(args: &Value) -> Result<ToolResult> {
        let todos = args
            .get("todos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'todos' argument".to_string()))?;

        let mut parsed_todos: Vec<TodoItem> = Vec::new();

        for (i, todo) in todos.iter().enumerate() {
            let content = todo
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::error::Error::Validation(format!("Missing 'content' in todo {}", i)))?;

            let status_str = todo
                .get("status")
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::error::Error::Validation(format!("Missing 'status' in todo {}", i)))?;

            let status = match status_str {
                "pending" => TodoStatus::Pending,
                "in_progress" => TodoStatus::InProgress,
                "completed" => TodoStatus::Completed,
                _ => {
                    return Err(crate::error::Error::Validation(format!(
                        "Invalid status '{}' in todo {}",
                        status_str, i
                    )));
                }
            };

            let active_form = todo
                .get("active_form")
                .and_then(|v| v.as_str())
                .ok_or_else(|| crate::error::Error::Validation(format!("Missing 'active_form' in todo {}", i)))?;

            parsed_todos.push(TodoItem {
                content: content.to_string(),
                status,
                active_form: active_form.to_string(),
            });
        }

        // Format the output
        let mut output = String::from("Todo list updated:\n\n");
        for todo in &parsed_todos {
            let marker = match todo.status {
                TodoStatus::Pending => "○",
                TodoStatus::InProgress => "●",
                TodoStatus::Completed => "✓",
            };
            output.push_str(&format!("{} {} [{}]\n", marker, todo.content, todo.status));
        }

        // Count by status
        let pending = parsed_todos.iter().filter(|t| t.status == TodoStatus::Pending).count();
        let in_progress = parsed_todos
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();
        let completed = parsed_todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();

        output.push_str(&format!(
            "\nSummary: {} pending, {} in progress, {} completed",
            pending, in_progress, completed
        ));

        Ok(ToolResult::success(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_todo_write() {
        let result = TodoWriteTool::execute(&serde_json::json!({
            "todos": [
                {"content": "Read the file", "status": "completed", "active_form": "Reading file"},
                {"content": "Fix the bug", "status": "in_progress", "active_form": "Fixing bug"},
                {"content": "Write tests", "status": "pending", "active_form": "Writing tests"}
            ]
        }))
        .await
        .unwrap();

        assert!(result.output.contains("Read the file"));
        assert!(result.output.contains("Fix the bug"));
        assert!(result.output.contains("Write tests"));
        assert!(result.output.contains("1 pending"));
        assert!(result.output.contains("1 in progress"));
        assert!(result.output.contains("1 completed"));
    }

    #[tokio::test]
    async fn test_todo_write_empty() {
        let result = TodoWriteTool::execute(&serde_json::json!({"todos": []})).await.unwrap();
        assert!(result.output.contains("0 pending"));
    }

    #[tokio::test]
    async fn test_todo_write_invalid_status() {
        let result = TodoWriteTool::execute(&serde_json::json!({
            "todos": [
                {"content": "Task", "status": "invalid", "active_form": "Tasking"}
            ]
        }))
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_todo_status_display() {
        assert_eq!(format!("{}", TodoStatus::Pending), "pending");
        assert_eq!(format!("{}", TodoStatus::InProgress), "in_progress");
        assert_eq!(format!("{}", TodoStatus::Completed), "completed");
    }
}
