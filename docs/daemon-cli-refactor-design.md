# Design Document: Daemon CLI Refactoring

**Author:** Claude (with Scott Idler)
**Date:** 2026-01-11
**Status:** In Review
**Review Passes:** 5/5

## Summary

Refactor neuraphage's CLI structure to consolidate daemon-related commands (`daemon`, `stop`, `ping`) under a single `daemon` subcommand with nested operations, following the pattern established in the `aka` project.

## Problem Statement

### Background

Neuraphage uses clap for CLI argument parsing. The current design places all commands at the same level in a flat `Command` enum. This includes daemon lifecycle commands (`Daemon`, `Stop`, `Ping`) alongside task management commands (`New`, `List`, `Show`, etc.).

### Problem

The current CLI structure is confusing because:

1. **Inconsistent command grouping**: `Stop` and `Ping` are daemon operations but appear as peers to `Daemon` rather than being nested under it
2. **Unclear command relationships**: Users can't tell that `stop` and `ping` require the daemon to be running
3. **Flat namespace pollution**: All 19 commands compete at the top level, making help output overwhelming
4. **Discoverability issues**: New users don't know that daemon commands exist or how they relate

**Before (current confused structure):**
```
np daemon [-f]     # Start daemon
np stop            # Stop daemon (why not under daemon?)
np ping            # Check daemon (why not under daemon?)
np new ...
np list ...
```

**After (proposed clean structure):**
```
np daemon start [-f] [-r]  # Start daemon (-r to restart if running)
np daemon stop             # Stop daemon
np daemon status           # Check daemon (renamed from ping)
np new ...
np list ...
```

### Goals

- Consolidate all daemon lifecycle commands under a single `daemon` subcommand
- Improve CLI discoverability and help text
- Follow established patterns from the `aka` project
- Maintain backwards compatibility during transition (if desired)

### Non-Goals

- Refactoring task management commands (separate design)
- Changing the daemon's internal architecture
- Modifying the daemon/client protocol

## Proposed Solution

### Overview

Move `Stop` and `Ping` commands under the `Daemon` subcommand using clap's nested subcommand pattern. The `daemon` command will have its own `DaemonCommand` enum for lifecycle operations.

### Architecture

#### Option A: Nested Subcommands (Recommended)

```
np daemon start [-f|--foreground] [-r|--restart]  # Start daemon
np daemon stop                                     # Stop daemon
np daemon status                                   # Check if daemon is running (renamed from ping)
```

The `-r|--restart` flag on `start` handles restart: if daemon is running, stop it first, then start.

This approach uses clap's subcommand nesting:

```rust
#[derive(Subcommand)]
pub enum Command {
    /// Manage the daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),

    // ... task commands ...
}

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
```

#### Option B: Flag-Based (AKA Style)

```
np daemon --start [-f]   # Start daemon
np daemon --stop         # Stop daemon
np daemon --status       # Check status
np daemon --restart      # Restart
```

This uses boolean flags (like `aka`):

```rust
#[derive(Parser)]
pub struct DaemonOpts {
    #[arg(long, help = "Start the daemon")]
    start: bool,

    #[arg(long, help = "Stop the daemon")]
    stop: bool,

    #[arg(long, help = "Show daemon status")]
    status: bool,

    #[arg(long, help = "Restart the daemon")]
    restart: bool,

    #[arg(short, long, help = "Run in foreground (with --start)")]
    foreground: bool,
}
```

### Comparison

| Aspect | Option A (Nested Subcommands) | Option B (Flags) |
|--------|------------------------------|------------------|
| Discoverability | Better - `np daemon --help` shows subcommands | Worse - flags are less obvious |
| Tab completion | Better - each subcommand completes separately | Worse - all flags compete |
| Extensibility | Better - easy to add command-specific args | Worse - flag combinations get complex |
| Help text | Better - each subcommand has own help | Worse - single help page for all |
| User familiarity | Standard pattern (`git remote add`) | Less common pattern |
| Implementation | Slightly more code | Simpler, but needs manual validation |

**Recommendation: Option A (Nested Subcommands)**

While `aka` uses the flag-based approach, nested subcommands are more idiomatic for clap and provide better UX for complex operations. The pattern is familiar from tools like `git`, `docker`, and `cargo`.

### Critical Implementation Note: Daemonize Before Tokio

