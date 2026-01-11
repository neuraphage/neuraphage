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

use cli::{Cli, Command};
use neuraphage::config::Config;
use neuraphage::daemon::{
    Daemon, DaemonClient, DaemonRequest, DaemonResponse, ExecutionEventDto, ExecutionStatusDto, is_daemon_running,
};

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

    // Check if this is a daemon command that needs to fork
    if let Some(Command::Daemon { foreground: false }) = &cli.command {
        // Daemonize BEFORE starting tokio runtime
        let config = Config::load(cli.config.as_ref()).context("Failed to load configuration")?;
        let daemon_config = config.to_daemon_config();

        if is_daemon_running(&daemon_config) {
            eprintln!("{} Daemon is already running", "!".yellow());
            return Ok(());
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
        Some(Command::Daemon { foreground: true }) => {
            // Foreground daemon mode
            let daemon_config = config.to_daemon_config();
            if is_daemon_running(&daemon_config) {
                eprintln!("{} Daemon is already running", "!".yellow());
                return Ok(());
            }
            println!("{} Starting daemon in foreground...", "→".blue());
            let daemon = Daemon::new(daemon_config)?;
            daemon.run().await?;
            Ok(())
        }
        Some(Command::Daemon { foreground: false }) => {
            // This shouldn't be reached (handled in main), but just in case
            unreachable!("Background daemon should be handled before tokio starts")
        }
        Some(cmd) => run_client_command(&config, cmd).await,
        None => show_status(&config).await,
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

async fn run_client_command(config: &Config, command: Command) -> Result<()> {
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

        Command::Run {
            description,
            dir: _,
            priority,
            tags,
            context,
        } => {
            // Create the task
            let request = DaemonRequest::CreateTask {
                description: description.clone(),
                priority,
                tags,
                context,
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
            let request = DaemonRequest::StartTask { id: task_id.clone() };
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
            let request = DaemonRequest::StartTask { id: id.clone() };
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
                handle_execution_event(&event);

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
fn handle_execution_event(event: &ExecutionEventDto) {
    match event {
        ExecutionEventDto::IterationComplete {
            iteration,
            tokens_used,
            cost,
        } => {
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
            println!();
            println!("{}", content);
        }
        ExecutionEventDto::WaitingForUser { prompt } => {
            println!();
            println!("{} {}", "?".yellow(), prompt);
        }
        ExecutionEventDto::Completed { reason } => {
            println!();
            println!("{} Task completed: {}", "✓".green(), reason);
        }
        ExecutionEventDto::Failed { error } => {
            println!();
            println!("{} Task failed: {}", "✗".red(), error);
        }
    }
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
        println!("Start with: {} daemon", "np".cyan());
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
