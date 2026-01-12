# Design Document: Parallel Workstream TUI

**Author:** Scott A. Idler
**Date:** 2026-01-11
**Status:** Draft
**Review Passes:** 5/5

## Summary

Design an awesome TUI (Terminal User Interface) for Neuraphage that enables monitoring and orchestrating multiple parallel workstreams. The TUI should provide real-time visibility into concurrent task execution, allow fluid navigation between workstreams, surface coordination events as they happen, and integrate with budget enforcement to prevent runaway costs.

## Problem Statement

### Background

Neuraphage is designed to run N concurrent tasks as tokio tasks within a single daemon. The existing TUI (`src/ui/tui.rs`) provides a basic single-pane dashboard with:
- Task list with status indicators
- Stats panel (counts, cost, tokens)
- Task detail view
- Notifications panel
- Help overlay

This works for single-task monitoring but falls short when orchestrating 5-20+ parallel workstreams. Gas Town uses tmux as its multi-pane solution, but that pushes complexity to the user. We want native TUI support for parallel workstream orchestration.

### Problem

When running multiple workstreams in parallel, users need to:
1. See all workstreams at a glance with their status
2. Watch real-time output from multiple tasks simultaneously
3. Quickly switch attention between workstreams needing input
4. Track coordination events (rebases, syncs, nudges) between tasks
5. Identify stuck or blocked tasks before they become problems
6. Understand cost/token burn across all active workstreams

The current single-pane TUI cannot handle this effectively.

### Goals

1. **Multi-pane layout** - View multiple workstreams simultaneously
2. **Real-time streaming** - See live output from all active tasks
3. **Attention routing** - Highlight tasks needing user input
4. **Event visibility** - Surface coordination events between tasks
5. **Fluid navigation** - Quick keyboard-driven task switching
6. **Visual density** - btop-level information density
7. **Scalable** - Handle 20+ workstreams without degrading UX

### Non-Goals

1. **tmux integration** - We're building native TUI, not wrapping tmux
2. **Web UI** - Terminal-only for this design
3. **Remote/distributed workers** - Local daemon focus
4. **Plugin architecture** - Can extend later
5. **Full IDE replacement** - This is orchestration, not editing

## Proposed Solution

### Overview

A multi-pane TUI with configurable layouts, inspired by btop's visual fidelity and Gas Town's operational model. The key insight is treating workstreams like "channels" you can tune into, with a sidebar showing all channels and a main view showing the currently selected workstream(s).

### Architecture

```
╭─ Workstreams ─────────╮╭─ Active: auth-feature ────────────────────────────────────╮
│ [1] ● auth-feature    ││ Reading src/auth/jwt.rs...                                 │
│ [2] ● api-refactor    ││                                                            │
│ [3] ? db-migration    ││ I found the JWT validation logic. The issue is in the     │
│ [4] ○ test-coverage   ││ token expiry check. Let me fix this...                     │
│ [5] ◌ docs-update     ││                                                            │
│                       ││ [Edit] src/auth/jwt.rs:45-52                               │
│                       ││ ████████████████████████░░░░░░░░░░ 68%                     │
├─ Stats ───────────────┤╰────────────────────────────────────────────────────────────╯
│ Active: 2  Waiting: 1 │╭─ Secondary: api-refactor ──────────────────────────────────╮
│ Queued: 2  Total: 5   ││ Running tests for the new endpoint structure...            │
│                       ││ $ cargo test api::                                         │
│ Cost: $4.28/hr        ││ test api::handlers::test_list ... ok                       │
│ Budget: ████████░ 68% ││ test api::handlers::test_create ... ok                     │
├─ Events ──────────────┤╰────────────────────────────────────────────────────────────╯
│ 14:32 rebase_required │╭─ Events Timeline ───────────────────────────────────────────╮
│   api-refactor → main ││ 14:35 ● auth-feature started iteration 12                   │
│ 14:31 sync_relayed    ││ 14:34 ↻ api-refactor rebasing against main                  │
│   learning: jwt_exp.. ││ 14:32 ? db-migration waiting: "Schema approach?"            │
│ 14:28 task_completed  ││ 14:31 ⚡ sync: jwt pattern → auth-feature, api-refactor     │
│   docs-setup          ││ 14:28 ✓ docs-setup completed                                │
╰───────────────────────╯╰─────────────────────────────────────────────────────────────╯
 [1-9] Select  [Tab] Cycle  [Enter] Focus  [Space] Toggle Split  [?] Help  [q] Quit
```

