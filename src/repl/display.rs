//! REPL display module for terminal output.
//!
//! Two-level architecture:
//! 1. ReplScreen - manages alternate screen buffer for entire REPL session
//! 2. ReplDisplay - manages raw mode and streaming for individual tasks
//!
//! Uses ANSI scroll regions to create a fixed status bar at the bottom
//! while streaming content scrolls above it.

use std::io::{IsTerminal, Write, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::cursor::{MoveToColumn, MoveToRow};
use crossterm::execute;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

use crate::error::Result;

/// Global flag tracking if alternate screen is active (for panic hook).
static ALT_SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Global flag tracking if raw mode is enabled (for panic hook).
static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Install panic hook to restore terminal state on panic.
fn install_panic_hook() {
    static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);
    if HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return; // Already installed
    }

    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal state
        if RAW_MODE_ACTIVE.load(Ordering::SeqCst) {
            let _ = disable_raw_mode();
            RAW_MODE_ACTIVE.store(false, Ordering::SeqCst);
        }
        if ALT_SCREEN_ACTIVE.load(Ordering::SeqCst) {
            let _ = execute!(stdout(), LeaveAlternateScreen, ResetColor);
            ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);
        }
        original_hook(panic_info);
    }));
}

/// REPL screen manager - handles alternate screen for entire REPL session.
/// Create at REPL start, drop (or call leave()) at REPL exit.
pub struct ReplScreen {
    is_tty: bool,
    rows: u16,
    cols: u16,
}

impl ReplScreen {
    /// Enter the REPL screen (alternate buffer with scroll region).
    pub fn enter() -> Result<Self> {
        let is_tty = stdout().is_terminal();
        let (cols, rows) = terminal::size().unwrap_or((80, 24));

        if is_tty {
            install_panic_hook();

            let mut out = stdout();
            // Enter alternate screen buffer (like vim, htop, etc.)
            execute!(out, EnterAlternateScreen)?;
            ALT_SCREEN_ACTIVE.store(true, Ordering::SeqCst);

            // Set scroll region to exclude bottom line for status bar
            write!(out, "\x1b[1;{}r", rows - 1)?;
            // Move cursor to top
            execute!(out, MoveToRow(0), MoveToColumn(0))?;
            out.flush()?;
        }

        Ok(Self { is_tty, rows, cols })
    }

    /// Print welcome message with logo.
    pub fn print_welcome(&self) -> Result<()> {
        if self.is_tty {
            let mut out = stdout();

            // Display the braille logo
            let logo = include_str!("../../art/neuraphage-logo.txt");
            for line in logo.lines() {
                execute!(
                    out,
                    SetForegroundColor(Color::Cyan),
                    Print(line),
                    ResetColor,
                    Print("\r\n")
                )?;
            }

            execute!(
                out,
                Print("\r\nType a message to start, or "),
                SetForegroundColor(Color::Yellow),
                Print("/help"),
                ResetColor,
                Print(" for help\r\n\r\n")
            )?;
            out.flush()?;
        }
        Ok(())
    }

    /// Print the input prompt.
    pub fn print_prompt(&self) -> Result<()> {
        if self.is_tty {
            let mut out = stdout();
            execute!(out, SetForegroundColor(Color::Cyan), Print("> "), ResetColor)?;
            out.flush()?;
        }
        Ok(())
    }

    /// Leave the REPL screen (restore original terminal).
    pub fn leave(&mut self) -> Result<()> {
        if self.is_tty && ALT_SCREEN_ACTIVE.load(Ordering::SeqCst) {
            let mut out = stdout();
            // Reset scroll region
            write!(out, "\x1b[r")?;
            // Leave alternate screen buffer
            execute!(out, LeaveAlternateScreen)?;
            out.flush()?;
            ALT_SCREEN_ACTIVE.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Get terminal dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }
}

impl Drop for ReplScreen {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

/// Braille spinner frames for animated activity indicator.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Activity state for the status bar.
#[derive(Debug, Clone, Default)]
pub enum Activity {
    #[default]
    Idle,
    Thinking,
    Streaming,
    ExecutingTool(String),
    WaitingForUser,
}

impl Activity {
    /// Get the spinner char for this activity.
    pub fn icon(&self, frame: usize) -> char {
        match self {
            Activity::Idle => '○',
            Activity::Thinking | Activity::Streaming | Activity::ExecutingTool(_) => {
                SPINNER_FRAMES[frame % SPINNER_FRAMES.len()]
            }
            Activity::WaitingForUser => '?',
        }
    }

