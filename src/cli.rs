//! CLI argument parsing for Neuraphage.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "neuraphage",
    about = "Multi-task AI orchestrator daemon",
    version = env!("GIT_DESCRIBE"),
    after_help = "Logs are written to: ~/.local/share/neuraphage/logs/neuraphage.log"
)]
pub struct Cli {
    /// Path to config file
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the daemon
    Daemon {
        /// Run in foreground (don't daemonize)
        #[arg(short, long)]
        foreground: bool,
    },

    /// Create a new task
    New {
        /// Task description
        description: String,

        /// Priority (0=critical, 4=low)
        #[arg(short, long, default_value = "2")]
        priority: u8,

        /// Tags for the task
        #[arg(short, long)]
        tags: Vec<String>,

        /// Extended context
        #[arg(short, long)]
        context: Option<String>,
    },

    /// List tasks
    #[command(alias = "ls")]
    List {
        /// Filter by status
        #[arg(short, long)]
        status: Option<String>,

        /// Show all tasks (including closed)
        #[arg(short, long)]
        all: bool,
    },

    /// Show task details
    Show {
        /// Task ID
        id: String,
    },

    /// Show ready tasks (tasks with no blockers)
    Ready,

    /// Show blocked tasks
    Blocked,

    /// Set task status
    Status {
        /// Task ID
        id: String,

        /// New status (queued, running, waiting, blocked, paused, completed, failed, cancelled)
        status: String,
    },

    /// Close a task
    Close {
        /// Task ID
        id: String,

        /// Close status (completed, failed, cancelled)
        #[arg(short, long, default_value = "completed")]
        status: String,

        /// Close reason
        #[arg(short, long)]
        reason: Option<String>,
    },

    /// Add a dependency between tasks
    Depend {
        /// Task that will be blocked
        blocked: String,

        /// Task that blocks
        blocker: String,
    },

    /// Remove a dependency between tasks
    Undepend {
        /// Task that was blocked
        blocked: String,

        /// Task that was blocking
        blocker: String,
    },

    /// Show task statistics
    Stats,

    /// Stop the daemon
    Stop,

    /// Check daemon status
    Ping,

    /// Run a task interactively (create, start, and attach)
    Run {
        /// Task description
        description: String,

        /// Working directory
        #[arg(short = 'd', long)]
        dir: Option<PathBuf>,

        /// Priority (0=critical, 4=low)
        #[arg(short, long, default_value = "2")]
        priority: u8,

        /// Tags for the task
        #[arg(short, long)]
        tags: Vec<String>,

        /// Extended context
        #[arg(short, long)]
        context: Option<String>,
    },

    /// Attach to a running task
    Attach {
        /// Task ID
        id: String,
    },

    /// Start a task
    Start {
        /// Task ID
        id: String,
    },

    /// Cancel a running task
    Cancel {
        /// Task ID
        id: String,
    },
}
