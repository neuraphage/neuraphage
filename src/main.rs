//! Neuraphage CLI entry point.

use clap::Parser;
use colored::*;
use eyre::{Context, Result};
use fork::{Fork, daemon};
use log::info;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

mod cli;

use cli::{Cli, Command, CostCommand, DaemonCommand, MergeCommand, RebaseCommand, WorktreeCommand};
use neuraphage::config::Config;
use neuraphage::daemon::{
    ActivityDto, Daemon, DaemonClient, DaemonRequest, DaemonResponse, ExecutionEventDto, ExecutionStatusDto,
    is_daemon_running,
};
use neuraphage::repl::Repl;

fn setup_logging() -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("neuraphage")
        .join("logs");

    fs::create_dir_all(&log_dir).context("Failed to create log directory")?;

    let log_file = log_dir.join("neuraphage.log");

    let target = Box::new(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_file)
            .context("Failed to open log file")?,
    );

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

fn main() -> Result<()> {
    // Parse CLI args first (before any async runtime)
    let cli = Cli::parse();

    // CRITICAL: Background daemon start must happen BEFORE tokio runtime.
    // The tokio runtime cannot survive a fork.
    if let Some(Command::Daemon(DaemonCommand::Start {
        foreground: false,
        restart,
    })) = &cli.command
    {
        let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;
        let daemon_config = config.to_daemon_config();

        if is_daemon_running(&daemon_config) {
            if *restart {
                // Stop the running daemon first (need a brief tokio runtime for this)
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    if let Ok(mut client) = DaemonClient::connect(&daemon_config).await {
                        client.request(DaemonRequest::Shutdown).await.ok();
                    }
                });
                // Wait for daemon to stop
                std::thread::sleep(std::time::Duration::from_millis(500));

                // Check if it actually stopped
                if is_daemon_running(&daemon_config) {
                    eprintln!("{} Daemon did not stop in time, aborting", "!".red());
                    return Ok(());
                }
            } else {
                eprintln!("{} Daemon is already running (use -r to restart)", "!".yellow());
                return Ok(());
            }
        }

        return daemonize(&daemon_config);
    }

    // For all other commands, run with tokio
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    setup_logging().context("Failed to setup logging")?;

    let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;

    info!("Starting with config from: {:?}", cli.config);

    match cli.command {
        Some(Command::Daemon(daemon_cmd)) => handle_daemon_command(&config, daemon_cmd).await,
        Some(cmd) => run_client_command(&config, cmd).await,
        None => run_repl(&config).await,
    }
}

