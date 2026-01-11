//! Control tools: complete_task and fail_task.

use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// Control tools for task completion/failure.
pub struct ControlTools;

impl ControlTools {
    pub fn complete_definition() -> Tool {
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
        }
    }

    pub fn fail_definition() -> Tool {
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
        }
    }

    pub async fn complete(args: &Value) -> Result<ToolResult> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'summary' argument".to_string()))?;

        Ok(ToolResult::completion(summary.to_string()))
    }

    pub async fn fail(args: &Value) -> Result<ToolResult> {
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

    #[tokio::test]
    async fn test_complete_task() {
        let result = ControlTools::complete(&serde_json::json!({"summary": "All done!"}))
            .await
            .unwrap();
        assert!(result.is_completion);
        assert_eq!(result.output, "All done!");
    }

    #[tokio::test]
    async fn test_fail_task() {
        let result = ControlTools::fail(&serde_json::json!({"reason": "Something went wrong"}))
            .await
            .unwrap();
        assert!(result.is_failure);
        assert_eq!(result.output, "Something went wrong");
    }
}
