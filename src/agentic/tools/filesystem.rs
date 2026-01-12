//! Filesystem tools: read, write, list, glob.

use std::path::PathBuf;

use globwalk::GlobWalkerBuilder;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};
use crate::error::Result;

/// Read file tool.
pub struct ReadFileTool;

impl ReadFileTool {
    pub fn definition() -> Tool {
        Tool {
            name: "read_file".to_string(),
            description: "Read the contents of a file. You should read files before editing them.".to_string(),
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
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let full_path = ctx.working_dir.join(path);

        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => {
                // Track that we read this file
                ctx.track_read(&full_path).await;
                Ok(ToolResult::success(content))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to read file: {}", e))),
        }
    }
}

/// Write file tool.
pub struct WriteFileTool;

impl WriteFileTool {
    pub fn definition() -> Tool {
        Tool {
            name: "write_file".to_string(),
            description: "Write content to a file. Creates parent directories if needed.".to_string(),
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
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'content' argument".to_string()))?;

        let full_path = ctx.working_dir.join(path);

        // Idempotency check: if file already has this exact content, skip the write
        if full_path.exists()
            && let Ok(existing) = tokio::fs::read_to_string(&full_path).await
            && existing == content
        {
            log::debug!("write_file: file {} already has identical content (idempotent)", path);
            return Ok(ToolResult::success(format!(
                "File {} already has identical content (no write needed)",
                path
            )));
        }

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
}

/// List directory tool.
pub struct ListDirectoryTool;

impl ListDirectoryTool {
    pub fn definition() -> Tool {
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
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'path' argument".to_string()))?;

        let full_path = ctx.working_dir.join(path);

        match tokio::fs::read_dir(&full_path).await {
            Ok(mut entries) => {
                let mut names = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let file_type = entry.file_type().await.ok();
                    let suffix = if file_type.is_some_and(|t| t.is_dir()) { "/" } else { "" };
                    names.push(format!("{}{}", name, suffix));
                }
                names.sort();
                Ok(ToolResult::success(names.join("\n")))
            }
            Err(e) => Ok(ToolResult::error(format!("Failed to list directory: {}", e))),
        }
    }
}

/// Glob pattern matching tool.
pub struct GlobTool;

impl GlobTool {
    pub fn definition() -> Tool {
        Tool {
            name: "glob".to_string(),
            description: "Find files matching a glob pattern (e.g., '**/*.rs'). Respects .gitignore.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files (e.g., '**/*.rs', 'src/**/*.ts')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base directory to search in (default: working directory)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'pattern' argument".to_string()))?;

        let base_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| ctx.working_dir.join(p))
            .unwrap_or_else(|| ctx.working_dir.clone());

        // Use globwalk which respects .gitignore
        let walker = match GlobWalkerBuilder::from_patterns(&base_path, &[pattern])
            .max_depth(25)
            .follow_links(false)
            .build()
        {
            Ok(w) => w,
            Err(e) => return Ok(ToolResult::error(format!("Invalid glob pattern: {}", e))),
        };

        let mut matches: Vec<PathBuf> = walker
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .collect();

        // Sort by modification time (newest first) then by path
        matches.sort_by(|a, b| {
            let a_time = std::fs::metadata(a)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let b_time = std::fs::metadata(b)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            b_time.cmp(&a_time).then_with(|| a.cmp(b))
        });

        if matches.is_empty() {
            return Ok(ToolResult::success("No files found matching pattern"));
        }

        // Format output with relative paths
        let output: Vec<String> = matches
            .iter()
            .filter_map(|p| p.strip_prefix(&ctx.working_dir).ok())
            .map(|p| p.display().to_string())
            .collect();