/// Handle daemon lifecycle commands.
async fn handle_daemon_command(config: &Config, cmd: DaemonCommand) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    match cmd {
        DaemonCommand::Start {
            foreground: true,
            restart,
        } => {
            // Foreground mode - runs in current process
            if is_daemon_running(&daemon_config) {
                if restart {
                    // Stop the running daemon first
                    let mut client = DaemonClient::connect(&daemon_config).await?;
                    client.request(DaemonRequest::Shutdown).await?;
                    // Wait for daemon to stop
                    for _ in 0..20 {
                        if !is_daemon_running(&daemon_config) {
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                    if is_daemon_running(&daemon_config) {
                        eprintln!("{} Daemon did not stop in time", "!".red());
                        return Ok(());
                    }
                } else {
                    eprintln!("{} Daemon is already running (use -r to restart)", "!".yellow());
                    return Ok(());
                }
            }
            println!("{} Starting daemon in foreground...", "→".blue());
            let daemon = Daemon::new(daemon_config)?;
            daemon.run().await?;
            Ok(())
        }
        DaemonCommand::Start { foreground: false, .. } => {
            // Should not reach here - handled in main() before tokio
            unreachable!("Background daemon start handled before tokio runtime")
        }
        DaemonCommand::Stop => {
            if !is_daemon_running(&daemon_config) {
                eprintln!("{} Daemon is not running", "!".yellow());
                return Ok(());
            }
            let mut client = DaemonClient::connect(&daemon_config).await?;
            let response = client.request(DaemonRequest::Shutdown).await?;
            match response {
                DaemonResponse::Shutdown => println!("{} Daemon stopped", "✓".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
            Ok(())
        }
        DaemonCommand::Status => {
            if daemon_config.socket_path.exists() {
                match DaemonClient::connect(&daemon_config).await {
                    Ok(mut client) => match client.request(DaemonRequest::Ping).await {
                        Ok(DaemonResponse::Pong) => {
                            println!("{} Daemon is running", "✓".green());
                        }
                        _ => {
                            println!("{} Daemon not responding", "!".yellow());
                        }
                    },
                    Err(_) => {
                        println!("{} Stale socket (daemon not running)", "!".yellow());
                        println!("  Socket: {}", daemon_config.socket_path.display());
                    }
                }
            } else {
                println!("{} Daemon is not running", "○".yellow());
            }
            Ok(())
        }
    }
}

fn daemonize(daemon_config: &neuraphage::DaemonConfig) -> Result<()> {
    // Use the fork crate for proper double-fork daemonization.
    // This is the recommended approach per tokio maintainers:
    // "You must create the Tokio runtime after daemonizing it.
    //  The Tokio runtime can't survive a fork."
    // See: https://users.rust-lang.org/t/tokio-0-2-and-daemonize-process/42427
    //
    // The fork crate's daemon() function:
    // - Performs double-fork (parent -> child -> grandchild)
    // - Calls setsid() to create new session
    // - Changes working directory to / (when nochdir=false)
    // - Redirects stdio to /dev/null (when noclose=false)
    // - Uses _exit to avoid running destructors post-fork (POSIX-safe)

    match daemon(false, false) {
        Ok(Fork::Child) => {
            // We are now the daemon process (grandchild after double-fork)
            // Safe to create tokio runtime here

            // Write PID file
            let pid = std::process::id();
            let pid_file = daemon_config.socket_path.with_extension("pid");
            if let Some(parent) = pid_file.parent() {
                fs::create_dir_all(parent).ok();
            }
            if let Ok(mut f) = fs::File::create(&pid_file) {
                writeln!(f, "{}", pid).ok();
            }

            // Setup logging for daemon
            setup_logging().ok();

            // Create tokio runtime and run the daemon
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let daemon = Daemon::new(daemon_config.clone())?;
                daemon.run().await?;
                Ok::<(), eyre::Error>(())
            })?;

            Ok(())
        }
        Ok(Fork::Parent(_)) => {
            // Parent process - daemon forked successfully
            println!("{} Daemon started in background", "✓".green());
            std::process::exit(0);
        }
        Err(e) => Err(eyre::eyre!("Failed to daemonize: {:?}", e)),
    }
}

/// Handle cost tracking commands.
async fn handle_cost_command(config: &Config, cmd: CostCommand) -> Result<()> {
    use chrono::{Datelike, NaiveDate, Utc};
    use neuraphage::cost::{CostEntry, CostTracker};
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    match cmd {
        CostCommand::Status => {
            let tracker = CostTracker::new(config.budget.clone(), config.model_pricing.clone(), &config.data_dir)?;

            let stats = tracker.get_stats().await;

            println!("{}", "Cost Status".cyan().bold());
            println!();

            // Daily usage
            print!("  Daily:   ${:.2}", stats.daily_used);
            if let Some(limit) = stats.daily_limit {
                let pct = (stats.daily_used / limit) * 100.0;
                let pct_str = format!(" ({:.0}% of ${:.2})", pct, limit);
                if pct > 90.0 {
                    print!("{}", pct_str.red());
                } else if pct > 75.0 {
                    print!("{}", pct_str.yellow());
                } else {
                    print!("{}", pct_str.dimmed());
                }
            } else {
                print!("{}", " (no limit)".dimmed());
            }
            println!();

            // Monthly usage
            print!("  Monthly: ${:.2}", stats.monthly_used);
            if let Some(limit) = stats.monthly_limit {
                let pct = (stats.monthly_used / limit) * 100.0;
                let pct_str = format!(" ({:.0}% of ${:.2})", pct, limit);
                if pct > 90.0 {
                    print!("{}", pct_str.red());
                } else if pct > 75.0 {
                    print!("{}", pct_str.yellow());
                } else {
                    print!("{}", pct_str.dimmed());
                }
            } else {
                print!("{}", " (no limit)".dimmed());
            }
            println!();
        }
        CostCommand::Report { from, to, group_by } => {
            let today = Utc::now().date_naive();

            let to_date = to
                .map(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d"))
                .transpose()
                .context("Invalid 'to' date format (use YYYY-MM-DD)")?
                .unwrap_or(today);

            let from_date = from
                .map(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d"))
                .transpose()
                .context("Invalid 'from' date format (use YYYY-MM-DD)")?
                .unwrap_or_else(|| to_date.with_day(1).unwrap_or(to_date));

            let ledger_path = config.data_dir.join("cost_ledger.jsonl");
            if !ledger_path.exists() {
                println!("{}", "No cost data recorded yet.".dimmed());
                return Ok(());
            }

            let file = File::open(&ledger_path).context("Failed to open cost ledger")?;
            let reader = BufReader::new(file);

            let mut totals: HashMap<String, f64> = HashMap::new();
            let mut grand_total = 0.0;

            for line in reader.lines() {
                let line = line.context("Failed to read ledger line")?;
                let entry: CostEntry = match serde_json::from_str(&line) {
                    Ok(e) => e,
                    Err(_) => continue, // Skip malformed entries
                };
                let entry_date = entry.timestamp.date_naive();

                if entry_date >= from_date && entry_date <= to_date {
                    let key = match group_by.as_str() {
                        "day" => entry_date.to_string(),
                        "task" => entry.task_id.clone(),
                        "model" => entry.model.clone(),
                        _ => "unknown".to_string(),
                    };
                    *totals.entry(key).or_insert(0.0) += entry.cost;
                    grand_total += entry.cost;
                }
            }

            println!("{} {} to {}", "Cost Report:".cyan().bold(), from_date, to_date);
            println!();

            if totals.is_empty() {
                println!("  {}", "No costs in this period.".dimmed());
            } else {
                let mut sorted: Vec<_> = totals.into_iter().collect();
                sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

                for (key, cost) in sorted {
                    println!("  {:30} ${:.4}", key, cost);
                }
                println!();
                println!("  {:30} {}", "Total:".bold(), format!("${:.4}", grand_total).bold());
            }
        }
        CostCommand::Task { id } => {
            let tracker = CostTracker::new(config.budget.clone(), config.model_pricing.clone(), &config.data_dir)?;

            let cost = tracker.get_task_cost(&id).await;

            println!("{} {}", "Task:".cyan(), id);
            println!("  Cost: ${:.4}", cost);

            // Check against per-task limit
            if config.budget.per_task > 0.0 {
                let pct = (cost / config.budget.per_task) * 100.0;
                print!("  Limit: ${:.2}", config.budget.per_task);
                let pct_str = format!(" ({:.0}% used)", pct);
                if pct > 90.0 {
                    println!("{}", pct_str.red());
                } else if pct > 75.0 {
                    println!("{}", pct_str.yellow());
                } else {
                    println!("{}", pct_str.dimmed());
                }
            }
        }
    }

    Ok(())
}

async fn run_client_command(config: &Config, command: Command) -> Result<()> {
    // Handle commands that don't need the daemon first
    if let Command::Cost(cost_cmd) = command {
        return handle_cost_command(config, cost_cmd).await;
    }

    let daemon_config = config.to_daemon_config();

    // Check if daemon is running, start if needed
    if !is_daemon_running(&daemon_config) {
        if config.daemon.auto_start {
            eprintln!("{} Daemon not running. Start with: np daemon", "!".yellow());
            return Ok(());
        } else {
            eprintln!("{} Daemon not running", "!".red());
            return Ok(());
        }
    }

    let mut client = DaemonClient::connect(&daemon_config).await?;

    match command {
        Command::Daemon(_) => unreachable!(),

        Command::New {
            description,
            priority,
            tags,
            context,
            repo,
        } => {
            let request = if let Some(repo_path) = repo {
                DaemonRequest::CreateTaskWithWorktree {
                    description,
                    priority,
                    tags,
                    context,
                    repo_path: repo_path.to_string_lossy().to_string(),
                }
            } else {
                let working_dir = std::env::current_dir().ok().map(|d| d.to_string_lossy().to_string());
                DaemonRequest::CreateTask {
                    description,
                    priority,
                    tags,
                    context,
                    working_dir,
                }
            };
            let response = client.request(request).await?;
            handle_task_response(response);
        }

        Command::List { status, all } => {
            let status = if all { None } else { status.or(Some("open".to_string())) };
            let request = DaemonRequest::ListTasks { status };
            let response = client.request(request).await?;
            handle_tasks_response(response);
        }

        Command::Show { id } => {
            let request = DaemonRequest::GetTask { id };
            let response = client.request(request).await?;
            handle_task_response(response);
        }

        Command::Ready => {
            let request = DaemonRequest::ReadyTasks;
            let response = client.request(request).await?;
            handle_tasks_response(response);
        }

        Command::Blocked => {
            let request = DaemonRequest::BlockedTasks;
            let response = client.request(request).await?;
            handle_tasks_response(response);
        }

        Command::Status { id, status } => {
            let request = DaemonRequest::SetStatus { id, status };
            let response = client.request(request).await?;
            handle_task_response(response);
        }

        Command::Close { id, status, reason } => {
            let request = DaemonRequest::CloseTask { id, status, reason };
            let response = client.request(request).await?;
            handle_task_response(response);
        }

        Command::Depend { blocked, blocker } => {
            let request = DaemonRequest::AddDependency {
                blocked_id: blocked,
                blocker_id: blocker,
            };
            let response = client.request(request).await?;
            match response {
                DaemonResponse::Ok { .. } => println!("{} Dependency added", "✓".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Undepend { blocked, blocker } => {
            let request = DaemonRequest::RemoveDependency {
                blocked_id: blocked,
                blocker_id: blocker,
            };
            let response = client.request(request).await?;
            match response {
                DaemonResponse::Ok { .. } => println!("{} Dependency removed", "✓".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Stats => {
            let request = DaemonRequest::TaskCounts;
            let response = client.request(request).await?;
            handle_counts_response(response);
        }

        Command::Run {
            description,
            dir,
            priority,
            tags,
            context,
            repo,
        } => {
            // Use provided directory or current working directory
            let working_dir = dir
                .map(|d| d.to_string_lossy().to_string())
                .or_else(|| std::env::current_dir().ok().map(|d| d.to_string_lossy().to_string()));

            // Create the task (with worktree if repo specified)
            let request = if let Some(repo_path) = repo {
                DaemonRequest::CreateTaskWithWorktree {
                    description: description.clone(),
                    priority,
                    tags,
                    context,
                    repo_path: repo_path.to_string_lossy().to_string(),
                }
            } else {
                DaemonRequest::CreateTask {
                    description: description.clone(),
                    priority,
                    tags,
                    context,
                    working_dir: working_dir.clone(),
                }
            };
            let response = client.request(request).await?;

            let task_id = match response {
                DaemonResponse::Task(Some(task)) => {
                    println!("{} Created task: {}", "✓".green(), task.id.0.cyan());
                    task.id.0.clone()
                }
                DaemonResponse::Error { message } => {
                    eprintln!("{} Failed to create task: {}", "✗".red(), message);
                    return Ok(());
                }
                _ => {
                    eprintln!("{} Unexpected response", "✗".red());
                    return Ok(());
                }
            };

            // Start the task
            let request = DaemonRequest::StartTask {
                id: task_id.clone(),
                working_dir,
            };
            let response = client.request(request).await?;

            match response {
                DaemonResponse::TaskStarted { task_id: _ } => {
                    println!("{} Task started", "→".blue());
                }
                DaemonResponse::Error { message } => {
                    eprintln!("{} Failed to start task: {}", "✗".red(), message);
                    return Ok(());
                }
                _ => {}
            }

            // Attach to the task
            run_interactive_loop(&mut client, &task_id).await?;
        }

        Command::Attach { id } => {
            run_interactive_loop(&mut client, &id).await?;
        }

        Command::Start { id } => {
            let working_dir = std::env::current_dir().ok().map(|d| d.to_string_lossy().to_string());
            let request = DaemonRequest::StartTask {
                id: id.clone(),
                working_dir,
            };
            let response = client.request(request).await?;
            match response {
                DaemonResponse::TaskStarted { task_id } => {
                    println!("{} Started task: {}", "✓".green(), task_id.cyan());
                }
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Cancel { id } => {
            let request = DaemonRequest::CancelTask { id };
            let response = client.request(request).await?;
            match response {
                DaemonResponse::Ok { message } => {
                    println!(
                        "{} {}",
                        "✓".green(),
                        message.unwrap_or_else(|| "Task cancelled".to_string())
                    );
                }
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Recover => {
            let request = DaemonRequest::GetRecoveryReport;
            let response = client.request(request).await?;
            match response {
                DaemonResponse::RecoveryReport { tasks } => {
                    if tasks.is_empty() {
                        println!("{} No recoverable tasks found", "✓".green());
                    } else {
                        println!(
                            "{} {} recoverable task(s) from previous session:\n",
                            "!".yellow(),
                            tasks.len()
                        );
                        for task in &tasks {
                            println!(
                                "  {} {} (iteration {}, {} tokens, ${})",
                                "•".blue(),
                                task.task_id,
                                task.iteration,
                                task.tokens_used,
                                task.cost
                            );
                            println!("    Phase: {}", task.phase);
                            println!("    Working dir: {}", task.working_dir);
                            println!("    Checkpoint: {}", task.checkpoint_at);
                            println!();
                        }
                        println!("Use {} to resume a task", "np resume <id>".cyan());
                    }
                }
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Resume { id } => {
            let request = DaemonRequest::ResumeTask { id: id.clone() };
            let response = client.request(request).await?;
            match response {
                DaemonResponse::TaskResumed { task_id } => {
                    println!("{} Task {} resumed from checkpoint", "✓".green(), task_id);
                }
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Checkpoint { id } => {
            // Force checkpoint a running task
            // Note: Currently we use the periodic checkpointing. For manual checkpoint,
            // we would need to add a new daemon request. For now, just inform the user.
            println!(
                "{} Task {} will be checkpointed automatically every 30 seconds",
                "i".blue(),
                id
            );
            println!("Manual checkpoint forcing is not yet implemented.");
        }

        Command::Worktree(worktree_cmd) => match worktree_cmd {
            WorktreeCommand::List => {
                let request = DaemonRequest::ListWorktrees;
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::Worktrees(worktrees) => {
                        if worktrees.is_empty() {
                            println!("{} No active worktrees", "○".yellow());
                        } else {
                            println!("{} Active Worktrees", "🌲".green());
                            println!();
                            for wt in worktrees {
                                println!("  {} {}", wt.task_id.cyan(), wt.branch);
                                println!("    Path: {}", wt.path.dimmed());
                            }
                        }
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
            WorktreeCommand::Info { id } => {
                let request = DaemonRequest::GetWorktreeInfo { id };
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::WorktreeInfo(Some(wt)) => {
                        println!("{} Worktree Info", "🌲".green());
                        println!();
                        println!("  Task:    {}", wt.task_id.cyan());
                        println!("  Branch:  {}", wt.branch);
                        println!("  Path:    {}", wt.path);
                        println!("  Repo:    {}", wt.repo_path);
                        println!("  Created: {}", wt.created_at);
                    }
                    DaemonResponse::WorktreeInfo(None) => {
                        println!("{} No worktree for this task", "○".yellow());
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
        },

        Command::Merge(merge_cmd) => match merge_cmd {
            MergeCommand::Queue => {
                let request = DaemonRequest::MergeQueueStatus;
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::MergeQueueStatusResponse { pending, active } => {
                        println!("{} Merge Queue Status", "🔀".blue());
                        println!();
                        println!("  Pending: {}", pending);
                        if let Some(task_id) = active {
                            println!("  Active:  {}", task_id.cyan());
                        } else {
                            println!("  Active:  {}", "none".dimmed());
                        }
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
            MergeCommand::Enqueue { id, target } => {
                let request = DaemonRequest::EnqueueMerge {
                    id,
                    target_branch: target,
                };
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::MergeEnqueued { task_id } => {
                        println!("{} Task {} enqueued for merge", "✓".green(), task_id.cyan());
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
        },
        Command::Cost(_) => unreachable!("Cost commands handled before daemon check"),

        Command::Rebase(rebase_cmd) => match rebase_cmd {
            RebaseCommand::Status => {
                let request = DaemonRequest::GetRebaseStatus;
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::RebaseStatus { tasks } => {
                        if tasks.is_empty() {
                            println!("{} No tasks with rebase status", "○".yellow());
                        } else {
                            println!("{} Rebase Status", "🔄".blue());
                            println!();
                            for task in tasks {
                                let status_icon = if task.needs_rebase { "!".yellow() } else { "✓".green() };
                                println!(
                                    "  {} {} ({}) - {} commits behind",
                                    status_icon, task.task_id.cyan(), task.branch, task.commits_behind
                                );
                            }
                        }
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
            RebaseCommand::Trigger { id } => {
                let request = DaemonRequest::TriggerRebase { id: id.clone() };
                let response = client.request(request).await?;
                match response {
                    DaemonResponse::RebaseResult {
                        task_id,
                        success,
                        message,
                    } => {
                        if success {
                            println!("{} Task {} rebased: {}", "✓".green(), task_id.cyan(), message);
                        } else {
                            println!("{} Task {} rebase failed: {}", "✗".red(), task_id.cyan(), message);
                        }
                    }
                    DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                    _ => {}
                }
            }
        },
    }

    Ok(())
}

/// Run the interactive loop for a task.
async fn run_interactive_loop(client: &mut DaemonClient, task_id: &str) -> Result<()> {
    use std::io::{BufRead, stdin};

    println!(
        "{} Attached to task {}. Press Ctrl+C to detach.",
        "→".blue(),
        task_id.cyan()
    );
    println!();

    // Set up Ctrl+C handler
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .ok();

    let mut mid_line = false; // Track if we're in the middle of streaming output
    loop {
        if !running.load(std::sync::atomic::Ordering::SeqCst) {
            println!();
            println!("{} Detached from task", "○".yellow());
            break;
        }

        // Poll for updates
        let request = DaemonRequest::AttachTask {
            id: task_id.to_string(),
        };
        let response = client.request(request).await?;

        match response {
            DaemonResponse::ExecutionUpdate { event, .. } => {
                handle_execution_event(&event, &mut mid_line);

                // Check if task completed or failed
                match event {
                    ExecutionEventDto::Completed { .. } | ExecutionEventDto::Failed { .. } => {
                        break;
                    }
                    _ => {}
                }
            }
            DaemonResponse::ExecutionStatusResponse { status, .. } => {
                match &status {
                    ExecutionStatusDto::WaitingForUser { prompt } => {
                        // Display prompt and get user input
                        println!();
                        println!("{} {}", "?".yellow(), prompt);
                        print!("{} ", ">".cyan());
                        std::io::stdout().flush().ok();

                        // Read user input
                        let stdin = stdin();
                        let mut input = String::new();
                        if stdin.lock().read_line(&mut input).is_ok() {
                            let input = input.trim().to_string();
                            if !input.is_empty() {
                                let request = DaemonRequest::ProvideInput {
                                    id: task_id.to_string(),
                                    input,
                                };
                                client.request(request).await?;
                            }
                        }
                    }
                    ExecutionStatusDto::Completed { reason } => {
                        println!();
                        println!("{} Task completed: {}", "✓".green(), reason);
                        break;
                    }
                    ExecutionStatusDto::Failed { error } => {
                        println!();
                        println!("{} Task failed: {}", "✗".red(), error);
                        break;
                    }
                    ExecutionStatusDto::Cancelled => {
                        println!();
                        println!("{} Task was cancelled", "⊘".yellow());
                        break;
                    }
                    ExecutionStatusDto::NotRunning => {
                        println!("{} Task is not running", "○".yellow());
                        break;
                    }
                    ExecutionStatusDto::Running {
                        iteration,
                        tokens_used,
                        cost,
                    } => {
                        // Show status line
                        print!(
                            "\r{} Iteration {} | Tokens: {} | Cost: ${:.4}   ",
                            "●".green(),
                            iteration,
                            tokens_used,
                            cost
                        );
                        std::io::stdout().flush().ok();
                    }
                }
            }
            DaemonResponse::WaitingForInput { prompt, .. } => {
                // Display prompt and get user input
                println!();
                println!("{} {}", "?".yellow(), prompt);
                print!("{} ", ">".cyan());
                std::io::stdout().flush().ok();

                // Read user input
                let stdin = stdin();
                let mut input = String::new();
                if stdin.lock().read_line(&mut input).is_ok() {
                    let input = input.trim().to_string();
                    if !input.is_empty() {
                        let request = DaemonRequest::ProvideInput {
                            id: task_id.to_string(),
                            input,
                        };
                        client.request(request).await?;
                    }
                }
            }
            DaemonResponse::Error { message } => {
                eprintln!("{} {}", "✗".red(), message);
                break;
            }
            _ => {}
        }

        // Small delay to avoid spinning
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    Ok(())
}

/// Handle an execution event and print appropriate output.
fn handle_execution_event(event: &ExecutionEventDto, mid_line: &mut bool) {
    match event {
        ExecutionEventDto::IterationComplete {
            iteration,
            tokens_used,
            cost,
        } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            // Update status line
            print!(
                "\r{} Iteration {} | Tokens: {} | Cost: ${:.4}   ",
                "●".green(),
                iteration,
                tokens_used,
                cost
            );
            std::io::stdout().flush().ok();
        }
        ExecutionEventDto::ToolCalled { name, result } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!();
            println!("{} Tool: {}", "→".blue(), name.cyan());
            // Truncate result if too long
            let display_result = if result.len() > 200 {
                format!("{}...", &result[..200])
            } else {
                result.clone()
            };
            println!("  {}", display_result.dimmed());
        }
        ExecutionEventDto::LlmResponse { content } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!();
            println!("{}", content);
        }
        ExecutionEventDto::TextDelta { content } => {
            // Print streaming text immediately without newline
            print!("{}", content);
            std::io::stdout().flush().ok();
            *mid_line = true;
        }
        ExecutionEventDto::WaitingForUser { prompt } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!();
            println!("{} {}", "?".yellow(), prompt);
        }
        ExecutionEventDto::Completed { reason } => {
            if *mid_line {
                println!();
            }
            println!();
            println!("{} Task completed: {}", "✓".green(), reason);
        }
        ExecutionEventDto::Failed { error } => {
            if *mid_line {
                println!();
            }
            println!();
            println!("{} Task failed: {}", "✗".red(), error);
        }
        ExecutionEventDto::ActivityChanged { activity } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            let activity_str = match activity {
                ActivityDto::Thinking => "Thinking...",
                ActivityDto::Streaming => "Writing...",
                ActivityDto::ExecutingTool { name } => {
                    println!("{} Running {}...", "⚙".blue(), name.cyan());
                    return;
                }
                ActivityDto::WaitingForTool { name } => {
                    println!("{} Waiting for {}...", "⏳".yellow(), name);
                    return;
                }
                ActivityDto::Idle => return,
            };
            print!("\r{} {}   ", "●".blue(), activity_str);
            std::io::stdout().flush().ok();
        }
        ExecutionEventDto::ToolStarted { name } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!("{} {}...", "⚙".blue(), name.cyan());
        }
        ExecutionEventDto::ToolCompleted { name, result } => {
            // Display truncated result
            let display = if result.len() > 200 {
                format!("{}...", &result[..200])
            } else {
                result.clone()
            };
            println!("  {} {}", "✓".green(), name);
            println!("  {}", display.dimmed());
        }
        ExecutionEventDto::BudgetWarning {
            budget_type,
            threshold,
            current,
            limit,
        } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!(
                "{} Budget warning: {} at {:.0}% (${:.2}/${:.2})",
                "⚠".yellow(),
                budget_type,
                threshold * 100.0,
                current,
                limit
            );
        }
        ExecutionEventDto::BudgetExceeded {
            budget_type,
            current,
            limit,
        } => {
            if *mid_line {
                println!();
                *mid_line = false;
            }
            println!(
                "{} Budget exceeded: {} (${:.2}/${:.2})",
                "✗".red(),
                budget_type,
                current,
                limit
            );
        }
    }
}

async fn run_repl(config: &Config) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    // Check if daemon is running
    if !is_daemon_running(&daemon_config) {
        println!("{} Daemon is not running", "○".yellow());
        println!("Start with: {} daemon", "np".cyan());
        return Ok(());
    }

    // Connect and start REPL
    let client = DaemonClient::connect(&daemon_config).await?;
    let working_dir = std::env::current_dir().unwrap_or_default();
    let mut repl = Repl::new(client, working_dir);
    repl.run().await?;

    Ok(())
}

fn handle_task_response(response: DaemonResponse) {
    match response {
        DaemonResponse::Task(Some(task)) => {
            println!("{} {}", task.id.0.cyan(), task.description);
            println!("  Status: {:?}", task.status);
            println!("  Priority: {}", task.priority);
            if !task.tags.is_empty() {
                println!("  Tags: {}", task.tags.join(", "));
            }
            if let Some(ctx) = &task.context {
                println!("  Context: {}", ctx);
            }
        }
        DaemonResponse::Task(None) => {
            println!("{} Task not found", "!".yellow());
        }
        DaemonResponse::Error { message } => {
            eprintln!("{} {}", "✗".red(), message);
        }
        _ => {}
    }
}

fn handle_tasks_response(response: DaemonResponse) {
    match response {
        DaemonResponse::Tasks(tasks) => {
            if tasks.is_empty() {
                println!("{} No tasks found", "○".yellow());
            } else {
                for task in tasks {
                    let status_icon = match task.status {
                        neuraphage::TaskStatus::Running => "●".green(),
                        neuraphage::TaskStatus::Queued => "○".white(),
                        neuraphage::TaskStatus::WaitingForUser => "?".yellow(),
                        neuraphage::TaskStatus::Blocked => "◌".red(),
                        neuraphage::TaskStatus::Paused => "◑".yellow(),
                        neuraphage::TaskStatus::Completed => "✓".green(),
                        neuraphage::TaskStatus::Failed => "✗".red(),
                        neuraphage::TaskStatus::Cancelled => "⊘".white(),
                    };
                    println!("{} {} {}", status_icon, task.id.0.cyan(), task.description);
                }
            }
        }
        DaemonResponse::Error { message } => {
            eprintln!("{} {}", "✗".red(), message);
        }
        _ => {}
    }
}

fn handle_counts_response(response: DaemonResponse) {
    match response {
        DaemonResponse::Counts {
            queued,
            running,
            waiting,
            blocked,
            paused,
            completed,
            failed,
            cancelled,
        } => {
            let active = queued + running + waiting + blocked + paused;
            let terminal = completed + failed + cancelled;

            println!("{} Task Statistics", "📊".blue());
            println!();
            println!("  Active:    {}", active.to_string().cyan());
            println!("    Queued:     {}", queued);
            println!("    Running:    {}", running.to_string().green());
            println!("    Waiting:    {}", waiting);
            println!("    Blocked:    {}", blocked);
            println!("    Paused:     {}", paused);
            println!();
            println!("  Terminal:  {}", terminal);
            println!("    Completed:  {}", completed.to_string().green());
            println!("    Failed:     {}", failed.to_string().red());
            println!("    Cancelled:  {}", cancelled);
        }
        DaemonResponse::Error { message } => {
            eprintln!("{} {}", "✗".red(), message);
        }
        _ => {}
    }
}
