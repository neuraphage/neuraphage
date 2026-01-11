//! Grep tool: regex search with ripgrep integration.

use std::process::Stdio;

use regex::Regex;
use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};
use crate::error::Result;

/// Grep tool - searches files using regex patterns.
pub struct GrepTool;

impl GrepTool {
    pub fn definition() -> Tool {
        Tool {
            name: "grep".to_string(),
            description: "Search for regex patterns in files. Uses ripgrep (rg) if available for speed.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search in (default: working directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Glob pattern to filter files (e.g., '*.rs', '*.{ts,tsx}')"
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Case insensitive search (default: false)"
                    },
                    "context_lines": {
                        "type": "integer",
                        "description": "Number of context lines before and after match (default: 0)"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 100)"
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

        let search_path = args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| ctx.working_dir.join(p))
            .unwrap_or_else(|| ctx.working_dir.clone());

        let glob_pattern = args.get("glob").and_then(|v| v.as_str());
        let case_insensitive = args.get("case_insensitive").and_then(|v| v.as_bool()).unwrap_or(false);
        let context_lines = args.get("context_lines").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
        let max_results = args.get("max_results").and_then(|v| v.as_i64()).unwrap_or(100) as usize;

        // Try ripgrep first (much faster for large codebases)
        if let Ok(result) = Self::try_ripgrep(
            pattern,
            &search_path,
            glob_pattern,
            case_insensitive,
            context_lines,
            max_results,
        )
        .await
        {
            return Ok(result);
        }