### Layout Modes

#### 1. Dashboard Mode (Default)

```
┌─────────┬────────────────────────┐
│ Sidebar │    Primary View        │
│         │                        │
│ (20%)   │       (80%)            │
│         ├────────────────────────┤
│         │    Events Strip        │
│         │       (15%)            │
└─────────┴────────────────────────┘
```

#### 2. Split Mode (Space key)

```
┌─────────┬───────────┬───────────┐
│ Sidebar │ Primary   │ Secondary │
│         │           │           │
│ (15%)   │  (42.5%)  │  (42.5%)  │
│         ├───────────┴───────────┤
│         │    Events Strip       │
└─────────┴───────────────────────┘
```

#### 3. Grid Mode (G key)

```
┌─────────┬───────────┬───────────┐
│ Sidebar │   Task 1  │   Task 2  │
│         ├───────────┼───────────┤
│ (15%)   │   Task 3  │   Task 4  │
│         ├───────────────────────┤
│         │    Events Strip       │
└─────────┴───────────────────────┘
```

#### 4. Focus Mode (Enter key)

```
┌─────────────────────────────────┐
│                                 │
│         Full Task View          │
│                                 │
│            (100%)               │
│                                 │
├─────────────────────────────────┤
│ Minimal Status Bar              │
└─────────────────────────────────┘
```

### Data Model

```rust
/// TUI application state for parallel workstreams.
pub struct ParallelTuiState {
    /// All active workstreams.
    pub workstreams: Vec<Workstream>,
    /// Currently focused workstream index.
    pub primary_focus: usize,
    /// Secondary focus (for split view).
    pub secondary_focus: Option<usize>,
    /// Current layout mode.
    pub layout_mode: LayoutMode,
    /// Event timeline (recent coordination events).
    pub event_timeline: VecDeque<TimelineEvent>,
    /// Global stats.
    pub stats: GlobalStats,
    /// Budget info from CostTracker.
    pub budget: BudgetStatus,
    /// Notification queue.
    pub notifications: VecDeque<Notification>,
    /// Whether help overlay is shown.
    pub show_help: bool,
    /// Filter state.
    pub filter: WorkstreamFilter,
    /// Sort order for workstream list.
    pub sort_order: SortOrder,
}

/// Budget status for display.
pub struct BudgetStatus {
    /// Current spend in USD.
    pub current_spend: f64,
    /// Budget limit in USD.
    pub budget_limit: f64,
    /// Percentage used (0.0 - 1.0).
    pub usage_percent: f64,
    /// Time until budget resets.
    pub reset_in: Option<Duration>,
    /// Whether budget is exceeded.
    pub exceeded: bool,
}

/// Sort order for workstream list.
pub enum SortOrder {
    /// By status (running first, then waiting, etc.)
    Status,
    /// By cost (highest first).
    Cost,
    /// By last activity (most recent first).
    Activity,
    /// By name (alphabetical).
    Name,
}

/// A single workstream being tracked.
pub struct Workstream {
    /// Task ID.
    pub task_id: TaskId,
    /// Short name (derived from first 20 chars of description, snake_cased).
    pub name: String,
    /// Full task description.
    pub description: String,
    /// Current status.
    pub status: TaskStatus,
    /// Live output buffer (VecDeque used as ring buffer, 1000 lines max).
    pub output: VecDeque<OutputLine>,
    /// Progress indicator (0.0-1.0, based on iteration/max_iteration).
    pub progress: f32,
    /// Current iteration.
    pub iteration: u32,
    /// Cost for this workstream in USD.
    pub cost: f64,
    /// Tokens used.
    pub tokens: u64,
    /// Last activity timestamp.
    pub last_activity: DateTime<Utc>,
    /// Whether this workstream needs attention.
    pub needs_attention: bool,
    /// Attention reason (if any).
    pub attention_reason: Option<String>,
    /// Git worktree path (if task has isolation).
    pub worktree_path: Option<PathBuf>,
    /// Time since last activity (for stuck detection).
    pub idle_duration: Duration,
}

impl Workstream {
    /// Generate a short name from task description.
    /// "Implement user authentication" -> "user-auth"
    pub fn name_from_description(desc: &str) -> String {
        desc.split_whitespace()
            .filter(|w| !["the", "a", "an", "to", "for", "and", "or"].contains(&w.to_lowercase().as_str()))
            .take(2)
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .take(20)
            .collect()
    }
}

/// Layout modes for the TUI.
pub enum LayoutMode {
    /// Single primary view with sidebar.
    Dashboard,
    /// Primary + secondary split.
    Split,
    /// 2x2 grid of workstreams.
    Grid,
    /// Full-screen focus on one workstream.
    Focus,
}

/// An event in the timeline.
pub struct TimelineEvent {
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
    /// Event kind icon.
    pub icon: char,
    /// Event description.
    pub description: String,
    /// Related task(s).
    pub task_ids: Vec<TaskId>,
    /// Event importance (for filtering).
    pub importance: EventImportance,
}
```

