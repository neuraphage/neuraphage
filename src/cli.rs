//! CLI argument parsing for Neuraphage.

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::LazyLock;

/// Generate the after-help text with tool versions and daemon status.
fn generate_after_help() -> String {
    let mut lines = Vec::new();

    // Required tools section (bold)
    lines.push("\x1b[1mRequired Tools:\x1b[0m".to_string());

    // Check bwrap
    let bwrap_status = match std::process::Command::new("bwrap").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim().replace("bubblewrap ", "");
            format!("  ✅ bwrap      {}", version)
        }
        _ => "  ❌ bwrap      not installed".to_string(),
    };
    lines.push(bwrap_status);

    // Check git
    let git_status = match std::process::Command::new("git").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim().replace("git version ", "");
            format!("  ✅ git        {}", version)
        }
        _ => "  ❌ git        not installed".to_string(),
    };
    lines.push(git_status);

    lines.push(String::new());

    // Daemon section (bold)
    let daemon_status = check_daemon_status();
    lines.push(format!(
        "\x1b[1mDaemon:\x1b[0m\n  {} {}",
        daemon_status.0, daemon_status.1
    ));

    lines.push(String::new());
    lines.push("Logs are written to: ~/.local/share/neuraphage/logs/neuraphage.log".to_string());

    lines.join("\n")
}

/// Check if daemon is running and return (icon, status_text).
fn check_daemon_status() -> (&'static str, &'static str) {
    let socket_path = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("neuraphage")
        .join("neuraphage.sock");

    if socket_path.exists() {
        // Try to connect briefly
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            ("✅", "running")
        } else {
            ("❌", "stale socket")
        }
    } else {
        ("❌", "not running")
    }
}

static AFTER_HELP: LazyLock<String> = LazyLock::new(generate_after_help);

#[derive(Parser)]
#[command(
    name = "neuraphage",
    about = "Multi-task AI orchestrator daemon",
    version = env!("GIT_DESCRIBE"),
    after_help = AFTER_HELP.as_str()
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

/// Daemon lifecycle commands.
#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon
    Start {
        /// Run in foreground (don't daemonize)
        #[arg(short, long)]
        foreground: bool,

        /// Restart: stop daemon first if already running
        #[arg(short, long)]
        restart: bool,
    },

    /// Stop the daemon
    Stop,

    /// Check daemon status
    Status,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage the neuraphage daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),

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
        #[arg(short = 'x', long)]
        context: Option<String>,

        /// Git repository path (creates worktree for task isolation)
        #[arg(short, long)]
        repo: Option<PathBuf>,
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
        #[arg(short = 'x', long)]
        context: Option<String>,

        /// Git repository path (creates worktree for task isolation)
        #[arg(short, long)]
        repo: Option<PathBuf>,
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

    /// Show recovery report (tasks that can be resumed after crash)
    Recover,

    /// Resume a task from its last checkpoint
    Resume {
        /// Task ID
        id: String,
    },

    /// Force checkpoint a running task
    Checkpoint {
        /// Task ID
        id: String,
    },

    /// Manage git worktrees
    #[command(subcommand)]
    Worktree(WorktreeCommand),

    /// Manage merge queue
    #[command(subcommand)]
    Merge(MergeCommand),

    /// View cost statistics and budget usage
    #[command(subcommand)]
    Cost(CostCommand),

    /// Manage proactive rebasing against main
    #[command(subcommand)]
    Rebase(RebaseCommand),
}

/// Worktree management commands.
#[derive(Subcommand)]
pub enum WorktreeCommand {
    /// List all active worktrees
    List,

    /// Show worktree info for a task
    Info {
        /// Task ID
        id: String,
    },
}

/// Merge queue commands.
#[derive(Subcommand)]
pub enum MergeCommand {
    /// Show merge queue status
    Queue,

    /// Enqueue a task's branch for merging
    Enqueue {
        /// Task ID
        id: String,

        /// Target branch (default: main)
        #[arg(short, long)]
        target: Option<String>,
    },
}

/// Cost tracking commands.
#[derive(Subcommand)]
pub enum CostCommand {
    /// Show current budget usage
    Status,

    /// Show cost report for date range
    Report {
        /// Start date (YYYY-MM-DD), defaults to start of month
        #[arg(short, long)]
        from: Option<String>,

        /// End date (YYYY-MM-DD), defaults to today
        #[arg(short, long)]
        to: Option<String>,

        /// Group by: day, task, model
        #[arg(short, long, default_value = "day")]
        group_by: String,
    },

    /// Show cost for a specific task
    Task {
        /// Task ID
        id: String,
    },
}

/// Rebase commands for proactive main-tracking.
#[derive(Subcommand)]
pub enum RebaseCommand {
    /// Show rebase status for all tasks
    Status,