        // Fall back to built-in search
        Self::builtin_search(
            pattern,
            &search_path,
            glob_pattern,
            case_insensitive,
            context_lines,
            max_results,
            &ctx.working_dir,
        )
        .await
    }

    async fn try_ripgrep(
        pattern: &str,
        search_path: &std::path::Path,
        glob_pattern: Option<&str>,
        case_insensitive: bool,
        context_lines: usize,
        max_results: usize,
    ) -> std::result::Result<ToolResult, ()> {
        let mut cmd = tokio::process::Command::new("rg");

        cmd.arg("--line-number")
            .arg("--color=never")
            .arg("--no-heading")
            .arg(format!("--max-count={}", max_results));

        if case_insensitive {
            cmd.arg("--ignore-case");
        }

        if context_lines > 0 {
            cmd.arg(format!("-C{}", context_lines));
        }

        if let Some(glob) = glob_pattern {
            cmd.arg("--glob").arg(glob);
        }

        cmd.arg(pattern).arg(search_path);

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let output = cmd.output().await.map_err(|_| ())?;

        if output.status.success() || output.status.code() == Some(1) {
            // rg returns 1 if no matches found
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                Ok(ToolResult::success("No matches found"))
            } else {
                Ok(ToolResult::success(stdout.to_string()))
            }
        } else {
            Err(())
        }
    }

    async fn builtin_search(
        pattern: &str,
        search_path: &std::path::Path,
        glob_pattern: Option<&str>,
        case_insensitive: bool,
        context_lines: usize,
        max_results: usize,
        working_dir: &std::path::Path,
    ) -> Result<ToolResult> {
        // Compile regex
        let regex = if case_insensitive {
            Regex::new(&format!("(?i){}", pattern))
        } else {
            Regex::new(pattern)
        };

        let regex = match regex {
            Ok(r) => r,
            Err(e) => return Ok(ToolResult::error(format!("Invalid regex pattern: {}", e))),
        };

        let mut results = Vec::new();
        let mut files_to_search = Vec::new();

        // Collect files to search
        if search_path.is_file() {
            files_to_search.push(search_path.to_path_buf());
        } else if search_path.is_dir() {
            Self::collect_files(&search_path.to_path_buf(), glob_pattern, &mut files_to_search);
        }

        // Search each file
        for file_path in files_to_search {
            if results.len() >= max_results {
                break;
            }

            if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
                let lines: Vec<&str> = content.lines().collect();
                let relative_path = file_path
                    .strip_prefix(working_dir)
                    .unwrap_or(&file_path)
                    .display()
                    .to_string();

                for (line_num, line) in lines.iter().enumerate() {
                    if results.len() >= max_results {
                        break;
                    }

                    if regex.is_match(line) {
                        if context_lines > 0 {
                            // Add context lines
                            let start = line_num.saturating_sub(context_lines);
                            let end = (line_num + context_lines + 1).min(lines.len());

                            for (i, ctx_line) in lines.iter().enumerate().take(end).skip(start) {
                                let prefix = if i == line_num { ">" } else { " " };
                                results.push(format!("{}:{}:{}{}", relative_path, i + 1, prefix, ctx_line));
                            }
                            if end < lines.len() {
                                results.push("--".to_string());
                            }
                        } else {
                            results.push(format!("{}:{}:{}", relative_path, line_num + 1, line));
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(ToolResult::success("No matches found"))
        } else {
            Ok(ToolResult::success(results.join("\n")))
        }
    }

    fn collect_files(dir: &std::path::PathBuf, glob_pattern: Option<&str>, files: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Skip hidden directories and common ignored dirs
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !name.starts_with('.') && name != "node_modules" && name != "target" {
                        Self::collect_files(&path, glob_pattern, files);
                    }
                } else if path.is_file() {
                    // Check if file matches glob pattern
                    if let Some(pattern) = glob_pattern {
                        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                        if !Self::matches_glob(file_name, pattern) {
                            continue;
                        }
                    }
                    files.push(path);
                }
            }
        }
    }

    fn matches_glob(filename: &str, pattern: &str) -> bool {
        // Simple glob matching for common patterns
        if pattern.starts_with("*.") && !pattern.contains('{') {
            let ext = &pattern[2..];
            filename.ends_with(&format!(".{}", ext))
        } else if pattern.contains('{') && pattern.contains('}') {
            // Handle *.{ext1,ext2} patterns
            let start = pattern.find('{').unwrap();
            let end = pattern.find('}').unwrap();
            let extensions: Vec<&str> = pattern[start + 1..end].split(',').collect();

            extensions.iter().any(|ext| filename.ends_with(&format!(".{}", ext)))
        } else {
            filename == pattern
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
    async fn test_grep_simple() {
        let (temp, ctx) = setup();

        // Create test files
        std::fs::write(temp.path().join("test.txt"), "Hello World\nfoo bar\nbaz").unwrap();

        let result = GrepTool::execute(
            &ctx,
            &serde_json::json!({
                "pattern": "foo",
                "path": "test.txt"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("foo bar"));
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let (temp, ctx) = setup();

        std::fs::write(temp.path().join("test.txt"), "Hello World\nFOO BAR\nbaz").unwrap();

        let result = GrepTool::execute(
            &ctx,
            &serde_json::json!({
                "pattern": "foo",
                "path": "test.txt",
                "case_insensitive": true
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("FOO BAR"));
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let (temp, ctx) = setup();

        std::fs::write(temp.path().join("test.txt"), "Hello World").unwrap();

        let result = GrepTool::execute(
            &ctx,
            &serde_json::json!({
                "pattern": "xyz",
                "path": "test.txt"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("No matches found"));
    }

    #[tokio::test]
    async fn test_grep_regex() {
        let (temp, ctx) = setup();

        std::fs::write(temp.path().join("test.txt"), "fn main() {}\nfn test() {}").unwrap();

        let result = GrepTool::execute(
            &ctx,
            &serde_json::json!({
                "pattern": r"fn \w+\(\)",
                "path": "test.txt"
            }),
        )
        .await
        .unwrap();

        assert!(result.output.contains("fn main()"));
        assert!(result.output.contains("fn test()"));
    }

    #[test]
    fn test_matches_glob() {
        assert!(GrepTool::matches_glob("test.rs", "*.rs"));
        assert!(!GrepTool::matches_glob("test.txt", "*.rs"));
        assert!(GrepTool::matches_glob("test.ts", "*.{ts,tsx}"));
        assert!(GrepTool::matches_glob("test.tsx", "*.{ts,tsx}"));
        assert!(!GrepTool::matches_glob("test.js", "*.{ts,tsx}"));
    }
}