    /// Get the color for this activity.
    pub fn color(&self) -> Color {
        match self {
            Activity::Idle => Color::DarkGrey,
            Activity::Thinking => Color::Blue,
            Activity::Streaming => Color::Green,
            Activity::ExecutingTool(_) | Activity::WaitingForUser => Color::Yellow,
        }
    }

    /// Get display text for this activity.
    pub fn text(&self) -> &str {
        match self {
            Activity::Idle => "Idle",
            Activity::Thinking => "Thinking",
            Activity::Streaming => "Streaming",
            Activity::ExecutingTool(name) => name.as_str(),
            Activity::WaitingForUser => "Waiting",
        }
    }

    /// Check if actively working.
    pub fn is_active(&self) -> bool {
        !matches!(self, Activity::Idle | Activity::WaitingForUser)
    }
}

/// Status state for the status bar.
#[derive(Debug, Clone, Default)]
pub struct StatusState {
    pub iteration: u32,
    pub tokens_used: u64,
    pub cost: f64,
    pub activity: Activity,
    pub task_started: Option<Instant>,
}

impl StatusState {
    pub fn elapsed(&self) -> Duration {
        self.task_started.map(|s| s.elapsed()).unwrap_or_default()
    }
}

/// Format duration as human-readable string.
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Format number with commas.
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}

/// REPL display with scroll region for status bar.
pub struct ReplDisplay {
    status: StatusState,
    raw_mode_enabled: bool,
    frame: usize,
    is_tty: bool,
    rows: u16,
    cols: u16,
}

impl ReplDisplay {
    /// Create a new display for streaming.
    /// Only enables raw mode - alternate screen is managed by ReplScreen.
    /// If `prompt` is provided, it will be displayed as "You: {prompt}".
    pub fn new_with_prompt(prompt: Option<&str>) -> Result<Self> {
        let is_tty = stdout().is_terminal();
        let (cols, rows) = terminal::size().unwrap_or((80, 24));

        if is_tty {
            install_panic_hook();
            enable_raw_mode()?;
            RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);

            let mut out = stdout();

            // Display user's prompt if provided
            if let Some(p) = prompt {
                // Truncate long prompts to fit on screen
                let max_len = (cols as usize).saturating_sub(10);
                let display_prompt = if p.len() > max_len {
                    format!("{}...", &p[..max_len.saturating_sub(3)])
                } else {
                    p.to_string()
                };
                execute!(
                    out,
                    SetForegroundColor(Color::Cyan),
                    Print("You: "),
                    ResetColor,
                    Print(&display_prompt),
                    Print("\r\n\r\n")
                )?;
            }

            out.flush()?;
        }

        let display = Self {
            status: StatusState {
                task_started: Some(Instant::now()),
                ..Default::default()
            },
            raw_mode_enabled: is_tty,
            frame: 0,
            is_tty,
            rows,
            cols,
        };

        // Draw initial status bar
        if is_tty {
            display.draw_status_bar()?;
        }

        Ok(display)
    }

    /// Create a new display for streaming (no initial prompt).
    pub fn new() -> Result<Self> {
        Self::new_with_prompt(None)
    }

    pub fn is_inline(&self) -> bool {
        self.is_tty
    }

    /// Print streaming content (scrolls in the scroll region).
    pub fn print_content(&mut self, text: &str) -> Result<()> {
        let mut out = stdout();
        if self.raw_mode_enabled {
            // In raw mode, convert \n to \r\n for proper line breaks
            for c in text.chars() {
                if c == '\n' {
                    write!(out, "\r\n")?;
                } else {
                    write!(out, "{}", c)?;
                }
            }
        } else {
            write!(out, "{}", text)?;
        }
        out.flush()?;
        Ok(())
    }