### API Design

```rust
/// Main TUI application for parallel workstreams.
pub struct ParallelTui {
    /// Application state.
    state: ParallelTuiState,
    /// Terminal wrapper.
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Event receiver from daemon.
    event_rx: mpsc::Receiver<DaemonEvent>,
    /// Command sender to daemon.
    cmd_tx: mpsc::Sender<TuiCommand>,
}

impl ParallelTui {
    /// Create a new TUI connected to the daemon.
    pub async fn connect(socket_path: &Path) -> Result<Self>;

    /// Run the TUI main loop.
    pub async fn run(&mut self) -> Result<()>;

    /// Handle keyboard input.
    fn handle_input(&mut self, key: KeyEvent);

    /// Process daemon events.
    fn process_daemon_event(&mut self, event: DaemonEvent);

    /// Render the current state.
    fn render(&self, frame: &mut Frame);
}

/// Events from daemon to TUI.
pub enum DaemonEvent {
    /// Task output line.
    TaskOutput { task_id: TaskId, line: String },
    /// Task status changed.
    TaskStatusChanged { task_id: TaskId, status: TaskStatus },
    /// Coordination event.
    CoordinationEvent(Event),
    /// Stats update.
    StatsUpdate(GlobalStats),
    /// Task needs attention.
    AttentionRequired { task_id: TaskId, reason: String },
}

/// Commands from TUI to daemon.
pub enum TuiCommand {
    /// Send user input to task.
    SendInput { task_id: TaskId, input: String },
    /// Pause a task.
    PauseTask(TaskId),
    /// Resume a task.
    ResumeTask(TaskId),
    /// Cancel a task.
    CancelTask(TaskId),
    /// Create a new task.
    CreateTask { description: String, priority: u8 },
}
```

### Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `1-9` | Jump to workstream by index |
| `j/k` or `↓/↑` | Navigate workstream list |
| `Tab` | Cycle focus between visible panes |
| `Enter` | Toggle focus mode on selected workstream |
| `Space` | Toggle split view |
| `g` | Toggle grid view |
| `d` | Dashboard view (default) |
| `a` | Attach to selected workstream (input mode) |
| `p` | Pause/resume selected workstream |
| `x` | Cancel selected workstream (with confirm) |
| `n` | New workstream dialog |
| `e` | Toggle events panel expansion |
| `f` | Filter workstreams dialog |
| `/` | Search workstreams |
| `?` | Help overlay |
| `q` | Quit (with confirm if tasks running) |
| `Ctrl+C` | Force quit |
| `t` | Show task-specific events for selected workstream |
| `b` | Show budget status overlay |
| `r` | Refresh all workstreams |
| `s` | Sort workstreams (cycle: status, cost, activity) |

