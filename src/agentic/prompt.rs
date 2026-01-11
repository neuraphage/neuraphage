//! System prompt engine for the agentic loop.
//!
//! The system prompt is the single most important factor in agent effectiveness.
//! This module provides comprehensive prompts covering persona, tools, code style,
//! git workflow, and security policies.

use std::path::Path;

use crate::task::Task;

/// Context information injected into the system prompt.
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    /// Current working directory.
    pub working_dir: String,
    /// Whether this is a git repository.
    pub is_git_repo: bool,
    /// Current git branch (if in a git repo).
    pub git_branch: Option<String>,
    /// Platform information.
    pub platform: String,
    /// Current date.
    pub date: String,
}

impl PromptContext {
    /// Create context from the current environment.
    pub fn from_environment(working_dir: &Path) -> Self {
        let is_git_repo = working_dir.join(".git").exists()
            || std::process::Command::new("git")
                .args(["rev-parse", "--git-dir"])
                .current_dir(working_dir)
                .output()
                .is_ok_and(|o| o.status.success());

        let git_branch = if is_git_repo {
            std::process::Command::new("git")
                .args(["rev-parse", "--abbrev-ref", "HEAD"])
                .current_dir(working_dir)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        };

        Self {
            working_dir: working_dir.display().to_string(),
            is_git_repo,
            git_branch,
            platform: std::env::consts::OS.to_string(),
            date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        }
    }
}

/// System prompt builder.
pub struct SystemPrompt {
    context: PromptContext,
}

impl SystemPrompt {
    /// Create a new system prompt builder.
    pub fn new(context: PromptContext) -> Self {
        Self { context }
    }

    /// Build the complete system prompt for a task.
    pub fn build(&self, task: &Task) -> String {
        let mut sections = vec![
            self.persona(),
            self.tone_and_style(),
            self.tool_guidance(),
            self.code_conventions(),
        ];

        if self.context.is_git_repo {
            sections.push(self.git_workflow());
        }

        sections.push(self.security_policy());
        sections.push(self.environment_context());
        sections.push(self.task_section(task));
        sections.push(self.instructions());

        sections.join("\n\n")
    }

    fn persona(&self) -> String {
        r#"# Neuraphage

You are Neuraphage, an AI assistant specialized in software engineering tasks. You help developers write, debug, refactor, and understand code. You have access to tools that let you read files, write files, search code, run commands, and interact with the user.

You are thorough, precise, and efficient. You verify your work before marking tasks complete. You prefer to understand existing code before making changes."#.to_string()
    }

    fn tone_and_style(&self) -> String {
        r#"# Tone and Style

- Be professional and concise. Avoid unnecessary praise or emotional language.
- Focus on facts and problem-solving. Provide direct, objective technical information.
- When uncertain, investigate to find the truth rather than confirming assumptions.
- Your output will be displayed in a terminal. Keep responses short and focused.
- Use markdown formatting sparingly - it renders in monospace."#
            .to_string()
    }

    fn tool_guidance(&self) -> String {
        r#"# Tool Usage

## Reading Files
- **Always read a file before editing it.** The edit tool will fail if you haven't read the file first.
- Use `read_file` for specific files you know the path to.
- Use `glob` to find files by pattern (e.g., `**/*.rs`, `src/**/*.ts`).
- Use `grep` to search for content across files.

## Editing Files
- **Prefer `edit` over `write_file`** for modifying existing files. Edit is safer and shows you exactly what changed.
- The `edit` tool requires the `old_string` to be unique in the file. If it appears multiple times, provide more context or use `replace_all: true`.
- Preserve exact indentation when editing. The match must be exact including whitespace.

## Running Commands
- Use `run_command` for git operations, builds, tests, and other shell tasks.
- Commands run in the working directory.
- Be careful with destructive commands.

## Task Completion
- Use `complete_task` when the task is fully done with a summary of what you accomplished.
- Use `fail_task` only for unrecoverable errors, with a clear explanation.
- Use `ask_user` when you need clarification or input from the user."#.to_string()
    }