    /// Trigger rebase for a specific task
    Trigger {
        /// Task ID
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_daemon_start_default() {
        let cli = Cli::parse_from(["np", "daemon", "start"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start {
                foreground: false,
                restart: false
            }))
        ));
    }

    #[test]
    fn test_daemon_start_foreground() {
        let cli = Cli::parse_from(["np", "daemon", "start", "-f"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start {
                foreground: true,
                restart: false
            }))
        ));
    }

    #[test]
    fn test_daemon_start_restart() {
        let cli = Cli::parse_from(["np", "daemon", "start", "-r"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start {
                foreground: false,
                restart: true
            }))
        ));
    }

    #[test]
    fn test_daemon_start_foreground_restart() {
        let cli = Cli::parse_from(["np", "daemon", "start", "-f", "-r"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start {
                foreground: true,
                restart: true
            }))
        ));
    }

    #[test]
    fn test_daemon_stop() {
        let cli = Cli::parse_from(["np", "daemon", "stop"]);
        assert!(matches!(cli.command, Some(Command::Daemon(DaemonCommand::Stop))));
    }

    #[test]
    fn test_daemon_status() {
        let cli = Cli::parse_from(["np", "daemon", "status"]);
        assert!(matches!(cli.command, Some(Command::Daemon(DaemonCommand::Status))));
    }

    #[test]
    fn test_new_with_repo() {
        let cli = Cli::parse_from(["np", "new", "test task", "--repo", "/tmp/repo"]);
        if let Some(Command::New { repo, .. }) = cli.command {
            assert_eq!(repo, Some(PathBuf::from("/tmp/repo")));
        } else {
            panic!("Expected New command");
        }
    }

    #[test]
    fn test_worktree_list() {
        let cli = Cli::parse_from(["np", "worktree", "list"]);
        assert!(matches!(cli.command, Some(Command::Worktree(WorktreeCommand::List))));
    }

    #[test]
    fn test_worktree_info() {
        let cli = Cli::parse_from(["np", "worktree", "info", "task-123"]);
        if let Some(Command::Worktree(WorktreeCommand::Info { id })) = cli.command {
            assert_eq!(id, "task-123");
        } else {
            panic!("Expected Worktree Info command");
        }
    }

    #[test]
    fn test_merge_queue() {
        let cli = Cli::parse_from(["np", "merge", "queue"]);
        assert!(matches!(cli.command, Some(Command::Merge(MergeCommand::Queue))));
    }

    #[test]
    fn test_merge_enqueue() {
        let cli = Cli::parse_from(["np", "merge", "enqueue", "task-123", "--target", "develop"]);
        if let Some(Command::Merge(MergeCommand::Enqueue { id, target })) = cli.command {
            assert_eq!(id, "task-123");
            assert_eq!(target, Some("develop".to_string()));
        } else {
            panic!("Expected Merge Enqueue command");
        }
    }

    #[test]
    fn test_cost_status() {
        let cli = Cli::parse_from(["np", "cost", "status"]);
        assert!(matches!(cli.command, Some(Command::Cost(CostCommand::Status))));
    }

    #[test]
    fn test_cost_report() {
        let cli = Cli::parse_from(["np", "cost", "report"]);
        if let Some(Command::Cost(CostCommand::Report { from, to, group_by })) = cli.command {
            assert!(from.is_none());
            assert!(to.is_none());
            assert_eq!(group_by, "day");
        } else {
            panic!("Expected Cost Report command");
        }
    }

    #[test]
    fn test_cost_report_with_dates() {
        let cli = Cli::parse_from([
            "np",
            "cost",
            "report",
            "--from",
            "2024-01-01",
            "--to",
            "2024-01-31",
            "--group-by",
            "task",
        ]);
        if let Some(Command::Cost(CostCommand::Report { from, to, group_by })) = cli.command {
            assert_eq!(from, Some("2024-01-01".to_string()));
            assert_eq!(to, Some("2024-01-31".to_string()));
            assert_eq!(group_by, "task");
        } else {
            panic!("Expected Cost Report command");
        }
    }

    #[test]
    fn test_cost_task() {
        let cli = Cli::parse_from(["np", "cost", "task", "task-123"]);
        if let Some(Command::Cost(CostCommand::Task { id })) = cli.command {
            assert_eq!(id, "task-123");
        } else {
            panic!("Expected Cost Task command");
        }
    }

    #[test]
    fn test_rebase_status() {
        let cli = Cli::parse_from(["np", "rebase", "status"]);
        assert!(matches!(cli.command, Some(Command::Rebase(RebaseCommand::Status))));
    }

    #[test]
    fn test_rebase_trigger() {
        let cli = Cli::parse_from(["np", "rebase", "trigger", "task-123"]);
        if let Some(Command::Rebase(RebaseCommand::Trigger { id })) = cli.command {
            assert_eq!(id, "task-123");
        } else {
            panic!("Expected Rebase Trigger command");
        }
    }
}
