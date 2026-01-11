//! REPL mode for interactive conversation.
//!
//! Enables persistent conversation sessions with the agent.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use chrono::{DateTime, Utc};
use colored::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::daemon::{DaemonClient, DaemonRequest, DaemonResponse, ExecutionEventDto, ExecutionStatusDto};
use crate::error::Result;
use crate::repl_display::{Activity, ReplDisplay, StatusState};
use crate::task::TaskId;

/// A persistent session for REPL mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session ID.
    pub id: String,
    /// When the session started.
    pub started_at: DateTime<Utc>,
    /// Working directory.
    pub working_dir: PathBuf,
    /// Path to conversation storage.
    pub conversation_path: PathBuf,
    /// Current task ID (if any).
    pub task_id: Option<TaskId>,
}

impl Session {
    /// Create a new session.
    pub fn new(working_dir: PathBuf) -> Self {
        let id = Uuid::now_v7().to_string();
        let sessions_dir = Self::sessions_dir();
        let conversation_path = sessions_dir.join(&id);

        Self {
            id,
            started_at: Utc::now(),
            working_dir,
            conversation_path,
            task_id: None,
        }
    }

    /// Get the sessions directory.
    fn sessions_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("neuraphage")
            .join("sessions")
    }

    /// Save the session to disk.
    pub fn save(&self) -> Result<()> {
        let sessions_dir = Self::sessions_dir();
        std::fs::create_dir_all(&sessions_dir)?;
        let path = sessions_dir.join(format!("{}.json", self.id));
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Load the most recent session for a working directory.
    pub fn load_recent(working_dir: &PathBuf) -> Option<Self> {
        let sessions_dir = Self::sessions_dir();
        if !sessions_dir.exists() {
            return None;
        }

        let mut sessions: Vec<Session> = std::fs::read_dir(&sessions_dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|e| {
                let content = std::fs::read_to_string(e.path()).ok()?;
                serde_json::from_str(&content).ok()
            })
            .filter(|s: &Session| s.working_dir == *working_dir)
            .collect();

        sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        sessions.into_iter().next()
    }
}

/// Parsed REPL input.
pub enum ReplInput {
    /// Regular message to send.
    Message(String),
    /// Slash command.
    Command { name: String, args: Vec<String> },
    /// End of input.
    Eof,
    /// Empty input.
    Empty,
}

impl ReplInput {
    /// Parse input string.
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Self::Empty;
        }
        if let Some(cmd) = trimmed.strip_prefix('/') {
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.is_empty() {
                return Self::Empty;
            }
            Self::Command {
                name: parts[0].to_string(),
                args: parts[1..].iter().map(|s| s.to_string()).collect(),
            }
        } else {
            Self::Message(trimmed.to_string())
        }
    }
}

/// REPL state and operations.
pub struct Repl {
    client: DaemonClient,
    session: Session,
}

impl Repl {
    /// Create a new REPL.
    pub fn new(client: DaemonClient, working_dir: PathBuf) -> Self {
        let session = Session::load_recent(&working_dir).unwrap_or_else(|| Session::new(working_dir));
        Self { client, session }
    }

    /// Run the REPL loop.
    pub async fn run(&mut self) -> Result<()> {
        self.print_welcome();

        loop {
            // Read input
            print!("{} ", ">".cyan());
            std::io::stdout().flush().ok();

            let stdin = std::io::stdin();
            let mut input = String::new();
            match stdin.lock().read_line(&mut input) {
                Ok(0) => break, // EOF
                Ok(_) => {}
                Err(_) => break,
            }

            // Parse and handle input
            match ReplInput::parse(&input) {
                ReplInput::Eof => break,
                ReplInput::Empty => continue,
                ReplInput::Command { name, args } => {
                    if !self.handle_command(&name, &args).await? {
                        break;
                    }
                }
                ReplInput::Message(msg) => {
                    self.send_message(&msg).await?;
                }
            }
        }

        // Save session on exit
        self.session.save().ok();
        println!("\n{} Session saved", "✓".green());

        Ok(())
    }

