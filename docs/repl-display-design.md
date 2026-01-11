# Design Document: REPL Dynamic Terminal Display

**Author:** Claude
**Date:** 2026-01-11
**Status:** Implemented
**Review Passes:** 5/5

## Summary

Implement a Claude Code-style terminal display for the neuraphage REPL that shows streaming LLM output in a scrolling area while maintaining a persistent status bar (tokens, cost, activity) at the bottom. This eliminates the current display garbling where status updates interrupt streaming text.

## Problem Statement

### Background

Claude Code achieves a polished terminal UX using React Ink, which provides component-based UI rendering with the `<Static>` component for persistent areas. Their renderer was eventually rewritten from scratch for finer-grained control, but the core pattern remains: scrolling content above, persistent status below.

Currently, neuraphage's REPL uses raw `print!()` and `println!()` calls with manual `mid_line` tracking. Status updates (`IterationComplete`, `ActivityChanged`) interrupt streaming text, causing garbled output where status lines appear mid-sentence.

### Problem

The current display has three issues:
1. **Garbled output**: Status lines print during streaming, breaking text flow
2. **No visual separation**: Status and content are indistinguishable
3. **Poor UX**: Users can't easily track progress while reading output

### Goals

- Clean separation between streaming content and status information
- Status bar that updates in-place without disrupting content
- Smooth streaming text display
- Professional appearance matching Claude Code's polish

### Non-Goals

