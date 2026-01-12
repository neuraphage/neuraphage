# Design Document: `np tui` Subcommand

**Author:** Claude
**Date:** 2026-01-11
**Status:** Ready for Review
**Review Passes:** 5/5

## Summary

Add an `np tui` CLI subcommand that launches the parallel workstream TUI, connecting it to the daemon via the existing Unix socket protocol. This wires up the TUI infrastructure built in phases 1-6 to provide a usable multi-task monitoring interface.

## Problem Statement

### Background

Neuraphage supports concurrent task execution where multiple AI agents work in parallel on different tasks, each in isolated git worktrees. The existing `np attach <id>` command allows monitoring a single task at a time, but operators running 5-20 concurrent workstreams need unified visibility.

Phases 1-6 of the parallel TUI design have been implemented:
- Phase 1: Core types (`ParallelTuiState`, `Workstream`, `OutputLine`)
- Phase 2: Layout modes (Dashboard, Split, Grid, Focus)
- Phase 3: Real-time features (attention levels, pulsing animations)
- Phase 4: Interaction (mode switching, dialogs, filters)
- Phase 5: Visual polish (help popup, status bar, smooth animations)
- Phase 6: Daemon integration (`SubscribeAllTasks`, `GetAllExecutionStatus`, `GetBudgetStatus`)

The TUI infrastructure exists but has no CLI entry point.

### Problem

Users cannot launch the parallel workstream TUI because:
1. No `np tui` command exists in `cli.rs`
2. No handler exists in `main.rs` to create `ParallelTuiApp`
3. No event loop connects the TUI to daemon polling

### Goals

1. Add `np tui` command that launches the parallel workstream TUI
2. Connect TUI to daemon via `DaemonClient` using existing protocol
3. Poll daemon for task list, execution status, events, and budget
4. Handle reconnection gracefully if daemon connection drops
5. Support all keyboard shortcuts and layout modes already implemented

### Non-Goals

1. Remote daemon connection (`--remote host:port`) - future work
2. Task creation from within TUI - use `np new` separately
3. Full terminal takeover for input mode - `np attach` handles that
4. Persistent TUI settings changes - config file handles that

## Proposed Solution

### Overview

Add a `Tui` command variant and handler that:
1. Connects to daemon via `DaemonClient`
2. Creates `ParallelTuiApp` with config-driven settings
3. Runs an async event loop that polls daemon and handles input
4. Gracefully exits on quit or daemon disconnect

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│                     np tui                              │
├─────────────────────────────────────────────────────────┤
│  ┌─────────────┐    ┌─────────────┐    ┌────────────┐  │
│  │ Terminal    │    │ ParallelTui │    │ Daemon     │  │
│  │ Input       │───>│ App         │<───│ Client     │  │
│  │ (crossterm) │    │             │    │ (polling)  │  │
│  └─────────────┘    └─────────────┘    └────────────┘  │
│                            │                  │         │
│                            v                  v         │
│                     ┌─────────────┐    ┌────────────┐  │
│                     │ ParallelTui │    │ Unix       │  │
│                     │ State       │    │ Socket     │  │
│                     └─────────────┘    └────────────┘  │
└─────────────────────────────────────────────────────────┘
```

### Data Flow

1. **Startup:**
   - Parse CLI args, load config
   - Connect to daemon, verify connection with `Ping`
   - Fetch initial task list via `ListTasks`
   - Fetch execution status via `GetAllExecutionStatus`
   - Fetch budget via `GetBudgetStatus`
   - Initialize `ParallelTuiApp` with data

2. **Event Loop (30 FPS render, 100ms daemon poll):**
   - Poll terminal events (keyboard input) - non-blocking, every frame
   - Poll daemon every 100ms (3 frames) via `SubscribeAllTasks`
   - Update `ParallelTuiState` from daemon responses
   - Tick animations (progress bars, attention pulsing) - every frame
   - Render frame (33ms interval)

3. **Shutdown:**
   - User presses 'q' or Ctrl+C
   - Confirm dialog if tasks are running
   - Restore terminal state
   - Exit cleanly

### API Design

#### CLI Addition (`src/cli.rs`)

```rust
/// Launch parallel workstream TUI
Tui {
    /// Layout mode override (dashboard, split, grid, focus)
    #[arg(short, long)]
    layout: Option<String>,

    /// Filter to specific task IDs
    #[arg(short, long)]
    filter: Vec<String>,
},
```

Layout mode parsing (in handler):
```rust
fn parse_layout_mode(s: &str) -> Option<LayoutMode> {
    match s.to_lowercase().as_str() {
        "dashboard" | "d" => Some(LayoutMode::Dashboard),
        "split" | "s" => Some(LayoutMode::Split),
        "grid" | "g" => Some(LayoutMode::Grid),
        "focus" | "f" => Some(LayoutMode::Focus),
        _ => None,
    }
}
```

#### Handler Function (`src/main.rs`)

```rust
async fn run_tui(
    mut client: DaemonClient,  // Takes ownership - runs until exit
    config: &Config,
    layout_override: Option<LayoutMode>,
    filter: Vec<String>,
) -> Result<()>
```

#### Event Loop Module (`src/ui/tui_runner.rs`)

The event loop complexity warrants a dedicated module:

```rust
pub struct TuiRunner {
    client: DaemonClient,
    app: ParallelTuiApp,
    tui: ParallelTui,
    poll_interval: Duration,
    last_task_refresh: Instant,
    last_event_poll: Instant,
}