The current code handles a critical constraint: **daemonization must happen BEFORE the tokio runtime starts**. The tokio runtime cannot survive a fork. This is why `main()` checks for background daemon mode before creating the runtime:

```rust
// In main() - BEFORE tokio runtime
if let Some(Command::Daemon(DaemonCommand::Start { foreground: false })) = &cli.command {
    // Daemonize BEFORE starting tokio runtime
    return daemonize(&daemon_config);
}

// Only then create tokio runtime
let rt = tokio::runtime::Runtime::new()?;
```

This constraint must be preserved in the refactored code.

### Data Model

No changes to the daemon protocol or data structures. This is a CLI-only refactor.

### API Design

#### New CLI Structure

```rust
// src/cli.rs

#[derive(Parser)]
#[command(
    name = "neuraphage",
    about = "Multi-task AI orchestrator daemon",
    version = env!("GIT_DESCRIBE"),
    after_help = AFTER_HELP.as_str()
)]
pub struct Cli {
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage the neuraphage daemon
    #[command(subcommand)]
    Daemon(DaemonCommand),

    // === Task Management ===
    /// Create a new task
    New { /* ... */ },

    /// List tasks
    #[command(alias = "ls")]
    List { /* ... */ },

    /// Show task details
    Show { id: String },

    /// Show ready tasks
    Ready,

    /// Show blocked tasks
    Blocked,

    /// Set task status
    Status { id: String, status: String },

    /// Close a task
    Close { /* ... */ },

    /// Add dependency between tasks
    Depend { blocked: String, blocker: String },

    /// Remove dependency between tasks
    Undepend { blocked: String, blocker: String },

    /// Show task statistics
    Stats,

    // === Task Execution ===
    /// Run a task interactively (create, start, attach)
    Run { /* ... */ },

    /// Attach to a running task
    Attach { id: String },

    /// Start a task
    Start { id: String },

    /// Cancel a running task
    Cancel { id: String },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon
    Start {
        /// Run in foreground (don't daemonize)
        #[arg(short, long)]
        foreground: bool,
    },

    /// Stop the daemon
    Stop,

    /// Check daemon status
    Status,

    /// Restart the daemon (stop then start)
    Restart {
        /// Run in foreground after restart
        #[arg(short, long)]
        foreground: bool,
    },
}
```

#### Command Handler Changes

**Important**: Background daemon start must be handled in `main()` BEFORE tokio runtime, while other daemon commands can be async.

```rust
// src/main.rs

fn main() -> Result<()> {
    let cli = Cli::parse();

    // CRITICAL: Background daemon start must happen before tokio runtime
    // The tokio runtime cannot survive a fork
    if let Some(Command::Daemon(DaemonCommand::Start { foreground: false, restart })) = &cli.command {
        let config = Config::load(cli.config.as_ref())?;
        let daemon_config = config.to_daemon_config();

        if is_daemon_running(&daemon_config) {
            if *restart {
                // Stop the running daemon first
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    if let Ok(mut client) = DaemonClient::connect(&daemon_config).await {
                        client.request(DaemonRequest::Shutdown).await.ok();
                    }
                });
                std::thread::sleep(std::time::Duration::from_millis(500));
            } else {
                eprintln!("{} Daemon is already running (use -r to restart)", "!".yellow());
                return Ok(());
            }
        }
        return daemonize(&daemon_config);
    }

    // All other commands run with tokio
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    setup_logging()?;
    let config = Config::load(cli.config.as_ref())?;

    match cli.command {
        Some(Command::Daemon(daemon_cmd)) => {
            handle_daemon_command(&config, daemon_cmd).await
        }
        Some(cmd) => run_client_command(&config, cmd).await,
        None => run_repl(&config).await,
    }
}

/// Handle daemon commands that can run within tokio runtime
/// (foreground start, stop, status)
async fn handle_daemon_command(config: &Config, cmd: DaemonCommand) -> Result<()> {
    let daemon_config = config.to_daemon_config();

    match cmd {
        DaemonCommand::Start { foreground: true, restart } => {
            // Foreground mode - runs in current process
            if is_daemon_running(&daemon_config) {
                if restart {
                    // Stop the running daemon first
                    let mut client = DaemonClient::connect(&daemon_config).await?;
                    client.request(DaemonRequest::Shutdown).await?;
                    wait_for_daemon_stop(&daemon_config).await;
                } else {
                    eprintln!("{} Daemon is already running (use -r to restart)", "!".yellow());
                    return Ok(());
                }
            }
            println!("{} Starting daemon in foreground...", "->".blue());
            let daemon = Daemon::new(daemon_config)?;
            daemon.run().await?;
        }
        DaemonCommand::Start { foreground: false, .. } => {
            // Should not reach here - handled in main() before tokio
            unreachable!("Background daemon start handled before tokio runtime");
        }
        DaemonCommand::Stop => {
            if !is_daemon_running(&daemon_config) {
                eprintln!("{} Daemon is not running", "!".yellow());
                return Ok(());
            }
            let mut client = DaemonClient::connect(&daemon_config).await?;
            let response = client.request(DaemonRequest::Shutdown).await?;
            match response {
                DaemonResponse::Shutdown => println!("{} Daemon stopped", "ok".green()),
                DaemonResponse::Error { message } => eprintln!("{} {}", "x".red(), message),
                _ => {}
            }
        }
        DaemonCommand::Status => {
            if is_daemon_running(&daemon_config) {
                let mut client = DaemonClient::connect(&daemon_config).await?;
                let response = client.request(DaemonRequest::Ping).await?;
                match response {
                    DaemonResponse::Pong => println!("{} Daemon is running", "ok".green()),
                    _ => println!("{} Daemon socket exists but not responding", "!".yellow()),
                }
            } else {
                println!("{} Daemon is not running", "x".red());
            }
        }
    }
    Ok(())
}

/// Wait for daemon to stop (socket to be removed or connection refused)
async fn wait_for_daemon_stop(daemon_config: &DaemonConfig) {
    for _ in 0..20 {  // Max 2 seconds
        if !is_daemon_running(daemon_config) {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}
```