### Visual Design

#### Status Icons

| Status | Icon | Color |
|--------|------|-------|
| Running | `●` | Green |
| WaitingForUser | `?` | Yellow (pulsing) |
| Queued | `○` | White |
| Blocked | `◌` | Red |
| Paused | `◑` | Yellow |
| Completed | `✓` | Green |
| Failed | `✗` | Red |
| Cancelled | `⊘` | Gray |

#### Event Icons

Based on `EventKind` in `coordination/events.rs`:

| Event | Icon | Description |
|-------|------|-------------|
| TaskStarted | `●` | Task began execution |
| TaskCompleted | `✓` | Task finished successfully |
| TaskFailed | `✗` | Task encountered error |
| TaskCancelled | `⊘` | Task was cancelled |
| TaskPaused | `◑` | Task was paused by watcher |
| MainUpdated | `↑` | Main branch has new commits |
| RebaseRequired | `↻` | Task needs to rebase |
| RebaseCompleted | `✓↻` | Rebase succeeded |
| RebaseConflict | `⚠` | Rebase has conflicts |
| NudgeSent | `→` | Watcher sent nudge to task |
| SyncRelayed | `⚡` | Learning synced between tasks |
| LearningExtracted | `💡` | New learning captured |
| LockAcquired | `🔒` | Resource lock taken |
| LockReleased | `🔓` | Resource lock released |
| SupervisionDegraded | `⚠` | Watcher in degraded mode |
| SupervisionRecovered | `✓` | Watcher recovered |

#### Color Scheme

```rust
const COLORS: ColorScheme = ColorScheme {
    // Status colors
    running: Color::Rgb(0, 255, 127),      // Spring green
    waiting: Color::Rgb(255, 215, 0),       // Gold
    queued: Color::Rgb(169, 169, 169),      // Dark gray
    blocked: Color::Rgb(255, 69, 0),        // Red-orange
    completed: Color::Rgb(50, 205, 50),     // Lime green
    failed: Color::Rgb(220, 20, 60),        // Crimson

    // UI elements
    border_active: Color::Rgb(0, 191, 255), // Deep sky blue
    border_inactive: Color::Rgb(70, 70, 70),
    text_primary: Color::Rgb(240, 240, 240),
    text_secondary: Color::Rgb(150, 150, 150),
    highlight: Color::Rgb(65, 105, 225),    // Royal blue

    // Budget gauge gradient
    budget_low: Color::Rgb(50, 205, 50),    // Green
    budget_mid: Color::Rgb(255, 215, 0),    // Yellow
    budget_high: Color::Rgb(255, 69, 0),    // Red
};
```

### Implementation Plan

**Phase 1: Core Infrastructure**
- Implement `ParallelTuiState` and `Workstream` types
- Add daemon event streaming via Unix socket (extend existing protocol in `daemon.rs`)
- Create output buffer with VecDeque (acts as ring buffer with `pop_front` when full)
- Basic layout with sidebar and single primary view
- Daemon-side: Add `TuiSubscribe` request type to stream task output

**Phase 2: Layout Modes**
- Implement Dashboard mode with events strip
- Add Split mode with secondary view
- Implement Grid mode (2x2)
- Add Focus mode (full-screen)
- Keyboard shortcuts for mode switching
- Layout persistence in user config

**Phase 3: Real-Time Features**
- Live output streaming to visible panes
- Event timeline with coordination events from EventBus
- Attention highlighting:
  - 30s idle: subtle indicator
  - 60s idle: yellow highlight
  - 2min idle: pulsing animation
  - WaitingForUser: immediate pulsing
- Progress bars with smooth animation
- Budget gauge integration with CostTracker

**Phase 4: Interaction**
- Input mode for sending to workstreams (`a` key)
- Workstream creation dialog with git repo selection
- Filter/search functionality (by status, tags, name)
- Confirmation dialogs for destructive actions
- Task-specific event viewer (`t` key)