impl TuiRunner {
    pub fn new(client: DaemonClient, config: &Config) -> Result<Self>;
    pub async fn run(&mut self) -> Result<()>;
    async fn poll_daemon(&mut self) -> Result<()>;
    fn handle_input(&mut self, event: Event) -> Result<()>;
    fn render(&mut self) -> Result<()>;
}
```

This follows the pattern of `run_interactive_loop` but with multi-task awareness.

#### State Sync Types (new in `src/ui/parallel_tui.rs`)

```rust
impl ParallelTuiState {
    /// Sync state from daemon responses.
    /// Called on initial fetch and periodically during event loop.
    pub fn sync_from_daemon(
        &mut self,
        tasks: &[Task],
        statuses: &[(String, ExecutionStatusDto)],
        events: &[(String, ExecutionEventDto)],
        budget: &BudgetStatusResponse,
    );
}

impl Workstream {
    /// Create from task and optional execution status.
    pub fn from_task(task: &Task, status: Option<&ExecutionStatusDto>) -> Self {
        let mut ws = Workstream::new(
            task.id.clone(),
            &task.description,
            task.status,
        );
        if let Some(status) = status {
            ws.apply_execution_status(status);
        }
        ws
    }

    /// Update from execution status DTO.
    pub fn apply_execution_status(&mut self, status: &ExecutionStatusDto) {
        match status {
            ExecutionStatusDto::Running { iteration, cost, .. } => {
                self.iteration = *iteration;
                self.cost_spent = *cost;
            }
            ExecutionStatusDto::WaitingForUser { prompt } => {
                self.attention_reason = Some(prompt.clone());
            }
            // ... other variants
        }
    }
}
```

### Implementation Plan

**Phase 0: Module Restructure**
- Rename `src/ui/` to `src/tui/`
- Rename `tui.rs` to `single.rs`
- Rename `parallel_tui.rs` to `multi.rs`
- Move `notifications.rs` to `src/notifications.rs`
- Update all imports across codebase
- Run tests to verify

**Phase 1: CLI Command**
- Add `Tui` variant to `Command` enum in `cli.rs`
- Add `run_tui` stub handler in `main.rs`
- Wire up command routing
- Test: `np tui --help` works

**Phase 2: TuiRunner Module**
- Create `src/tui/runner.rs`
- Implement `TuiRunner::new()` with daemon connection
- Implement initial data fetch (tasks, status, budget)
- Add `Workstream::from_task()` and `apply_execution_status()`
- Export from `src/tui/mod.rs`

**Phase 3: Event Loop**
- Implement `TuiRunner::run()` main loop
- Poll terminal events via `crossterm::event::poll` (every frame, 33ms)
- Poll daemon via `SubscribeAllTasks` (every 100ms)
- Refresh full task list via `ListTasks` (every 1s)
- Update budget via `GetBudgetStatus` (every 1s)
- Handle terminal resize events

**Phase 4: Input Handling**
- Route keyboard events to `ParallelTuiApp::handle_key_event()`
- Handle quit confirmation flow
- Implement Ctrl+C signal handler

**Phase 5: Reconnection & Error Handling**
- Detect daemon disconnect (request error)
- Show `ConnectionStatus::Disconnected` in UI
- Attempt reconnect with exponential backoff (1s, 2s, 4s, ..., max 30s)
- Handle edge cases (zero tasks, terminal too small)
- Add SIGHUP handler for clean exit

**Phase 6: Tests & Polish**
- Unit tests for CLI parsing
- Unit tests for `Workstream::from_task()`
- Integration test with mock daemon
- Update `src/ui/mod.rs` exports

## Alternatives Considered

### Alternative 1: Separate Binary

- **Description:** Create `np-tui` as a separate binary
- **Pros:** Smaller main binary, could be optional feature
- **Cons:** Complicates distribution, user confusion, shared code duplication
- **Why not chosen:** Single binary is simpler and aligns with existing design

### Alternative 2: Web UI Instead

- **Description:** Launch a web server and open browser
- **Pros:** Richer visuals, easier layout, mobile access
- **Cons:** Different tech stack, additional dependencies, latency
- **Why not chosen:** Terminal-native is the project's philosophy; web UI can be added later

### Alternative 3: Blocking Synchronous Loop

- **Description:** Use `std::thread` and blocking I/O instead of async
- **Pros:** Simpler code, no tokio in TUI
- **Cons:** Can't share daemon connection, harder reconnection logic
- **Why not chosen:** Async integrates better with existing daemon client

## Technical Considerations

### Dependencies

Already present in `Cargo.toml`:
- `ratatui = "0.29"` - TUI framework
- `crossterm = "0.28"` - Terminal handling
- `tokio` - Async runtime

No new dependencies required.

### Performance

- **Render throttle:** 30 FPS cap (33ms per frame)
- **Daemon polling:** Every 100ms for events, every 1s for full task refresh
- **Event batching:** Process all available events per poll
- **Memory:** ~1MB per workstream (output buffer of 1000 lines)
- **CPU:** Minimal when idle (sleep between frames, no busy loop)
- **Socket I/O:** Non-blocking where possible, timeout on requests

### Security

- Uses existing Unix socket authentication
- No new attack surface
- Terminal state always restored on exit/panic

### Error Handling

| Error | Handling |
|-------|----------|
| Daemon not running | Print error message, suggest `np daemon start`, exit 1 |
| Daemon connection lost | Show disconnected status in UI, attempt reconnect |
| Invalid layout mode arg | Print error, show valid options, exit 1 |
| Terminal initialization fails | Print error, exit 1 (no cleanup needed) |
| Panic during render | Catch with `catch_unwind`, restore terminal, re-panic |
| Zero tasks | Show "No tasks. Create one with `np new`" placeholder |
| Terminal too small | Show "Terminal too small" message, min 40x10 |
| Terminal resize | Handle `Event::Resize`, re-render immediately |
| SIGHUP (SSH disconnect) | Restore terminal, exit cleanly |
| Task filter matches nothing | Show "No matching tasks" with filter hint |

### Testing Strategy

1. **Unit tests:**
   - CLI parsing for `Tui` command
   - State sync from daemon responses
   - Event handling (key mappings)

2. **Integration tests:**
   - TUI startup with mock daemon
   - Reconnection behavior

3. **Manual testing:**
   - Launch with 0, 1, 5, 20 tasks
   - All layout modes
   - All keyboard shortcuts
   - Daemon restart while TUI running

### Rollout Plan

1. Implement behind feature flag (optional)
2. Test with internal users
3. Enable by default in next minor release

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Terminal state corruption on crash | Low | High | Use `std::panic::catch_unwind` and always restore terminal |
| High CPU from busy loop | Medium | Medium | Use `tokio::time::sleep` between frames |
| Daemon connection flapping | Low | Medium | Exponential backoff with max 30s delay |
| Output buffer memory growth | Low | Low | Already bounded to 1000 lines per workstream |
| Blocking daemon request stalls UI | Medium | Medium | Use request timeout (500ms), skip update on timeout |
| Task list changes during render | Low | Low | Clone state before render, atomic updates |

## Open Questions

- [x] Should `np tui` auto-start daemon if not running? **No - require explicit `np daemon start`**
- [x] Should layout mode persist across TUI sessions? **Yes - via config file, not runtime flag**
- [x] Should TUI show completed/failed tasks or only active? **Show active by default, 'a' key toggles show-all**

## Usage Examples

```bash
# Basic usage - launch TUI for all tasks
np tui