        Ok(ToolResult::success(output.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, ToolContext) {
        let temp = TempDir::new().unwrap();
        let ctx = ToolContext::new(temp.path().to_path_buf());
        (temp, ctx)
    }

    #[tokio::test]
    async fn test_read_file() {
        let (temp, ctx) = setup();

        // Create a test file
        let test_file = temp.path().join("test.txt");
        std::fs::write(&test_file, "Hello, World!").unwrap();

        let result = ReadFileTool::execute(&ctx, &serde_json::json!({"path": "test.txt"}))
            .await
            .unwrap();
        assert_eq!(result.output, "Hello, World!");
        assert!(!result.is_completion);
        assert!(!result.is_failure);

        // Verify file was tracked
        assert!(ctx.was_read(&test_file).await);
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let (_temp, ctx) = setup();

        let result = ReadFileTool::execute(&ctx, &serde_json::json!({"path": "nonexistent.txt"}))
            .await
            .unwrap();
        assert!(result.output.contains("Failed to read file"));
    }

    #[tokio::test]
    async fn test_write_file() {
        let (temp, ctx) = setup();

        let result = WriteFileTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "output.txt",
                "content": "Test content"
            }),
        )
        .await
        .unwrap();
        assert!(result.output.contains("Wrote"));

        // Verify file was written
        let content = std::fs::read_to_string(temp.path().join("output.txt")).unwrap();
        assert_eq!(content, "Test content");
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let (temp, ctx) = setup();

        let result = WriteFileTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "nested/dir/file.txt",
                "content": "Nested content"
            }),
        )
        .await
        .unwrap();
        assert!(result.output.contains("Wrote"));

        // Verify file was written
        let content = std::fs::read_to_string(temp.path().join("nested/dir/file.txt")).unwrap();
        assert_eq!(content, "Nested content");
    }

    #[tokio::test]
    async fn test_list_directory() {
        let (temp, ctx) = setup();

        // Create some files
        std::fs::write(temp.path().join("a.txt"), "").unwrap();
        std::fs::write(temp.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(temp.path().join("subdir")).unwrap();

        let result = ListDirectoryTool::execute(&ctx, &serde_json::json!({"path": "."}))
            .await
            .unwrap();
        assert!(result.output.contains("a.txt"));
        assert!(result.output.contains("b.txt"));
        assert!(result.output.contains("subdir/"));
    }

    #[tokio::test]
    async fn test_glob() {
        let (temp, ctx) = setup();

        // Create some files
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "// lib").unwrap();
        std::fs::write(temp.path().join("README.md"), "# README").unwrap();

        let result = GlobTool::execute(&ctx, &serde_json::json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains("lib.rs"));
        assert!(!result.output.contains("README.md"));
    }

    #[tokio::test]
    async fn test_glob_no_matches() {
        let (_temp, ctx) = setup();

        let result = GlobTool::execute(&ctx, &serde_json::json!({"pattern": "**/*.xyz"}))
            .await
            .unwrap();
        assert!(result.output.contains("No files found"));
    }

    #[tokio::test]
    async fn test_write_file_idempotent() {
        let (temp, ctx) = setup();

        // First write
        let result1 = WriteFileTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "idem.txt",
                "content": "Same content"
            }),
        )
        .await
        .unwrap();
        assert!(result1.output.contains("Wrote"), "First write should succeed");

        // Second write with same content should be idempotent
        let result2 = WriteFileTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "idem.txt",
                "content": "Same content"
            }),
        )
        .await
        .unwrap();
        assert!(
            result2.output.contains("already has identical content"),
            "Second write should be idempotent: {}",
            result2.output
        );

        // Third write with different content should write
        let result3 = WriteFileTool::execute(
            &ctx,
            &serde_json::json!({
                "path": "idem.txt",
                "content": "Different content"
            }),
        )
        .await
        .unwrap();
        assert!(result3.output.contains("Wrote"), "Different content should write");

        // Verify final content
        let content = std::fs::read_to_string(temp.path().join("idem.txt")).unwrap();
        assert_eq!(content, "Different content");
    }
}
