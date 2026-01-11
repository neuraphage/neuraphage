//! Edit tool: precise string replacement in files.

use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};
use crate::error::Result;

/// Edit tool - replaces exact strings in files.
pub struct EditTool;

impl EditTool {
    pub fn definition() -> Tool {
        Tool {
            name: "edit".to_string(),
            description: "Replace exact string occurrences in a file. You must read the file first before editing. The old_string must be unique in the file unless replace_all is true.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit (relative to working directory)"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact string to replace (must be unique unless replace_all is true)"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The string to replace it with"
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurrences instead of requiring uniqueness (default: false)"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let old_string = args
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'old_string' argument".to_string()))?;

        let new_string = args
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'new_string' argument".to_string()))?;

        let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        let full_path = ctx.working_dir.join(path);

        // Check if file was previously read
        if !ctx.was_read(&full_path).await {
            return Ok(ToolResult::error(format!(
                "You must read the file '{}' before editing it. Use the read_file tool first.",
                path
            )));
        }

        // Read current content
        let content = match tokio::fs::read_to_string(&full_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(format!("Failed to read file: {}", e))),
        };

        // Check if old_string is different from new_string
        if old_string == new_string {
            return Ok(ToolResult::error(
                "old_string and new_string are identical - no edit needed",
            ));
        }

        // Count occurrences
        let occurrence_count = content.matches(old_string).count();

        if occurrence_count == 0 {
            return Ok(ToolResult::error(
                "old_string not found in file. Make sure you're using the exact text including whitespace and indentation.".to_string()
            ));
        }

        if !replace_all && occurrence_count > 1 {
            return Ok(ToolResult::error(format!(
                "old_string appears {} times in the file. Either provide more context to make it unique, or set replace_all to true to replace all occurrences.",
                occurrence_count
            )));
        }

        // Perform replacement
        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        // Write back
        match tokio::fs::write(&full_path, &new_content).await {
            Ok(()) => {
                let msg = if replace_all && occurrence_count > 1 {
                    format!("Replaced {} occurrences in {}", occurrence_count, path)
                } else {
                    format!("Successfully edited {}", path)
                };
                Ok(ToolResult::success(msg))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to write file: {}", e))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ToolContext) {
        let temp = TempDir::new().unwrap();
        let ctx = super::super::ToolContext::new(temp.path().to_path_buf());
        (temp, ctx)
    }

    #[tokio::test]
    async fn test_edit_requires_read_first() {
        let (temp, ctx) = setup();

        std::fs::write(temp.path().join("test.txt"), "Hello World").unwrap();

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "Hello",
                "new_string": "Hi"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("must read the file"));
    }

    #[tokio::test]
    async fn test_edit_success() {
        let (temp, ctx) = setup();

        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "Hello World").unwrap();

        // Track that we read the file
        ctx.track_read(&file_path).await;

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "Hello",
                "new_string": "Hi"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("Successfully edited"));

        // Verify the edit
        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "Hi World");
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let (temp, ctx) = setup();

        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "Hello World").unwrap();
        ctx.track_read(&file_path).await;

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "xyz",
                "new_string": "abc"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn test_edit_not_unique() {
        let (temp, ctx) = setup();

        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();
        ctx.track_read(&file_path).await;

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "foo",
                "new_string": "qux"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("appears 3 times"));
    }

    #[tokio::test]
    async fn test_edit_replace_all() {
        let (temp, ctx) = setup();

        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "foo bar foo baz foo").unwrap();
        ctx.track_read(&file_path).await;

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "foo",
                "new_string": "qux",
                "replace_all": true
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("Replaced 3 occurrences"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "qux bar qux baz qux");
    }

    #[tokio::test]
    async fn test_edit_same_string() {
        let (temp, ctx) = setup();

        let file_path = temp.path().join("test.txt");
        std::fs::write(&file_path, "Hello World").unwrap();
        ctx.track_read(&file_path).await;

        let result = EditTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "test.txt",
                "old_string": "Hello",
                "new_string": "Hello"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("identical"));
    }
}