### Implementation Plan

#### Phase 1: Update cli.rs

**File:** `src/cli.rs`

1. Add `DaemonCommand` enum after imports:
   ```rust
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
   ```

2. Modify `Command` enum:
   - Change `Daemon { foreground: bool }` to `#[command(subcommand)] Daemon(DaemonCommand)`
   - Remove `Stop` variant
   - Remove `Ping` variant

#### Phase 2: Update main.rs

**File:** `src/main.rs`

1. Update `main()` to check for `Command::Daemon(DaemonCommand::Start { foreground: false, restart })` and handle restart flag

2. Update `async_main()` to route `Command::Daemon(cmd)` to new `handle_daemon_command()`

3. Add `handle_daemon_command()` function with match arms for:
   - `DaemonCommand::Start { foreground: true, restart }` - foreground mode with optional restart
   - `DaemonCommand::Start { foreground: false, .. }` - unreachable (handled pre-tokio)
   - `DaemonCommand::Stop` - shutdown request
   - `DaemonCommand::Status` - ping/status check

4. Add `wait_for_daemon_stop()` helper function

5. Remove `Command::Stop` and `Command::Ping` handling from `run_client_command()`

#### Phase 3: Update tests and documentation

1. Update any CLI tests to use new command structure
2. Update README if it documents CLI usage
3. Regenerate shell completions if applicable

#### Phase 4: Cleanup

1. Remove any dead code from removed variants
2. Run `cargo clippy` to catch any issues
3. Run full test suite

### Expected Help Text Output

After refactoring, `np --help` will show:

```
neuraphage - Multi-task AI orchestrator daemon

Usage: np [OPTIONS] [COMMAND]

Commands:
  daemon   Manage the neuraphage daemon
  new      Create a new task
  list     List tasks
  show     Show task details
  ready    Show ready tasks
  ...
```

And `np daemon --help` will show:

```
Manage the neuraphage daemon

Usage: np daemon <COMMAND>

Commands:
  start    Start the daemon
  stop     Stop the daemon
  status   Check daemon status
```

And `np daemon start --help` will show:

```
Start the daemon

Usage: np daemon start [OPTIONS]

Options:
  -f, --foreground  Run in foreground (don't daemonize)
  -r, --restart     Restart: stop daemon first if already running
  -h, --help        Print help
```

### Migration Strategy

For users with existing scripts using `np stop` or `np ping`:

1. **Immediate removal** (recommended): Since neuraphage is pre-1.0, breaking changes are acceptable. Remove old commands outright.

2. **Deprecation period** (if needed): Keep old commands but emit a warning:
   ```rust
   #[deprecated(note = "Use `np daemon stop` instead")]
   Stop,
   ```
   This would print: `warning: `np stop` is deprecated, use `np daemon stop``

