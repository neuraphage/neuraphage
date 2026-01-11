//! Bash tool: command execution.

use serde_json::Value;

use super::{Tool, ToolContext, ToolResult};
use crate::error::Result;
use crate::sandbox::{self, SandboxAvailability, SandboxConfig};

/// Bash tool - runs shell commands.
pub struct BashTool;

impl BashTool {
    pub fn definition() -> Tool {
        Tool {
            name: "run_command".to_string(),
            description: "Run a shell command in the working directory.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to run"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds (default: 120000)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::Error::Validation("Missing 'command' argument".to_string()))?;

        let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_i64()).unwrap_or(120_000) as u64;

        // Determine if we should use sandboxing
        let use_sandbox =
            ctx.sandbox_enabled && matches!(sandbox::check_availability(), SandboxAvailability::Available);

        let output = if use_sandbox {
            // Run command in sandbox
            let config = SandboxConfig::for_working_dir(&ctx.working_dir);
            let mut cmd = sandbox::wrap_command_async(&config, command);
            cmd.current_dir(&ctx.working_dir);

            tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), cmd.output()).await
        } else {
            // Run command directly
            tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms),
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&ctx.working_dir)
                    .output(),
            )
            .await
        };

        match output {
            Ok(Ok(output)) => {
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
            Ok(Err(e)) => Ok(ToolResult::error(format!("Failed to run command: {}", e))),
            Err(_) => Ok(ToolResult::error(format!("Command timed out after {}ms", timeout_ms))),
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

    fn setup_with_sandbox() -> (TempDir, ToolContext) {
        let temp = TempDir::new().unwrap();
        let ctx = super::super::ToolContext::with_sandbox(temp.path().to_path_buf(), true);
        (temp, ctx)
    }

    #[tokio::test]
    async fn test_run_command() {
        let (_temp, ctx) = setup();

        let result = BashTool::execute(&ctx, &serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.output.contains("Exit code: 0"));
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn test_run_command_failure() {
        let (_temp, ctx) = setup();

        let result = BashTool::execute(&ctx, &serde_json::json!({"command": "exit 1"}))
            .await
            .unwrap();
        assert!(result.output.contains("Exit code: 1"));
    }

    #[tokio::test]
    async fn test_run_command_timeout() {
        let (_temp, ctx) = setup();

        let result = BashTool::execute(
            &ctx,
            &serde_json::json!({
                "command": "sleep 10",
                "timeout_ms": 100
            }),
        )
        .await
        .unwrap();
        assert!(result.output.contains("timed out"));
    }

    #[tokio::test]
    #[ignore] // Requires bwrap to be installed
    async fn test_run_command_sandboxed() {
        let (_temp, ctx) = setup_with_sandbox();

        let result = BashTool::execute(&ctx, &serde_json::json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.output.contains("Exit code: 0"));
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    #[ignore] // Requires bwrap to be installed
    async fn test_sandboxed_cannot_access_home_ssh() {
        let (_temp, ctx) = setup_with_sandbox();

        // Try to access ~/.ssh - should fail in sandbox
        let result = BashTool::execute(&ctx, &serde_json::json!({"command": "ls ~/.ssh"}))
            .await
            .unwrap();
        // Should fail because ~/.ssh is not mounted in the sandbox
        assert!(
            result.output.contains("Exit code: 1") || result.output.contains("Exit code: 2"),
            "Expected failure accessing ~/.ssh in sandbox, got: {}",
            result.output
        );
    }
}
