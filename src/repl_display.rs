//! REPL display module for inline terminal rendering.
//!
//! Provides a status bar at the bottom of the terminal while streaming
//! content scrolls above it, similar to Claude Code's terminal UX.

use std::io::{IsTerminal, Stdout, Write, stdout};
use std::time::{Duration, Instant};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Widget;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::error::Result;

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
    pub fn color(&self) -> Color {
        match self {
            Activity::Idle => Color::Gray,
            Activity::Thinking => Color::Blue,
            Activity::Streaming => Color::Green,
            Activity::ExecutingTool(_) => Color::Yellow,
            Activity::WaitingForUser => Color::Yellow,
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

/// Display mode - either inline (TTY) or fallback (non-TTY).
enum DisplayMode {
    /// Inline viewport mode with ratatui.
    Inline {
        terminal: Terminal<CrosstermBackend<Stdout>>,
    },
    /// Fallback mode for non-TTY environments.
    Fallback,
}

/// REPL display with inline viewport for status bar.
pub struct ReplDisplay {
    mode: DisplayMode,
    status: StatusState,
    /// Track if we're in raw mode (for cleanup).
    raw_mode_enabled: bool,
    /// Animation frame counter for spinner.
    frame: usize,
}

impl ReplDisplay {
    /// Create a new display.
    ///
    /// If stdout is a TTY, uses inline viewport mode.
    /// Otherwise, falls back to simple printing.
    pub fn new() -> Result<Self> {
        if stdout().is_terminal() {
            // Enable raw mode for terminal control
            enable_raw_mode()?;

            // Create terminal with inline viewport (1 line for status)
            let backend = CrosstermBackend::new(stdout());
            let terminal = Terminal::with_options(
                backend,
                TerminalOptions {
                    viewport: Viewport::Inline(1),
                },
            )?;

            Ok(Self {
                mode: DisplayMode::Inline { terminal },
                status: StatusState {
                    task_started: Some(Instant::now()),
                    ..Default::default()
                },
                raw_mode_enabled: true,
                frame: 0,
            })
        } else {
            Ok(Self {
                mode: DisplayMode::Fallback,
                status: StatusState {
                    task_started: Some(Instant::now()),
                    ..Default::default()
                },
                raw_mode_enabled: false,
                frame: 0,
            })
        }
    }

    /// Check if we're in inline (TTY) mode.
    pub fn is_inline(&self) -> bool {
        matches!(self.mode, DisplayMode::Inline { .. })
    }

    /// Print streaming content (scrolls above status bar).
    pub fn print_content(&mut self, text: &str) -> Result<()> {
        match &mut self.mode {
            DisplayMode::Inline { terminal } => {
                // Use insert_before to print above the inline viewport
                terminal.insert_before(1, |buf| {
                    // Just render the text - it will scroll naturally
                    let para = Paragraph::new(text);
                    para.render(buf.area, buf);
                })?;
            }
            DisplayMode::Fallback => {
                print!("{}", text);
                stdout().flush()?;
            }
        }
        Ok(())
    }

    /// Print a complete line (e.g., tool output).
    pub fn println(&mut self, line: &str) -> Result<()> {
        match &mut self.mode {
            DisplayMode::Inline { terminal } => {
                terminal.insert_before(1, |buf| {
                    let para = Paragraph::new(format!("{}\n", line));
                    para.render(buf.area, buf);
                })?;
            }
            DisplayMode::Fallback => {
                println!("{}", line);
            }
        }
        Ok(())
    }

    /// Update the status bar.
    pub fn update_status(&mut self, status: StatusState) -> Result<()> {
        self.status = status;
        // Advance spinner frame for animation
        self.frame = self.frame.wrapping_add(1);

        // Pre-compute values before borrowing terminal
        let color = self.status.activity.color();
        // Use a reasonable default width, will be overwritten in draw
        let status_text = self.format_status_line(120);

        match &mut self.mode {
            DisplayMode::Inline { terminal } => {
                terminal.draw(|frame| {
                    let area = frame.area();
                    // Truncate to actual width if needed
                    let display_text = if status_text.len() > area.width as usize {
                        format!("{}...", &status_text[..area.width.saturating_sub(3) as usize])
                    } else {
                        status_text.clone()
                    };
                    let para = Paragraph::new(display_text).style(Style::default().fg(color));
                    frame.render_widget(para, area);
                })?;
            }
            DisplayMode::Fallback => {
                // In fallback mode, don't print status during streaming
                // It will be shown at the end via print_summary()
            }
        }
        Ok(())
    }

    /// Format the status line to fit within the given width.
    fn format_status_line(&self, max_width: usize) -> String {
        let icon = self.status.activity.icon(self.frame);
        let activity = self.status.activity.text();
        let elapsed = format_duration(self.status.elapsed());
        let tokens = format_number(self.status.tokens_used);

        let status = format!(
            "{} {} │ Iter {} │ {} tokens │ ${:.4} │ {}",
            icon, activity, self.status.iteration, tokens, self.status.cost, elapsed
        );

        // Truncate if too long
        if status.len() > max_width {
            format!("{}...", &status[..max_width.saturating_sub(3)])
        } else {
            status
        }
    }

    /// Print final summary (for fallback mode or at end).
    pub fn print_summary(&self) -> Result<()> {
        let elapsed = format_duration(self.status.elapsed());
        let tokens = format_number(self.status.tokens_used);

        eprintln!(
            "\n[Completed: {} iterations, {} tokens, ${:.4}, {}]",
            self.status.iteration, tokens, self.status.cost, elapsed
        );
        Ok(())
    }

    /// Cleanup and restore terminal state.
    pub fn cleanup(&mut self) -> Result<()> {
        if self.raw_mode_enabled {
            disable_raw_mode()?;
            self.raw_mode_enabled = false;
        }
        Ok(())
    }
}

impl Drop for ReplDisplay {
    fn drop(&mut self) {
        // Always try to restore terminal state
        if self.raw_mode_enabled {
            let _ = disable_raw_mode();
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
