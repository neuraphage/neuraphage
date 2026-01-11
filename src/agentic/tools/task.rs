//! Task tool: spawn subagent tasks.

use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// Task tool - spawns a subagent to handle complex work.
pub struct TaskTool;

impl TaskTool {
    pub fn definition() -> Tool {
        Tool {
            name: "spawn_task".to_string(),
            description: "Spawn a subagent to handle a complex, multi-step task autonomously. \
                          Use this for tasks that require exploration, research, or multiple operations \
                          that can run independently."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A clear description of what the subagent should accomplish"
                    },
                    "context": {
                        "type": "string",
                        "description": "Additional context or constraints for the task"
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Priority (1-100, higher is more important, default: 50)"
                    }
                },
                "required": ["description"]
            }),
        }
    }

    pub async fn execute(args: &Value) -> Result<ToolResult> {
        let description = args
            .get("description")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'description' argument".to_string()))?;

        let context = args.get("context").and_then(|v| v.as_str());
        let priority = args.get("priority").and_then(|v| v.as_i64()).unwrap_or(50) as u8;

        // For now, we return a result indicating the task should be spawned.
        // The actual spawning is handled by the executor/daemon.
        // In a full implementation, this would create a task via the daemon client.

        let mut output = format!(
            "Task queued for execution:\n\
             Description: {}\n\
             Priority: {}",
            description, priority
        );

        if let Some(ctx) = context {
            output.push_str(&format!("\nContext: {}", ctx));
        }

        // Mark that this requires spawning a subtask
        // The executor will handle the actual creation
        Ok(ToolResult::success(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_task() {
        let result = TaskTool::execute(&serde_json::json!({
            "description": "Explore the authentication module"
        }))
        .await
        .unwrap();
        assert!(result.output.contains("Task queued"));
        assert!(result.output.contains("authentication module"));
    }

    #[tokio::test]
    async fn test_spawn_task_with_context() {
        let result = TaskTool::execute(&serde_json::json!({
            "description": "Fix the bug",
            "context": "The bug is in auth.rs",
            "priority": 80
        }))
        .await
        .unwrap();
        assert!(result.output.contains("Priority: 80"));
        assert!(result.output.contains("Context: The bug is in auth.rs"));
    }

    #[tokio::test]
    async fn test_spawn_task_missing_description() {
        let result = TaskTool::execute(&serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