## Alternatives Considered

### Alternative 1: Keep Flat Structure

**Description:** Leave the CLI as-is with all commands at the top level.

**Pros:**
- No migration needed
- Fewer keystrokes for common operations

**Cons:**
- Continues the confusing design
- Poor discoverability
- Hard to extend with more daemon operations

**Why not chosen:** The current structure is objectively confusing and violates the principle of least surprise.

### Alternative 2: Command Groups via Aliases

**Description:** Keep flat structure but add aliases like `daemon-stop`, `daemon-ping`.

**Pros:**
- Backwards compatible
- Clear naming

**Cons:**
- Still pollutes the namespace
- Doesn't leverage clap's subcommand capabilities
- Inconsistent with modern CLI conventions

**Why not chosen:** This is a half-measure that doesn't solve the core problem.

### Alternative 3: Task Subcommand Too

**Description:** Also group task commands under `np task <cmd>`.

**Pros:**
- Maximum organization
- Clean separation of concerns

**Cons:**
- Breaking change for all commands
- More typing for common operations
- Scope creep

**Why not chosen:** Out of scope for this design. Task command grouping should be a separate design decision.

## Technical Considerations

### Dependencies

- clap (already in use) - no new dependencies

### Performance

No performance impact. This is purely a CLI parsing change.

### Security

No security implications. CLI structure doesn't affect security.

### Testing Strategy

1. **Unit tests**: Test CLI parsing with the new structure
2. **Integration tests**: Ensure daemon lifecycle commands work correctly
3. **Manual testing**: Verify help text, tab completion, error messages

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_daemon_start_parse() {
        let cli = Cli::parse_from(["np", "daemon", "start"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start { foreground: false }))
        ));
    }

    #[test]
    fn test_daemon_start_foreground() {
        let cli = Cli::parse_from(["np", "daemon", "start", "-f"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Start { foreground: true }))
        ));
    }

    #[test]
    fn test_daemon_stop_parse() {
        let cli = Cli::parse_from(["np", "daemon", "stop"]);
        assert!(matches!(
            cli.command,
            Some(Command::Daemon(DaemonCommand::Stop))
        ));
    }
}
```

### Architectural Fit

#### Module Structure

The refactoring maintains the existing module boundaries:

```
src/
├── main.rs      # CLI entry point and command routing (modified)
├── cli.rs       # CLI definitions with clap derives (modified)
├── daemon.rs    # Daemon implementation (unchanged)
└── lib.rs       # Core types and exports (unchanged)
```

The change is purely in the CLI layer - no changes to `daemon.rs` or the core library. This is a good separation of concerns.

#### Future Extensibility

The nested subcommand pattern makes it easy to add more daemon operations:

```rust
#[derive(Subcommand)]
pub enum DaemonCommand {
    Start { foreground: bool, restart: bool },
    Stop,
    Status,

    // Future additions:
    /// View daemon logs
    Logs {
        /// Number of lines to show
        #[arg(short, long, default_value = "50")]
        lines: usize,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
    },
    /// Reload daemon configuration
    Reload,
    /// Show daemon version and build info
    Info,
}
```

#### Consistency with Task Commands

This design intentionally does NOT group task commands under `np task <cmd>`. However, the pattern established here could be applied later if desired:

```rust
// Current (after this refactor):
np daemon start
np new "task"
np list

// Possible future:
np daemon start
np task new "task"
np task list
```

The current design keeps task commands at the top level for brevity since they are the primary use case. This is a deliberate trade-off.

#### Handler Separation

The design introduces a clear handler separation:

1. `main()` - pre-tokio handling for background operations
2. `async_main()` - routing to appropriate handlers
3. `handle_daemon_command()` - daemon-specific logic
4. `run_client_command()` - task-related client logic

This improves code organization and makes each function's responsibility clearer.

### Rollout Plan

1. Implement new structure
2. Add deprecation warnings for old commands (optional)
3. Update documentation
4. Release as minor version bump (if deprecating) or patch (if removing)

## Edge Cases and Error Handling

### `np daemon` without subcommand

Clap will display help automatically when a subcommand is required but not provided:

```
$ np daemon
error: 'np daemon' requires a subcommand but one was not provided

Usage: np daemon <COMMAND>

Commands:
  start    Start the daemon
  stop     Stop the daemon
  status   Check daemon status