    fn print_welcome(&self) {
        println!();
        println!("{}", "Neuraphage REPL".cyan().bold());
        println!("Type a message to start, or {} for help", "/help".yellow());
        println!();
    }

    /// Handle a slash command. Returns false if REPL should exit.
    async fn handle_command(&mut self, name: &str, args: &[String]) -> Result<bool> {
        match name {
            "quit" | "exit" | "q" => {
                return Ok(false);
            }
            "help" | "h" | "?" => {
                self.show_help();
            }
            "clear" => {
                self.clear_history();
            }
            "status" => {
                self.show_status().await?;
            }
            "commit" => {
                self.commit(args).await?;
            }
            "pr" => {
                self.create_pr(args).await?;
            }
            _ => {
                println!("{} Unknown command: /{}", "!".yellow(), name);
                println!("Type {} for available commands", "/help".yellow());
            }
        }
        Ok(true)
    }

    fn show_help(&self) {
        println!();
        println!("{}", "Available Commands".cyan().bold());
        println!();
        println!("  {}     Show this help", "/help".yellow());
        println!("  {}    Clear conversation history", "/clear".yellow());
        println!("  {}   Show current status", "/status".yellow());
        println!("  {}   Create a git commit", "/commit".yellow());
        println!("  {}       Create a pull request", "/pr".yellow());
        println!("  {}     Exit the REPL", "/quit".yellow());
        println!();
    }

    fn clear_history(&mut self) {
        self.session.task_id = None;
        println!("{} Conversation cleared", "✓".green());
    }

    async fn show_status(&mut self) -> Result<()> {
        let response = self.client.request(DaemonRequest::TaskCounts).await?;
        match response {
            DaemonResponse::Counts {
                queued,
                running,
                waiting,
                ..
            } => {
                println!();
                println!("{} Status", "→".blue());
                println!("  Session: {}", self.session.id.dimmed());
                println!("  Working dir: {}", self.session.working_dir.display());
                if let Some(task_id) = &self.session.task_id {
                    println!("  Current task: {}", task_id.0.cyan());
                }
                println!("  Tasks: {} queued, {} running, {} waiting", queued, running, waiting);
                println!();
            }
            DaemonResponse::Error { message } => {
                println!("{} {}", "✗".red(), message);
            }
            _ => {}
        }
        Ok(())
    }

    async fn commit(&mut self, args: &[String]) -> Result<()> {
        let message = if args.is_empty() {
            "Create a git commit for the changes made".to_string()
        } else {
            format!("Create a git commit with message: {}", args.join(" "))
        };
        self.send_message(&message).await
    }

    async fn create_pr(&mut self, args: &[String]) -> Result<()> {
        let message = if args.is_empty() {
            "Create a pull request for the current branch".to_string()
        } else {
            format!("Create a pull request: {}", args.join(" "))
        };
        self.send_message(&message).await
    }

    async fn send_message(&mut self, message: &str) -> Result<()> {
        // Create a new task for this message
        let request = DaemonRequest::CreateTask {
            description: message.to_string(),
            priority: 2,
            tags: vec!["repl".to_string()],
            context: None,
            working_dir: Some(self.session.working_dir.to_string_lossy().to_string()),
        };

        let response = self.client.request(request).await?;

        let task_id = match response {
            DaemonResponse::Task(Some(task)) => task.id.0.clone(),
            DaemonResponse::Error { message } => {
                println!("{} {}", "✗".red(), message);
                return Ok(());
            }
            _ => return Ok(()),
        };

        // Start the task with the working directory
        let request = DaemonRequest::StartTask {
            id: task_id.clone(),
            working_dir: Some(self.session.working_dir.to_string_lossy().to_string()),
        };
        self.client.request(request).await?;

        // Update session
        self.session.task_id = Some(TaskId(task_id.clone()));

        // Wait for completion
        self.wait_for_task(&task_id).await?;

        Ok(())
    }