**Phase 5: Polish**
- btop-level visual refinements
- Braille sparklines for API rate and token burn
- Gradient progress bars (green→yellow→red for budget)
- Smooth animations and transitions
- Performance optimization for 20+ workstreams
- Accessibility: High-contrast mode, screen reader hints

**Phase 6: Daemon Integration**
- Extend `daemon.rs` with TUI-specific notification channel
- Add output capture to `SupervisedExecutor` for streaming
- Integrate with `CostTracker` for real-time budget display
- Add reconnection logic for TUI disconnects

## Alternatives Considered

### Alternative 1: tmux Wrapper

- **Description:** Use tmux as the multi-pane backend, similar to Gas Town
- **Pros:** Proven solution, tmux handles pane management, detach/reattach
- **Cons:** Adds dependency, pushes complexity to user, less integrated experience
- **Why not chosen:** We want a single-binary experience without requiring tmux knowledge

### Alternative 2: Multiple Terminal Windows

- **Description:** Spawn separate terminal windows per workstream
- **Pros:** Simple, native OS window management
- **Cons:** Cluttered, no unified view, hard to orchestrate
- **Why not chosen:** Defeats the purpose of unified orchestration

### Alternative 3: Web UI

- **Description:** Browser-based dashboard instead of TUI
- **Pros:** Richer visuals, easier multi-pane layouts, mobile access
- **Cons:** Different tech stack, additional process, latency
- **Why not chosen:** Terminal-native aligns with developer workflow, can add web UI later

### Alternative 4: Minimal Status-Only TUI

- **Description:** Just show status, use `np attach` for individual tasks
- **Pros:** Simple, less code, existing pattern
- **Cons:** Context switching overhead, can't see multiple outputs
- **Why not chosen:** Doesn't solve the parallel monitoring problem

## Technical Considerations

### Dependencies

```toml
[dependencies]
ratatui = { version = "0.28", features = ["all-widgets"] }
crossterm = { version = "0.28", features = ["event-stream"] }
tokio = { version = "1", features = ["sync", "time", "net"] }

# For smooth animations and unicode width
unicode-width = "0.1"
```

### Integration with Existing Components

#### Relationship to `ui/tui.rs`

The existing `src/ui/tui.rs` provides a basic single-task TUI. The parallel TUI will:
1. **Extend, not replace:** Keep existing TUI for simple single-task use
2. **Share types:** Reuse `AppState`, `Notification`, visual helpers
3. **New module:** Create `src/ui/parallel_tui.rs` for multi-workstream logic
4. **CLI integration:** `np tui` launches parallel TUI, `np attach <id>` uses existing attach flow

```rust
// src/ui/mod.rs
pub mod notifications;
pub mod tui;           // Existing single-task TUI
pub mod parallel_tui;  // New parallel workstream TUI

// CLI decides which to use
match daemon.task_count().await {
    0 => println!("No tasks running"),
    1 => tui::run_single_task_tui(daemon).await?,
    _ => parallel_tui::run(daemon).await?,
}
```

#### EventBus Integration

The TUI subscribes to the daemon's EventBus for real-time updates:

```rust
impl ParallelTui {
    async fn setup_subscriptions(&mut self) {
        // Subscribe to all coordination events
        let event_sub = self.event_bus.subscribe_to(vec![
            EventKind::TaskStarted,
            EventKind::TaskCompleted,
            EventKind::TaskFailed,
            EventKind::TaskCancelled,
            EventKind::TaskPaused,
            EventKind::MainUpdated,
            EventKind::RebaseRequired,
            EventKind::SyncRelayed,
            EventKind::NudgeSent,
        ]);

        // Subscribe to all task output
        let output_sub = self.output_channel.subscribe();

        // Process in select! loop
        tokio::select! {
            Some(event) = event_sub.recv() => self.handle_event(event),
            Some(output) = output_sub.recv() => self.handle_output(output),
            Some(key) = self.input_rx.recv() => self.handle_key(key),
        }
    }
}
```

#### Attach Workflow

The `np attach <id>` workflow remains for focused single-task interaction:
- From parallel TUI: Press `Enter` on a task to enter Focus mode, then `a` to attach
- Attach takes over the terminal, TUI state is preserved
- `Ctrl+D` or `/detach` returns to TUI in previous layout mode

