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
}