    /// Print a complete line (handles embedded newlines in raw mode).
    pub fn println(&mut self, line: &str) -> Result<()> {
        let mut out = stdout();
        if self.raw_mode_enabled {
            // Convert any embedded \n to \r\n for raw mode
            for c in line.chars() {
                if c == '\n' {
                    write!(out, "\r\n")?;
                } else {
                    write!(out, "{}", c)?;
                }
            }
            write!(out, "\r\n")?;
        } else {
            writeln!(out, "{}", line)?;
        }
        out.flush()?;
        Ok(())
    }

    /// Update status and redraw status bar.
    /// NOTE: Skips redraw during streaming to avoid cursor position corruption.
    pub fn update_status(&mut self, status: StatusState) -> Result<()> {
        let is_streaming = matches!(status.activity, Activity::Streaming);

        self.status = status;
        self.frame = self.frame.wrapping_add(1);

        // Only redraw status bar when NOT actively streaming
        // The save/restore cursor during streaming corrupts cursor position
        if self.is_tty && !is_streaming {
            self.draw_status_bar()?;
        }
        Ok(())
    }

    /// Draw the status bar on the bottom line.
    fn draw_status_bar(&self) -> Result<()> {
        let mut out = stdout();
        let color = self.status.activity.color();
        let icon = self.status.activity.icon(self.frame);
        let activity = self.status.activity.text();
        let elapsed = format_duration(self.status.elapsed());
        let tokens = format_number(self.status.tokens_used);

        let status_text = format!(
            "{} {} │ Iter {} │ {} tokens │ ${:.4} │ {}",
            icon, activity, self.status.iteration, tokens, self.status.cost, elapsed
        );

        // Truncate if needed
        let display_text: String = status_text.chars().take(self.cols as usize).collect();

        // Save cursor position (using CSI s)
        write!(out, "\x1b[s")?;

        // Move to status bar row (1-indexed for ANSI)
        write!(out, "\x1b[{};1H", self.rows)?;

        // Clear line
        write!(out, "\x1b[2K")?;

        // Set color and print
        execute!(out, SetForegroundColor(color), Print(&display_text), ResetColor,)?;

        // Restore cursor position (using CSI u)
        write!(out, "\x1b[u")?;

        out.flush()?;
        Ok(())
    }

    /// Print final summary.
    pub fn print_summary(&self) -> Result<()> {
        let elapsed = format_duration(self.status.elapsed());
        let tokens = format_number(self.status.tokens_used);
        eprintln!(
            "\n[Completed: {} iterations, {} tokens, ${:.4}, {}]",
            self.status.iteration, tokens, self.status.cost, elapsed
        );
        Ok(())
    }

    /// Cleanup: disable raw mode (alternate screen is managed by ReplScreen).
    pub fn cleanup(&mut self) -> Result<()> {
        if self.raw_mode_enabled {
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
            RAW_MODE_ACTIVE.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Drop for ReplDisplay {
    fn drop(&mut self) {
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
            RAW_MODE_ACTIVE.store(false, Ordering::SeqCst);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(65)), "1m 5s");
        assert_eq!(format_duration(Duration::from_secs(3665)), "1h 1m");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn test_activity_icon() {
        assert_eq!(Activity::Idle.icon(0), '○');
        assert_eq!(Activity::Thinking.icon(0), '⠋');
        assert_eq!(Activity::Thinking.icon(1), '⠙');
    }

    #[test]
    fn test_activity_is_active() {
        assert!(!Activity::Idle.is_active());
        assert!(Activity::Thinking.is_active());
        assert!(Activity::Streaming.is_active());
    }

    #[test]
    fn test_status_state_default() {
        let s = StatusState::default();
        assert_eq!(s.iteration, 0);
        assert_eq!(s.tokens_used, 0);
    }
}