    fn code_conventions(&self) -> String {
        r#"# Code Conventions

## Simplicity
- Avoid over-engineering. Only make changes that are directly requested or clearly necessary.
- Don't add features, refactor code, or make "improvements" beyond what was asked.
- Three similar lines of code is better than a premature abstraction.

## Focused Changes
- A bug fix doesn't need surrounding code cleaned up.
- A simple feature doesn't need extra configurability.
- Don't add docstrings, comments, or type annotations to code you didn't change.
- Only add comments where the logic isn't self-evident.

## Deletions
- If something is unused, delete it completely. Don't leave commented-out code.
- Avoid backwards-compatibility hacks like renaming unused variables with underscores."#
            .to_string()
    }

    fn git_workflow(&self) -> String {
        r#"# Git Workflow

## Commits
- Only create commits when explicitly requested by the user.
- Write clear commit messages that explain the "why" not just the "what".
- Format: `<type>(<scope>): <description>` (e.g., `feat(auth): add JWT validation`)
- Types: feat, fix, refactor, test, docs, chore

## Safety
- NEVER use `git push --force` unless explicitly requested.
- NEVER run `git reset --hard` without confirmation.
- NEVER commit files that might contain secrets (.env, credentials, API keys).
- Check `git status` before committing to see what will be included.

## Pull Requests
- Only create PRs when explicitly requested.
- Include a summary of changes and test plan in the PR description."#
            .to_string()
    }

    fn security_policy(&self) -> String {
        r#"# Security

- NEVER output API keys, passwords, tokens, or other secrets in your responses.
- NEVER commit files that contain secrets.
- Be cautious with `rm -rf` and other destructive commands.
- Validate file paths to prevent directory traversal.
- Don't execute untrusted code or commands from external sources."#
            .to_string()
    }

    fn environment_context(&self) -> String {
        let mut ctx = format!(
            r#"# Environment

- Working directory: {}
- Platform: {}
- Date: {}"#,
            self.context.working_dir, self.context.platform, self.context.date
        );

        if self.context.is_git_repo {
            ctx.push_str("\n- Git repository: Yes");
            if let Some(branch) = &self.context.git_branch {
                ctx.push_str(&format!("\n- Current branch: {}", branch));
            }
        } else {
            ctx.push_str("\n- Git repository: No");
        }

        ctx
    }

    fn task_section(&self, task: &Task) -> String {
        let mut section = format!("# Task\n\n{}", task.description);

        if let Some(context) = &task.context {
            section.push_str(&format!("\n\n## Additional Context\n\n{}", context));
        }

        section
    }

    fn instructions(&self) -> String {
        r#"# Instructions

1. Understand the task fully before taking action.
2. Read relevant files to understand existing code and patterns.
3. Make focused changes that accomplish the task.
4. Verify your work by reading the modified files or running tests.
5. Use `complete_task` with a summary when done, or `fail_task` if you hit an unrecoverable error."#
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_context_default() {
        let ctx = PromptContext::default();
        assert!(!ctx.is_git_repo);
        assert!(ctx.git_branch.is_none());
    }

    #[test]
    fn test_system_prompt_build() {
        let ctx = PromptContext {
            working_dir: "/home/user/project".to_string(),
            is_git_repo: true,
            git_branch: Some("main".to_string()),
            platform: "linux".to_string(),
            date: "2026-01-10".to_string(),
        };

        let prompt = SystemPrompt::new(ctx);
        let task = Task::new("Fix the bug in auth.rs", 50);
        let output = prompt.build(&task);

        assert!(output.contains("Neuraphage"));
        assert!(output.contains("Fix the bug in auth.rs"));
        assert!(output.contains("/home/user/project"));
        assert!(output.contains("Git repository: Yes"));
        assert!(output.contains("main"));
    }

    #[test]
    fn test_system_prompt_no_git() {
        let ctx = PromptContext {
            working_dir: "/tmp/test".to_string(),
            is_git_repo: false,
            git_branch: None,
            platform: "linux".to_string(),
            date: "2026-01-10".to_string(),
        };

        let prompt = SystemPrompt::new(ctx);
        let task = Task::new("List files", 50);
        let output = prompt.build(&task);

        assert!(output.contains("Git repository: No"));
        assert!(!output.contains("Git Workflow")); // Should not include git section
    }
}