    /// Handle a single execution event, updating display state.
    fn handle_event(
        event: &ExecutionEventDto,
        display: &mut ReplDisplay,
        status: &mut StatusState,
        should_break: &mut bool,
    ) {
        match event {
            ExecutionEventDto::IterationComplete {
                iteration,
                tokens_used,
                cost,
            } => {
                status.iteration = *iteration;
                status.tokens_used = *tokens_used;
                status.cost = *cost;
                status.activity = Activity::Streaming;
                display.update_status(status.clone()).ok();
            }
            ExecutionEventDto::LlmResponse { content } => {
                display.println("").ok();
                display.println(content).ok();
            }
            ExecutionEventDto::ToolCalled { name, result } => {
                display.println("").ok();
                display.println(&format!("{} {}", "→".blue(), name.cyan())).ok();
                let truncated = if result.len() > 200 {
                    format!("{}...", &result[..200])
                } else {
                    result.clone()
                };
                display.println(&format!("  {}", truncated.dimmed())).ok();
            }
            ExecutionEventDto::Completed { reason } => {
                status.activity = Activity::Idle;
                display.update_status(status.clone()).ok();
                display.println("").ok();
                display.println(&format!("{} {}", "✓".green(), reason)).ok();
                display.print_summary().ok();
                *should_break = true;
            }
            ExecutionEventDto::Failed { error } => {
                status.activity = Activity::Idle;
                display.update_status(status.clone()).ok();
                display.println("").ok();
                display.println(&format!("{} {}", "✗".red(), error)).ok();
                display.print_summary().ok();
                *should_break = true;
            }
            ExecutionEventDto::TextDelta { content } => {
                status.activity = Activity::Streaming;
                display.print_content(content).ok();
            }
            ExecutionEventDto::ActivityChanged { activity } => {
                use crate::daemon::ActivityDto;
                match activity {
                    ActivityDto::ExecutingTool { name } => {
                        status.activity = Activity::ExecutingTool(name.clone());
                        display.update_status(status.clone()).ok();
                        display
                            .println(&format!("{} Running {}...", "⚙".blue(), name.cyan()))
                            .ok();
                    }
                    ActivityDto::WaitingForTool { name } => {
                        status.activity = Activity::WaitingForUser;
                        display.update_status(status.clone()).ok();
                        display
                            .println(&format!("{} Waiting for {}...", "⏳".yellow(), name))
                            .ok();
                    }
                    ActivityDto::Thinking => {
                        status.activity = Activity::Thinking;
                        display.update_status(status.clone()).ok();
                    }
                    ActivityDto::Streaming => {
                        status.activity = Activity::Streaming;
                        display.update_status(status.clone()).ok();
                    }
                    ActivityDto::Idle => {
                        status.activity = Activity::Idle;
                        display.update_status(status.clone()).ok();
                    }
                }
            }
            ExecutionEventDto::ToolStarted { name } => {
                status.activity = Activity::ExecutingTool(name.clone());
                display.update_status(status.clone()).ok();
                display.println(&format!("{} {}...", "⚙".blue(), name.cyan())).ok();
            }
            ExecutionEventDto::ToolCompleted { name, result } => {
                let truncated = if result.len() > 200 {
                    format!("{}...", &result[..200])
                } else {
                    result.clone()
                };
                display.println(&format!("  {} {}", "✓".green(), name)).ok();
                display.println(&format!("  {}", truncated.dimmed())).ok();
            }
            _ => {}
        }
    }