#### Future: Federation Preparation

While federation (remote workers) is out of scope, the architecture should not preclude it:
- `Workstream` has optional `remote_host: Option<String>` field (unused now)
- Protocol can extend with `RemoteTask` variants
- TUI state can merge workstreams from multiple daemons
- Phase 6+ could add `np tui --remote host:port` support

### Performance

- **Output buffering:** VecDeque per workstream (1000 lines default, configurable)
- **Render throttling:** 30 FPS cap to prevent CPU burn
- **Event batching:** Batch daemon events before render (100ms window)
- **Lazy rendering:** Only render visible workstreams
- **Memory:** ~1MB per workstream (output buffer + metadata)
- **Output rate limiting:** If task outputs >100 lines/sec, sample to prevent flood

### Edge Cases and Error Handling

#### Daemon Disconnection
```rust
/// Handle daemon socket disconnection.
async fn handle_disconnect(&mut self) {
    // 1. Show "Disconnected" status bar
    self.state.connection_status = ConnectionStatus::Disconnected;

    // 2. Attempt reconnection with exponential backoff
    let mut delay = Duration::from_millis(100);
    for attempt in 1..=5 {
        tokio::time::sleep(delay).await;
        if let Ok(stream) = UnixStream::connect(&self.socket_path).await {
            self.reconnect(stream).await;
            return;
        }
        delay *= 2;
    }

    // 3. After 5 attempts, show error and offer manual retry
    self.show_reconnect_dialog();
}
```

#### Terminal Size Constraints
- **Minimum size:** 80x24 characters
- **Below minimum:** Show "Terminal too small" message with required size
- **Dynamic resize:** Re-layout on SIGWINCH, debounced 100ms
- **Grid mode fallback:** If <120 cols, force Dashboard mode

#### High Output Volume
- **Rate limiting:** Sample output if >100 lines/sec per workstream
- **Indicator:** Show "Output rate limited" badge when sampling
- **Full log:** Point user to `np logs <task-id>` for complete output

#### Unicode and Terminal Compatibility
```rust
/// Detect terminal capabilities on startup.
pub struct TerminalCaps {
    /// True color support (24-bit).
    pub true_color: bool,
    /// Unicode support level.
    pub unicode: UnicodeLevel,
    /// Terminal width calculation.
    pub wcwidth_available: bool,
}

pub enum UnicodeLevel {
    /// Full unicode with emoji.
    Full,
    /// Basic unicode (no emoji, use ASCII fallbacks).
    Basic,
    /// ASCII only.
    Ascii,
}

impl TerminalCaps {
    pub fn detect() -> Self {
        let true_color = std::env::var("COLORTERM")
            .map(|v| v == "truecolor" || v == "24bit")
            .unwrap_or(false);

        // Check for common terminal emulators with good unicode
        let term = std::env::var("TERM").unwrap_or_default();
        let unicode = if term.contains("256color") || term.contains("kitty") {
            UnicodeLevel::Full
        } else if term.contains("xterm") {
            UnicodeLevel::Basic
        } else {
            UnicodeLevel::Ascii
        };

        Self {
            true_color,
            unicode,
            wcwidth_available: true, // Use unicode-width crate
        }
    }
}
```

#### Color Fallbacks
- **True color:** Use full RGB palette
- **256-color:** Map to nearest 256-color palette
- **16-color:** Use basic ANSI colors
- **No color:** Respect `NO_COLOR` env var, use bold/dim instead

### Daemon Communication

The TUI connects to the daemon via Unix socket and uses a bidirectional protocol:

```rust
// TUI → Daemon
#[derive(Serialize)]
pub enum TuiRequest {
    Subscribe { filter: Option<EventFilter> },
    SendInput { task_id: String, input: String },
    PauseTask { task_id: String },
    ResumeTask { task_id: String },
    CancelTask { task_id: String },
    CreateTask { description: String, priority: u8 },
}

// Daemon → TUI
#[derive(Deserialize)]
pub enum TuiNotification {
    TaskOutput { task_id: String, line: String },
    TaskStatusChanged { task_id: String, status: String },
    Event(coordination::Event),
    Stats(GlobalStats),
    AttentionRequired { task_id: String, reason: String },
}
```

