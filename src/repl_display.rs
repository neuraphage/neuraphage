//! REPL display module for terminal output.
//!
//! Provides streaming text output with a final status summary.
//! Handles raw mode for proper terminal control.

use std::io::{IsTerminal, Write, stdout};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::error::Result;

/// Global flag tracking if raw mode is enabled (for panic hook).
static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Install panic hook to restore terminal state on panic.
fn install_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Restore terminal state before printing panic
        if RAW_MODE_ACTIVE.load(Ordering::SeqCst) {
            let _ = disable_raw_mode();
            RAW_MODE_ACTIVE.store(false, Ordering::SeqCst);
        }
        // Call original panic handler
        original_hook(panic_info);
    }));
}

/// Braille spinner frames for animated activity indicator.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
    /// Get the icon for this activity, optionally animated.
    pub fn icon(&self, frame: usize) -> &'static str {
        match self {
            Activity::Idle => "○",
            Activity::Thinking | Activity::Streaming => SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
            Activity::ExecutingTool(_) => SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
            Activity::WaitingForUser => "?",
        }
    }

    /// Check if this activity should be animated.
    pub fn is_active(&self) -> bool {
        !matches!(self, Activity::Idle | Activity::WaitingForUser)
    }

    /// Get the color for this activity.
    pub fn color(&self) -> &'static str {
        match self {
            Activity::Idle => "\x1b[90m",             // Gray
            Activity::Thinking => "\x1b[34m",         // Blue
            Activity::Streaming => "\x1b[32m",        // Green
            Activity::ExecutingTool(_) => "\x1b[33m", // Yellow
            Activity::WaitingForUser => "\x1b[33m",   // Yellow
        }
    }

    /// Get display text for this activity.
    pub fn text(&self) -> String {
        match self {
            Activity::Idle => "Idle".to_string(),
            Activity::Thinking => "Thinking".to_string(),
            Activity::Streaming => "Streaming".to_string(),
            Activity::ExecutingTool(name) => name.clone(),
            Activity::WaitingForUser => "Waiting".to_string(),
        }
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
    /// Get elapsed time since task started.
    pub fn elapsed(&self) -> Duration {
        self.task_started.map(|start| start.elapsed()).unwrap_or_default()
    }
}

/// Format a duration as a human-readable string.
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

/// Format a number with commas for readability.
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

/// REPL display for terminal output.
pub struct ReplDisplay {
    status: StatusState,
    /// Track if we're in raw mode (for cleanup).
    raw_mode_enabled: bool,
    /// Animation frame counter for spinner.
    frame: usize,
    /// Whether stdout is a TTY.
    is_tty: bool,
}

impl ReplDisplay {
    /// Create a new display.
    pub fn new() -> Result<Self> {
        let is_tty = stdout().is_terminal();

        if is_tty {
            // Install panic hook to restore terminal on panic
            install_panic_hook();

            // Enable raw mode for terminal control
            enable_raw_mode()?;
            RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);
        }

        Ok(Self {
            status: StatusState {
                task_started: Some(Instant::now()),
                ..Default::default()
            },
            raw_mode_enabled: is_tty,
            frame: 0,
            is_tty,
        })
    }

    /// Check if we're in TTY mode.
    pub fn is_inline(&self) -> bool {
        self.is_tty
    }

    /// Print streaming content.
    pub fn print_content(&mut self, text: &str) -> Result<()> {
        if self.raw_mode_enabled {
            // In raw mode, \n doesn't include carriage return
            print!("{}", text.replace('\n', "\r\n"));
        } else {
            print!("{}", text);
        }
        stdout().flush()?;
        Ok(())
    }

    /// Print a complete line.
    pub fn println(&mut self, line: &str) -> Result<()> {
        if self.raw_mode_enabled {
            print!("{}\r\n", line);
        } else {
            println!("{}", line);
        }
        stdout().flush()?;
        Ok(())
    }

    /// Update the status (just tracks state, doesn't print during streaming).
    pub fn update_status(&mut self, status: StatusState) -> Result<()> {
        self.status = status;
        self.frame = self.frame.wrapping_add(1);
        // Don't print status during streaming - it causes flicker
        // Status will be shown in print_summary()
        Ok(())
    }

    /// Print final summary.
    pub fn print_summary(&self) -> Result<()> {
        let elapsed = format_duration(self.status.elapsed());
        let tokens = format_number(self.status.tokens_used);

        if self.raw_mode_enabled {
            // Already exited raw mode at this point via cleanup()
            eprintln!(
                "\r\n[Completed: {} iterations, {} tokens, ${:.4}, {}]",
                self.status.iteration, tokens, self.status.cost, elapsed
            );
        } else {
            eprintln!(
                "\n[Completed: {} iterations, {} tokens, ${:.4}, {}]",
                self.status.iteration, tokens, self.status.cost, elapsed
            );
        }
        Ok(())
    }

    /// Cleanup and restore terminal state.
    pub fn cleanup(&mut self) -> Result<()> {
        if self.raw_mode_enabled {
            // Print a newline to ensure we're on a fresh line
            print!("\r\n");
            stdout().flush()?;

            disable_raw_mode()?;
            self.raw_mode_enabled = false;
            RAW_MODE_ACTIVE.store(false, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl Drop for ReplDisplay {
    fn drop(&mut self) {
        // Always try to restore terminal state
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
    fn test_format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_secs(5)), "5s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
    }

    #[test]
    fn test_format_duration_minutes() {
        assert_eq!(format_duration(Duration::from_secs(60)), "1m 0s");
        assert_eq!(format_duration(Duration::from_secs(90)), "1m 30s");
        assert_eq!(format_duration(Duration::from_secs(3599)), "59m 59s");
    }

    #[test]
    fn test_format_duration_hours() {
        assert_eq!(format_duration(Duration::from_secs(3600)), "1h 0m");
        assert_eq!(format_duration(Duration::from_secs(7200)), "2h 0m");
        assert_eq!(format_duration(Duration::from_secs(3660)), "1h 1m");
    }

    #[test]
    fn test_format_number() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(1234567), "1,234,567");
    }

    #[test]
    fn test_activity_icon() {
        // Static icons
        assert_eq!(Activity::Idle.icon(0), "○");
        assert_eq!(Activity::WaitingForUser.icon(0), "?");

        // Animated icons (spinners)
        assert_eq!(Activity::Thinking.icon(0), "⠋");
        assert_eq!(Activity::Thinking.icon(1), "⠙");
        assert_eq!(Activity::Streaming.icon(0), "⠋");
        assert_eq!(Activity::ExecutingTool("test".to_string()).icon(0), "⠋");

        // Test frame wrapping
        assert_eq!(Activity::Thinking.icon(10), "⠋"); // wraps to frame 0
    }

    #[test]
    fn test_activity_is_active() {
        assert!(!Activity::Idle.is_active());
        assert!(Activity::Thinking.is_active());
        assert!(Activity::Streaming.is_active());
        assert!(Activity::ExecutingTool("test".to_string()).is_active());
        assert!(!Activity::WaitingForUser.is_active());
    }

    #[test]
    fn test_status_state_default() {
        let status = StatusState::default();
        assert_eq!(status.iteration, 0);
        assert_eq!(status.tokens_used, 0);
        assert_eq!(status.cost, 0.0);
        assert!(status.task_started.is_none());
    }
}