    async fn wait_for_task(&mut self, task_id: &str) -> Result<()> {
        // Create the display - enters raw mode if TTY
        let mut display = ReplDisplay::new()?;
        let mut status = StatusState {
            task_started: Some(Instant::now()),
            ..Default::default()
        };
        let mut should_break = false;

        loop {
            let request = DaemonRequest::AttachTask {
                id: task_id.to_string(),
            };
            let response = self.client.request(request).await?;

            match response {
                DaemonResponse::ExecutionStatusResponse {
                    status: exec_status, ..
                } => {
                    match exec_status {
                        ExecutionStatusDto::Running {
                            iteration,
                            tokens_used,
                            cost,
                        } => {
                            status.iteration = iteration;
                            status.tokens_used = tokens_used;
                            status.cost = cost;
                            display.update_status(status.clone())?;
                        }
                        ExecutionStatusDto::WaitingForUser { prompt } => {
                            // Need to cleanup display to read user input
                            display.cleanup()?;
                            println!();
                            println!("{} {}", "?".yellow(), prompt);
                            print!("{} ", ">".cyan());
                            std::io::stdout().flush().ok();

                            let stdin = std::io::stdin();
                            let mut input = String::new();
                            if stdin.lock().read_line(&mut input).is_ok() {
                                let input = input.trim().to_string();
                                if !input.is_empty() {
                                    let request = DaemonRequest::ProvideInput {
                                        id: task_id.to_string(),
                                        input,
                                    };
                                    self.client.request(request).await?;
                                }
                            }
                            // Re-create display for continued execution
                            display = ReplDisplay::new()?;
                        }
                        ExecutionStatusDto::Completed { reason } => {
                            display.cleanup()?;
                            println!();
                            println!("{} {}", "✓".green(), reason);
                            break;
                        }
                        ExecutionStatusDto::Failed { error } => {
                            display.cleanup()?;
                            println!();
                            println!("{} {}", "✗".red(), error);
                            break;
                        }
                        ExecutionStatusDto::Cancelled => {
                            display.cleanup()?;
                            println!();
                            println!("{} Task cancelled", "⊘".yellow());
                            break;
                        }
                        ExecutionStatusDto::NotRunning => {
                            display.cleanup()?;
                            break;
                        }
                    }
                }
                DaemonResponse::ExecutionUpdate { event, .. } => {
                    Self::handle_event(&event, &mut display, &mut status, &mut should_break);
                    if should_break {
                        display.cleanup()?;
                        break;
                    }
                }
                DaemonResponse::ExecutionEvents { events, .. } => {
                    // Handle multiple events from the buffer
                    for event in events {
                        Self::handle_event(&event, &mut display, &mut status, &mut should_break);
                        if should_break {
                            break;
                        }
                    }
                    if should_break {
                        display.cleanup()?;
                        break;
                    }
                }
                DaemonResponse::Error { message } => {
                    display.cleanup()?;
                    println!("{} {}", "✗".red(), message);
                    break;
                }
                _ => {}
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        println!();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_new() {
        let session = Session::new(PathBuf::from("/tmp/test"));
        assert!(!session.id.is_empty());
        assert_eq!(session.working_dir, PathBuf::from("/tmp/test"));
        assert!(session.task_id.is_none());
    }

    #[test]
    fn test_repl_input_parse_message() {
        let input = ReplInput::parse("hello world");
        match input {
            ReplInput::Message(msg) => assert_eq!(msg, "hello world"),
            _ => panic!("Expected message"),
        }
    }

    #[test]
    fn test_repl_input_parse_command() {
        let input = ReplInput::parse("/commit -m 'test'");
        match input {
            ReplInput::Command { name, args } => {
                assert_eq!(name, "commit");
                assert_eq!(args, vec!["-m", "'test'"]);
            }
            _ => panic!("Expected command"),
        }
    }

    #[test]
    fn test_repl_input_parse_empty() {
        let input = ReplInput::parse("   ");
        assert!(matches!(input, ReplInput::Empty));
    }

    #[test]
    fn test_repl_input_parse_slash_only() {
        let input = ReplInput::parse("/");
        assert!(matches!(input, ReplInput::Empty));
    }
}