### Testing Strategy

1. **Unit tests:** State transitions, layout calculations, color gradients
2. **Snapshot tests:** Render output for various states
3. **Integration tests:** Daemon communication, event handling
4. **Manual testing:** Real workloads with 5, 10, 20 workstreams
5. **Stress tests:** Rapid output, many events, resize handling

### Rollout Plan

1. Add as `np tui` command alongside existing `np attach`
2. Feature flag for parallel mode vs single-task mode
3. Default to parallel mode when daemon has 3+ tasks
4. Collect feedback, iterate on UX

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Terminal compatibility issues | Medium | Medium | Test on common terminals (iTerm, Alacritty, Kitty, default Terminal.app) |
| Performance with many workstreams | Medium | High | Ring buffers, render throttling, lazy updates |
| Complex codebase | High | Medium | Clean separation of state/render/input, comprehensive tests |
| User learning curve | Medium | Low | Intuitive vim-like shortcuts, good help overlay |
| Daemon socket stability | Low | High | Reconnect logic, graceful degradation |

## Open Questions

- [ ] Should grid mode support 3x3 or stay at 2x2?
- [ ] Should we support mouse input for pane selection?
- [ ] How to handle terminal resize mid-render?
- [ ] Should output scroll position be per-workstream or global?
- [ ] Should we persist TUI preferences (layout mode, colors)?
- [ ] Should we show git diff preview for completed tasks?
- [ ] How should budget warnings be displayed (overlay vs inline)?

## Quick Start for Implementers

If you're implementing this design, start here:

1. **Phase 1 first:** Get `ParallelTuiState` and `Workstream` types compiling
2. **Add daemon subscription:** Extend `daemon.rs` with `TuiSubscribe` request
3. **Basic render:** Sidebar + single primary view, no interaction
4. **Add keyboard:** Navigation (j/k), quit (q), help (?)
5. **Iterate:** Add layout modes one at a time

Key files to modify:
- `src/ui/parallel_tui.rs` (new)
- `src/ui/mod.rs` (add export)
- `src/daemon.rs` (add TUI notification channel)
- `src/main.rs` (add `np tui` command)

## References

- `src/ui/tui.rs` - Existing single-task TUI implementation
- `src/coordination/events.rs` - EventKind definitions
- `src/cost.rs` - CostTracker for budget integration
- `docs/neuraphage-infrastructure.md` - TUI visual design guidelines
- `docs/yegge/welcome-to-gas-town.md` - Gas Town tmux-based UI inspiration
- [btop](https://github.com/aristocratos/btop) - Visual fidelity reference
- [bottom](https://github.com/ClementTsang/bottom) - ratatui implementation reference
- [ratatui docs](https://ratatui.rs) - Widget and layout documentation

---

## Review Log

### Pass 1: Completeness (2026-01-11)
- Added budget enforcement integration
- Added workstream naming algorithm
- Added task-specific events hotkey
- Expanded implementation plan with daemon-side changes
- Added accessibility considerations

### Pass 2: Correctness (2026-01-11)
- Fixed RingBuffer to use std VecDeque
- Added BudgetStatus and SortOrder types
- Aligned event icons with coordination/events.rs EventKind
- Added budget percentage calculation (usage vs limit)

### Pass 3: Edge Cases (2026-01-11)
- Added daemon disconnection handling with reconnect
- Added terminal size constraints and fallbacks
- Added high output volume rate limiting
- Added unicode/color terminal capability detection

### Pass 4: Architecture (2026-01-11)
- Clarified relationship to existing ui/tui.rs
- Added EventBus subscription integration
- Documented attach workflow interaction
- Added federation preparation notes

### Pass 5: Clarity (2026-01-11)
- Added Quick Start section for implementers
- Updated review passes counter
- Added key file references
- Document converged — no significant changes needed

**Final Status:** Document complete after 5 passes.