- Full TUI dashboard (that's `np tui`, not REPL mode)
- Multiple concurrent task views
- Mouse support
- Custom themes or colors beyond what we have

## Proposed Solution

### Overview

Use **ratatui's Inline Viewport** to create a fixed-height status region at the bottom of the terminal while streaming content scrolls above it. This is the Rust equivalent of Ink's `<Static>` component pattern.

### Architecture

```
Terminal (normal scrolling area)
┌─────────────────────────────────────────────────────────────┐
│ > what is the borrow checker in Rust?                       │ ← User input (scrolls up)
│                                                             │
│ The borrow checker is one of Rust's most important features.│ ← Streaming LLM output
│ It's a compile-time analysis system that ensures memory     │   (scrolls up naturally)
│ safety without requiring garbage collection...              │
│                                                             │
│ ## How It Works                                             │
│ The borrow checker enforces ownership rules:                │
│ 1. Each value has exactly one owner                         │
│ ...                                                         │
├─────────────────────────────────────────────────────────────┤
│ ● Iteration 1 │ Tokens: 3,736 │ Cost: $0.0193 │ Streaming   │ ← Status bar (fixed)
└─────────────────────────────────────────────────────────────┘
```

### Three Implementation Options

#### Option A: Ratatui Inline Viewport (Recommended)

Uses ratatui's `Viewport::Inline(height)` to reserve bottom lines for status while content flows above.

```rust
use ratatui::{Terminal, TerminalOptions, Viewport};
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::Paragraph;
use ratatui::style::{Style, Color};
use crossterm::terminal::{enable_raw_mode, disable_raw_mode};
use std::io::stdout;

// Enable raw mode for terminal control
enable_raw_mode()?;

// Reserve 1 line for status bar (inline viewport)
let backend = CrosstermBackend::new(stdout());
let mut terminal = Terminal::with_options(
    backend,
    TerminalOptions { viewport: Viewport::Inline(1) }
)?;

// Stream content above status (scrolls naturally in terminal)
// insert_before takes line count and a closure that renders to a buffer
terminal.insert_before(1, |buf| {
    Paragraph::new(text_chunk).render(buf.area, buf);
})?;

// Redraw status bar (stays fixed at bottom of inline viewport)
terminal.draw(|frame| {
    let elapsed = format_duration(status.elapsed);
    let status_text = format!(
        "● {} │ Iter {} │ {} tokens │ ${:.4} │ {}",
        activity_icon(&status.activity),
        status.iteration,
        format_number(status.tokens_used),
        status.cost,
        elapsed
    );
    let para = Paragraph::new(status_text)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(para, frame.area());
})?;

// On cleanup
disable_raw_mode()?;
```

**Pros:**
- Already have ratatui as dependency
- Clean separation of concerns
- Battle-tested approach (used by many Rust CLIs)
- Can evolve to full TUI later

**Cons:**
- Learning curve for ratatui patterns
- Slightly more complex than raw printing

#### Option B: Indicatif MultiProgress

Uses indicatif's progress bar system with `println()` for content above.

```rust
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

let mp = MultiProgress::new();
let status_bar = mp.add(ProgressBar::new_spinner());
status_bar.set_style(ProgressStyle::default_spinner()
    .template("{spinner} Iter {msg}")?);

// Print content (appears above progress bars)
mp.println("The borrow checker is...")?;

// Update status (redraws in place)
status_bar.set_message(format!("{} │ {} tokens │ ${:.4}", iter, tokens, cost));
```

**Pros:**
- Simple API
- Designed for this exact use case
- Thread-safe by default

**Cons:**
- New dependency
- Less flexible than ratatui
- Can't easily evolve to rich TUI

#### Option C: Raw ANSI Scroll Regions

Use ANSI escape sequences to set scroll margins, reserving bottom line.

```rust
use crossterm::{execute, terminal, cursor};

// Set scroll region to exclude bottom 2 lines
let (_, rows) = terminal::size()?;
execute!(stdout(), terminal::SetScrollRegion(0, rows - 2))?;

// Print content (scrolls within region)
println!("The borrow checker is...");

// Update status (direct cursor positioning)
execute!(stdout(), cursor::SavePosition)?;
execute!(stdout(), cursor::MoveTo(0, rows - 1))?;
print!("● Iter {} │ Tokens: {} │ ${:.4}", iter, tokens, cost);
execute!(stdout(), cursor::RestorePosition)?;
```

**Pros:**
- No new dependencies (crossterm already present)
- Maximum control
- Lowest overhead

**Cons:**
- Manual cursor management
- Terminal compatibility issues (especially Windows)
- Error-prone
- Hardest to maintain

### Recommended Approach: Option A (Ratatui Inline)

Ratatui Inline Viewport is the best choice because:
1. We already have ratatui as a dependency
2. It handles terminal quirks (resize, alternate screen restoration)
3. Natural upgrade path to full TUI when needed
4. Well-documented with active community

### Data Model

New display state structure:

```rust
/// REPL display state for inline viewport mode.
pub struct ReplDisplay {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// Current status to render
    status: StatusState,
    /// Whether we're actively streaming
    streaming: bool,
}

#[derive(Default)]
pub struct StatusState {
    pub iteration: u32,
    pub tokens_used: u64,
    pub cost: f64,
    pub activity: Activity,
    pub elapsed: std::time::Duration,
    pub task_started: Option<std::time::Instant>,
}

pub enum Activity {
    Idle,
    Thinking,
    Streaming,
    ExecutingTool(String),
    WaitingForUser,
}
```

### API Design

```rust
impl ReplDisplay {
    /// Create new display with inline viewport.
    /// Returns simple fallback if stdout is not a TTY.
    pub fn new() -> Result<Self>;

    /// Check if we're in inline mode (TTY) or fallback mode.
    pub fn is_inline(&self) -> bool;

    /// Print streaming content (scrolls above status).
    /// In fallback mode, just prints to stdout.
    pub fn print_content(&mut self, text: &str) -> Result<()>;

    /// Update status bar (redraws in place).
    /// In fallback mode, this is a no-op (status shown at end).
    pub fn update_status(&mut self, status: StatusState) -> Result<()>;

    /// Print a complete line (e.g., tool output).
    pub fn println(&mut self, line: &str) -> Result<()>;

    /// Show final status summary (for fallback mode).
    pub fn print_summary(&mut self) -> Result<()>;

    /// Cleanup and restore terminal.
    pub fn cleanup(&mut self) -> Result<()>;
}

/// Fallback display for non-TTY environments.
pub struct FallbackDisplay {
    status: StatusState,
}

impl FallbackDisplay {
    pub fn print_content(&mut self, text: &str) {
        print!("{}", text);
    }

    pub fn println(&mut self, line: &str) {
        println!("{}", line);
    }

    pub fn print_summary(&self) {
        eprintln!(
            "\n[Completed: {} iterations, {} tokens, ${:.4}, {:?}]",
            self.status.iteration,
            self.status.tokens_used,
            self.status.cost,
            self.status.elapsed
        );
    }
}
```

### Implementation Plan

**Phase 1: ReplDisplay Module**
- Create `src/repl/display.rs` with `ReplDisplay` struct
- Initialize ratatui with `Viewport::Inline(1)`
- Implement `print_content()` using `insert_before()`
- Implement `update_status()` using `draw()`
- Add cleanup on Drop trait impl
- Add `is_terminal()` detection for fallback mode

**Phase 2: Integrate with REPL**
- Modify `Repl` struct to own a `ReplDisplay`
- Replace `handle_event()` print calls with `ReplDisplay` methods:
  - `TextDelta` → `display.print_content()`
  - `IterationComplete` → `display.update_status()`
  - `ToolStarted/Completed` → `display.println()`
  - `Completed/Failed` → `display.print_summary()` + cleanup
- Update `wait_for_task()` to create display at start, cleanup at end
- Register Ctrl+C handler that calls `display.cleanup()`
- Add panic hook for raw mode cleanup

**Phase 3: Status Bar Styling**
- Color coding:
  - Green `●` for streaming/success
  - Blue `●` for thinking
  - Yellow `⚙` for tool execution
  - Red `✗` for errors
- Spinner character rotation for active states: `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`
- Format tokens with commas: `3,736`
- Format elapsed time: `12s`, `1m 23s`, `1h 2m`

**Phase 4: Polish**
- Handle terminal resize (ratatui does this automatically)
- Test on: Linux (gnome-terminal, kitty, alacritty), macOS (Terminal, iTerm2), Windows Terminal
- Add fallback for non-TTY: detect with `std::io::stdout().is_terminal()`
- Add `--no-tui` flag to force fallback mode

### Integration with Existing Code

Current `repl.rs` structure:
```
Repl::run()
  └── loop
        ├── read input
        ├── parse (ReplInput)
        └── match
              ├── Command → handle_command()
              └── Message → send_message()
                              └── wait_for_task()
                                    └── handle_event() ← Display changes here
```

New structure:
```
Repl::run()
  └── loop
        ├── read input (normal stdin, no raw mode yet)
        ├── parse (ReplInput)
        └── match
              ├── Command → handle_command()
              └── Message → send_message()
                              ├── ReplDisplay::new() ← Enter raw mode, inline viewport
                              ├── wait_for_task()
                              │     └── handle_event()
                              │           ├── TextDelta → display.print_content()
                              │           ├── IterationComplete → display.update_status()
                              │           └── ...
                              └── display.cleanup() ← Exit raw mode (also via Drop)
```

Key insight: Only enter raw mode during task execution, not during input.

## Alternatives Considered

### Alternative 1: Keep Raw Printing (Current Approach)

**Description:** Continue with current `print!()` approach, just suppress status during streaming.

**Pros:**
- No code changes needed
- Simple to understand

**Cons:**
- No status visibility during streaming
- Can't show progress while LLM generates
- Poor UX

**Why not chosen:** Users need to see progress during long generations.

### Alternative 2: Alternate Screen

**Description:** Use terminal alternate screen (like vim/nano do) for entire REPL.

**Pros:**
- Clean slate, full control
- No scroll history interference

**Cons:**
- Loses scroll history
- Can't see previous commands
- Feels disconnected from shell

**Why not chosen:** REPL should feel integrated with terminal, not isolated.

### Alternative 3: Full Ratatui TUI

**Description:** Use full-screen ratatui TUI even for REPL mode.

**Pros:**
- Maximum visual control
- Consistent with `np tui` mode

**Cons:**
- Overkill for simple REPL
- Loses terminal scroll history
- Higher complexity

**Why not chosen:** Save full TUI for `np tui`; REPL should be lightweight.

## Technical Considerations

### Dependencies

Already have:
- `ratatui = "0.29"`
- `crossterm = "0.29"`

No new dependencies required.

### Performance

- `insert_before()` is O(1) for text chunks
- Status bar redraws at most once per event (~10-100ms intervals)
- No noticeable overhead vs raw printing

### Security

No security implications - purely display logic.

### Testing Strategy

- Unit tests for `StatusState` formatting
- Manual testing on:
  - Linux (various terminal emulators)
  - macOS Terminal.app and iTerm2
  - Windows Terminal
- Test edge cases:
  - Very long lines (wrapping)
  - Rapid updates
  - Terminal resize during streaming
  - Ctrl+C during streaming

### Rollout Plan

1. Implement behind feature flag initially
2. Test with real LLM streaming
3. Enable by default once stable
4. Keep fallback for non-TTY environments

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Terminal compatibility issues | Medium | Medium | Test on major terminals; fallback to raw mode |
| Ratatui inline viewport bugs | Low | Medium | Pin version; report upstream issues |
| Performance overhead | Low | Low | Batch updates; profile if needed |
| Cleanup failures (corrupted terminal) | Medium | High | Register panic hook; use Drop impl; Ctrl+C handler |
| Non-TTY environments (CI, pipes) | Medium | Low | Detect with `std::io::stdout().is_terminal()`; use fallback |
| Panic during raw mode | Medium | High | Custom panic hook that calls `disable_raw_mode()` first |
| Terminal resize during streaming | Medium | Low | Ratatui handles resize; status bar adapts to width |
| Very long status text | Low | Low | Truncate to terminal width with `...` |
| SSH disconnect | Low | Medium | Raw mode cleanup in signal handlers |

## Open Questions

- [x] Which ratatui features to enable? → `all-widgets` already enabled
- [x] Should status bar be 1 or 2 lines? → 1 line (minimal footprint, matches Claude Code)
- [x] Include elapsed time in status? → Yes, format as `12s`, `1m 23s`
- [x] Show tool name during execution? → Yes, e.g., `⚙ read_file`

## References

- [How Claude Code is built](https://newsletter.pragmaticengineer.com/p/how-claude-code-is-built) - Ink architecture details
- [Ink GitHub](https://github.com/vadimdemedes/ink) - React for CLIs
- [Ratatui Inline Viewport](https://ratatui.rs/examples/apps/inline/) - Example code
- [indicatif MultiProgress](https://docs.rs/indicatif/latest/indicatif/struct.MultiProgress.html) - Alternative approach
- [ANSI Scroll Regions](https://gist.github.com/ConnerWill/d4b6c776b509add763e17f9f113fd25b) - Low-level escape sequences
- [crossterm](https://github.com/crossterm-rs/crossterm) - Cross-platform terminal library