For more information, try '--help'.
```

This is the desired behavior - no code needed.

### Stale socket file

The `is_daemon_running()` function already handles stale sockets by attempting a connection:

```rust
fn is_daemon_running(config: &DaemonConfig) -> bool {
    if !config.socket_path.exists() {
        return false;
    }
    // Try to connect - if it fails, socket is stale
    UnixStream::connect(&config.socket_path).is_ok()
}
```

For `status` command, we should report stale sockets explicitly:

```rust
DaemonCommand::Status => {
    if config.socket_path.exists() {
        match DaemonClient::connect(&daemon_config).await {
            Ok(mut client) => {
                // Socket exists and is connectable
                match client.request(DaemonRequest::Ping).await {
                    Ok(DaemonResponse::Pong) => {
                        println!("{} Daemon is running", "ok".green());
                    }
                    _ => {
                        println!("{} Daemon not responding", "!".yellow());
                    }
                }
            }
            Err(_) => {
                println!("{} Stale socket (daemon not running)", "!".yellow());
                println!("  Socket: {}", config.socket_path.display());
            }
        }
    } else {
        println!("{} Daemon is not running", "x".red());
    }
}
```

### `start -r` fails to stop daemon

If the daemon doesn't stop within the timeout when using `--restart`, start should fail gracefully:

```rust
// In background start with restart flag (pre-tokio)
if is_daemon_running(&daemon_config) && restart {
    // ... send shutdown ...
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Check if it actually stopped
    if is_daemon_running(&daemon_config) {
        eprintln!("{} Daemon did not stop in time, aborting", "!".red());
        return Ok(());
    }
}
```

### Concurrent operations

If multiple clients try to stop the daemon simultaneously, the daemon should handle this gracefully. The first `Shutdown` request succeeds; subsequent connections will fail with "connection refused" which is the expected behavior.

### Permission errors

Socket creation may fail if the directory doesn't exist or isn't writable. The daemon already creates the directory with `create_dir_all`. Errors should propagate with clear messages.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Users have scripts using `np stop` | Medium | Low | Add deprecation warning pointing to `np daemon stop` |
| Tab completion breaks | Low | Low | Regenerate shell completions |
| Help text becomes unclear | Low | Medium | Carefully craft help strings for nested structure |
| `start -r` hangs on unresponsive daemon | Low | Medium | Add timeout and clear error message |
| Stale socket confuses users | Low | Low | `status` command reports stale sockets explicitly |

## Open Questions

- [x] Should we rename `ping` to `status`? **Yes** - `status` is more intuitive
- [ ] Should we add a `daemon logs` subcommand for viewing logs?
- [ ] Should we support `np d start` as a shorthand alias for `np daemon start`?
- [ ] Do we need backwards compatibility aliases for `np stop` -> `np daemon stop`?

## Summary of Changes

| File | Change |
|------|--------|
| `src/cli.rs` | Add `DaemonCommand` enum (Start with -f/-r flags, Stop, Status), remove top-level `Stop` and `Ping` |
| `src/main.rs` | Update pre-tokio checks, add `handle_daemon_command()`, update routing |

**New CLI structure:**
```
np daemon start [-f] [-r]  # Start daemon (-r restarts if running)
np daemon stop             # Stop daemon
np daemon status           # Check daemon status
```

**Lines of code estimate:**
- ~25 lines added to cli.rs (DaemonCommand enum + doc comments)
- ~60 lines added to main.rs (handle_daemon_command function)
- ~20 lines removed from main.rs (old Stop/Ping handling)
- Net: ~65 lines added

## Review Process Summary

| Pass | Focus | Changes Made |
|------|-------|--------------|
| 1 | Completeness | Added pre-tokio constraint section, migration strategy, expected help output |
| 2 | Correctness | Fixed async/sync handling for daemonize, added proper restart logic |
| 3 | Edge Cases | Added section on stale sockets, missing subcommand, restart failures |
| 4 | Architecture | Added module structure, future extensibility, handler separation analysis |
| 5 | Clarity | Improved before/after comparison, made implementation phases more specific |

**Document Status:** Ready for user review

## References

- [AKA project daemon implementation](~/repos/scottidler/aka/src/bin/aka.rs)
- [Clap derive documentation](https://docs.rs/clap/latest/clap/_derive/index.html)
- [Clap subcommand tutorial](https://docs.rs/clap/latest/clap/_tutorial/chapter_1/index.html)