# Start with specific layout
np tui --layout grid
np tui -l split

# Filter to specific tasks
np tui --filter task-abc --filter task-xyz
```

## Module Restructure

Before implementation, refactor `src/ui/` to cleaner structure:

```
# Before (messy)
src/ui/
├── mod.rs
├── notifications.rs    # Not really UI
├── parallel_tui.rs     # Underscore, verbose
└── tui.rs

# After (clean)
src/tui/
├── mod.rs
├── single.rs           # Was ui/tui.rs
├── multi.rs            # Was ui/parallel_tui.rs
└── runner.rs           # New: event loop

src/notifications.rs    # Moved out - terminal title stuff isn't TUI
```

## Key Files to Modify

| File | Changes |
|------|---------|
| `src/cli.rs` | Add `Tui` variant to `Command` enum |
| `src/main.rs` | Add handler that calls `TuiRunner::run()` |
| `src/tui/runner.rs` | New file: event loop and daemon integration |
| `src/tui/multi.rs` | Add `from_task()`, `apply_execution_status()`, `sync_from_daemon()` |
| `src/tui/mod.rs` | Export runner module and types |
| `src/notifications.rs` | Move from `src/ui/notifications.rs` |

## References

- `docs/parallel-workstream-tui-design.md` - Original TUI design
- `src/tui/multi.rs` - Multi-workstream TUI (was `ui/parallel_tui.rs`)
- `src/tui/single.rs` - Single-task TUI (was `ui/tui.rs`)
- `src/daemon.rs` - Daemon protocol (lines 206-212, 279-292)
- `src/main.rs:915` - Existing `run_interactive_loop` for single task
