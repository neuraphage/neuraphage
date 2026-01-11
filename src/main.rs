//! Neuraphage CLI entry point.

use clap::Parser;
use colored::*;
use eyre::{Context, Result};
use log::info;
use std::fs;
use std::path::PathBuf;

mod cli;

use cli::{Cli, Command};
use neuraphage::config::Config;
use neuraphage::daemon::{Daemon, DaemonClient, DaemonRequest, DaemonResponse, is_daemon_running};

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

#[tokio::main]
async fn main() -> Result<()> {
    setup_logging().context("Failed to setup logging")?;

    let cli = Cli::parse();
    let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;

    info!("Starting with config from: {:?}", cli.config);

    match cli.command {
        Some(Command::Daemon { foreground }) => run_daemon(&config, foreground).await,
        Some(cmd) => run_client_command(&config, cmd).await,
        None => {
            // Default: show status
            show_status(&config).await
        }
    }
}

async fn run_daemon(config: &Config, foreground: bool) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    if is_daemon_running(&daemon_config) {
        eprintln!("{} Daemon is already running", "!".yellow());
        return Ok(());
    }

    if !foreground {
        // TODO: Implement proper daemonization
        eprintln!(
            "{} Background mode not yet implemented, running in foreground",
            "!".yellow()
        );
    }

    println!("{} Starting neuraphage daemon...", "→".blue());
    let daemon = Daemon::new(daemon_config)?;
    daemon.run().await?;

    Ok(())
}

async fn run_client_command(config: &Config, command: Command) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    // Check if daemon is running, start if needed
    if !is_daemon_running(&daemon_config) {
        if config.daemon.auto_start {
            eprintln!("{} Daemon not running. Start with: neuraphage daemon", "!".yellow());
            return Ok(());
        } else {
            eprintln!("{} Daemon not running", "!".red());
            return Ok(());
        }
    }

    let mut client = DaemonClient::connect(&daemon_config).await?;

    match command {
        Command::Daemon { .. } => unreachable!(),

        Command::New {
            description,
            priority,
            tags,
            context,
        } => {
            let request = DaemonRequest::CreateTask {
                description,
                priority,
                tags,
                context,
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

        Command::Stop => {
            let request = DaemonRequest::Shutdown;
            let response = client.request(request).await?;
            match response {
                DaemonResponse::Shutdown => println!("{} Daemon stopped", "✓".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }

        Command::Ping => {
            let request = DaemonRequest::Ping;
            let response = client.request(request).await?;
            match response {
                DaemonResponse::Pong => println!("{} Daemon is running", "✓".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "✗".red(), message),
                _ => {}
            }
        }
    }

    Ok(())
}

async fn show_status(config: &Config) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    if is_daemon_running(&daemon_config) {
        println!("{} Daemon is running", "✓".green());

        // Try to get stats
        if let Ok(mut client) = DaemonClient::connect(&daemon_config).await {
            let response = client.request(DaemonRequest::TaskCounts).await?;
            handle_counts_response(response);
        }
    } else {
        println!("{} Daemon is not running", "○".yellow());
        println!("Start with: {} daemon", "neuraphage".cyan());
    }

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
