//! User interaction tool: ask_user.

use serde_json::Value;

use super::{Tool, ToolResult};
use crate::error::Result;

/// Ask user tool - prompts for user input.
pub struct AskUserTool;

impl AskUserTool {
    pub fn definition() -> Tool {
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
        }
    }

    pub async fn execute(args: &Value) -> Result<ToolResult> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'question' argument".to_string()))?;

        Ok(ToolResult::needs_user_input(question.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ask_user() {
        let result = AskUserTool::execute(&serde_json::json!({"question": "What is your name?"}))
            .await
            .unwrap();
        assert!(result.requires_user_input);
        assert_eq!(result.output, "What is your name?");
    }
}
