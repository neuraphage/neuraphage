//! Parallel workstream TUI for monitoring multiple concurrent tasks.
//!
//! Provides a multi-pane terminal interface inspired by btop's visual fidelity
//! and Gas Town's operational model. Supports Dashboard, Split, Grid, and Focus
//! layout modes.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap};

use crate::error::Result;
use crate::task::{TaskId, TaskStatus};
use crate::ui::notifications::Notification;

/// Maximum lines to keep in output buffer per workstream.
const MAX_OUTPUT_LINES: usize = 1000;

/// Layout modes for the parallel TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LayoutMode {
    /// Single primary view with sidebar (default).
    #[default]
    Dashboard,
    /// Primary + secondary split view.
    Split,
    /// 2x2 grid of workstreams.
    Grid,
    /// Full-screen focus on one workstream.
    Focus,
}

/// Sort order for workstream list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    /// By status (running first, then waiting, etc.)
    #[default]
    Status,
    /// By cost (highest first).
    Cost,
    /// By last activity (most recent first).
    Activity,
    /// By name (alphabetical).
    Name,
}

/// Connection status to daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectionStatus {
    /// Connected to daemon.
    #[default]
    Connected,
    /// Disconnected, attempting reconnect.
    Disconnected,
    /// Reconnecting with attempt count.
    Reconnecting(u8),
}

/// Budget status for display.
#[derive(Debug, Clone, Default)]
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

impl BudgetStatus {
    /// Create a new budget status.
    pub fn new(current_spend: f64, budget_limit: f64) -> Self {
        let usage_percent = if budget_limit > 0.0 { (current_spend / budget_limit).min(1.0) } else { 0.0 };
        Self {
            current_spend,
            budget_limit,
            usage_percent,
            reset_in: None,
            exceeded: current_spend >= budget_limit,
        }
    }
}

/// Event importance for filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EventImportance {
    /// Low importance (informational).
    Low,
    /// Normal importance.
    Normal,
    /// High importance (requires attention).
    High,
    /// Critical (immediate attention needed).
    Critical,
}

/// An event in the timeline.
#[derive(Debug, Clone)]
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

impl TimelineEvent {
    /// Create a new timeline event.
    pub fn new(icon: char, description: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            icon,
            description: description.into(),
            task_ids: Vec::new(),
            importance: EventImportance::Normal,
        }
    }

    /// Set the related tasks.
    pub fn with_tasks(mut self, task_ids: Vec<TaskId>) -> Self {
        self.task_ids = task_ids;
        self
    }

    /// Set the importance.
    pub fn with_importance(mut self, importance: EventImportance) -> Self {
        self.importance = importance;
        self
    }
}

/// A line of output from a workstream.
#[derive(Debug, Clone)]
pub struct OutputLine {
    /// The text content.
    pub text: String,
    /// When this line was received.
    pub timestamp: DateTime<Utc>,
    /// Whether this is an error line.
    pub is_error: bool,
}

impl OutputLine {
    /// Create a new output line.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            timestamp: Utc::now(),
            is_error: false,
        }
    }

    /// Create an error output line.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            timestamp: Utc::now(),
            is_error: true,
        }
    }
}

/// Global stats for display.
#[derive(Debug, Clone, Default)]
pub struct GlobalStats {
    /// Number of running tasks.
    pub running_count: usize,
    /// Number of tasks waiting for user.
    pub waiting_count: usize,
    /// Number of queued tasks.
    pub queued_count: usize,
    /// Number of completed tasks.
    pub completed_count: usize,
    /// Total cost across all tasks.
    pub total_cost: f64,
    /// Total tokens used.
    pub total_tokens: u64,
}

/// Filter for workstream list.
#[derive(Debug, Clone, Default)]
pub struct WorkstreamFilter {
    /// Filter by status.
    pub status: Option<TaskStatus>,
    /// Filter by name (substring match).
    pub name_filter: Option<String>,
    /// Only show tasks needing attention.
    pub attention_only: bool,
}

/// A single workstream being tracked.
#[derive(Debug, Clone)]
pub struct Workstream {
    /// Task ID.
    pub task_id: TaskId,
    /// Short name (derived from description).
    pub name: String,
    /// Full task description.
    pub description: String,
    /// Current status.
    pub status: TaskStatus,
    /// Live output buffer (VecDeque used as ring buffer).
    pub output: VecDeque<OutputLine>,
    /// Progress indicator (0.0-1.0).
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
    /// Create a new workstream from task info.
    pub fn new(task_id: TaskId, description: impl Into<String>, status: TaskStatus) -> Self {
        let description = description.into();
        let name = Self::name_from_description(&description);
        Self {
            task_id,
            name,
            description,
            status,
            output: VecDeque::with_capacity(MAX_OUTPUT_LINES),
            progress: 0.0,
            iteration: 0,
            cost: 0.0,
            tokens: 0,
            last_activity: Utc::now(),
            needs_attention: false,
            attention_reason: None,
            worktree_path: None,
            idle_duration: Duration::ZERO,
        }
    }

    /// Generate a short name from task description.
    /// "Implement user authentication" -> "user-auth"
    pub fn name_from_description(desc: &str) -> String {
        const STOP_WORDS: &[&str] = &["the", "a", "an", "to", "for", "and", "or", "in", "on", "with"];
        desc.split_whitespace()
            .filter(|w| !STOP_WORDS.contains(&w.to_lowercase().as_str()))
            .take(2)
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-')
            .take(20)
            .collect()
    }

    /// Add an output line to the buffer.
    pub fn add_output(&mut self, line: OutputLine) {
        self.output.push_back(line);
        while self.output.len() > MAX_OUTPUT_LINES {
            self.output.pop_front();
        }
        self.last_activity = Utc::now();
        self.idle_duration = Duration::ZERO;
    }

    /// Update idle duration based on current time.
    pub fn update_idle(&mut self) {
        let now = Utc::now();
        if let Ok(duration) = (now - self.last_activity).to_std() {
            self.idle_duration = duration;
        }
    }

    /// Get status icon.
    pub fn status_icon(&self) -> &'static str {
        match self.status {
            TaskStatus::Running => "●",
            TaskStatus::WaitingForUser => "?",
            TaskStatus::Queued => "○",
            TaskStatus::Blocked => "◌",
            TaskStatus::Paused => "◑",
            TaskStatus::Completed => "✓",
            TaskStatus::Failed => "✗",
            TaskStatus::Cancelled => "⊘",
        }
    }

    /// Get status color.
    pub fn status_color(&self) -> Color {
        match self.status {
            TaskStatus::Running => Color::Rgb(0, 255, 127),        // Spring green
            TaskStatus::WaitingForUser => Color::Rgb(255, 215, 0), // Gold
            TaskStatus::Queued => Color::Rgb(169, 169, 169),       // Dark gray
            TaskStatus::Blocked => Color::Rgb(255, 69, 0),         // Red-orange
            TaskStatus::Paused => Color::Rgb(255, 215, 0),         // Gold
            TaskStatus::Completed => Color::Rgb(50, 205, 50),      // Lime green
            TaskStatus::Failed => Color::Rgb(220, 20, 60),         // Crimson
            TaskStatus::Cancelled => Color::Rgb(128, 128, 128),    // Gray
        }
    }
}

/// TUI application state for parallel workstreams.
#[derive(Debug, Clone)]
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
    /// Connection status to daemon.
    pub connection_status: ConnectionStatus,
    /// Maximum events in timeline.
    max_timeline_events: usize,
}

impl Default for ParallelTuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl ParallelTuiState {
    /// Create a new parallel TUI state.
    pub fn new() -> Self {
        Self {
            workstreams: Vec::new(),
            primary_focus: 0,
            secondary_focus: None,
            layout_mode: LayoutMode::Dashboard,
            event_timeline: VecDeque::with_capacity(100),
            stats: GlobalStats::default(),
            budget: BudgetStatus::default(),
            notifications: VecDeque::with_capacity(50),
            show_help: false,
            filter: WorkstreamFilter::default(),
            sort_order: SortOrder::Status,
            connection_status: ConnectionStatus::Connected,
            max_timeline_events: 100,
        }
    }

    /// Add a workstream.
    pub fn add_workstream(&mut self, workstream: Workstream) {
        self.workstreams.push(workstream);
        self.update_stats();
    }

    /// Remove a workstream by task ID.
    pub fn remove_workstream(&mut self, task_id: &TaskId) {
        self.workstreams.retain(|w| &w.task_id != task_id);
        if self.primary_focus >= self.workstreams.len() && !self.workstreams.is_empty() {
            self.primary_focus = self.workstreams.len() - 1;
        }
        self.update_stats();
    }

    /// Get a workstream by task ID.
    pub fn get_workstream(&self, task_id: &TaskId) -> Option<&Workstream> {
        self.workstreams.iter().find(|w| &w.task_id == task_id)
    }

    /// Get a mutable workstream by task ID.
    pub fn get_workstream_mut(&mut self, task_id: &TaskId) -> Option<&mut Workstream> {
        self.workstreams.iter_mut().find(|w| &w.task_id == task_id)
    }

    /// Get the currently selected workstream.
    pub fn selected(&self) -> Option<&Workstream> {
        self.filtered_workstreams().get(self.primary_focus).copied()
    }

    /// Get the secondary selected workstream (for split view).
    pub fn secondary_selected(&self) -> Option<&Workstream> {
        self.secondary_focus
            .and_then(|idx| self.filtered_workstreams().get(idx).copied())
    }

    /// Get filtered workstreams based on current filter.
    pub fn filtered_workstreams(&self) -> Vec<&Workstream> {
        self.workstreams
            .iter()
            .filter(|w| {
                // Apply status filter
                if let Some(status) = self.filter.status
                    && w.status != status
                {
                    return false;
                }
                // Apply name filter
                if let Some(ref name_filter) = self.filter.name_filter
                    && !w.name.contains(name_filter)
                    && !w.description.contains(name_filter)
                {
                    return false;
                }
                // Apply attention filter
                if self.filter.attention_only && !w.needs_attention {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Navigate to next workstream.
    pub fn next_workstream(&mut self) {
        let filtered = self.filtered_workstreams();
        if !filtered.is_empty() {
            self.primary_focus = (self.primary_focus + 1) % filtered.len();
        }
    }

    /// Navigate to previous workstream.
    pub fn prev_workstream(&mut self) {
        let filtered = self.filtered_workstreams();
        if !filtered.is_empty() {
            self.primary_focus = self.primary_focus.checked_sub(1).unwrap_or(filtered.len() - 1);
        }
    }

    /// Jump to workstream by index (1-9).
    pub fn jump_to_workstream(&mut self, index: usize) {
        let filtered = self.filtered_workstreams();
        if index > 0 && index <= filtered.len() {
            self.primary_focus = index - 1;
        }
    }

    /// Toggle layout mode.
    pub fn toggle_layout(&mut self, mode: LayoutMode) {
        if self.layout_mode == mode {
            self.layout_mode = LayoutMode::Dashboard;
        } else {
            self.layout_mode = mode;
        }

        // Set up secondary focus for split mode
        if self.layout_mode == LayoutMode::Split {
            let filtered = self.filtered_workstreams();
            if filtered.len() > 1 {
                self.secondary_focus = Some((self.primary_focus + 1) % filtered.len());
            }
        } else {
            self.secondary_focus = None;
        }
    }

    /// Cycle sort order.
    pub fn cycle_sort_order(&mut self) {
        self.sort_order = match self.sort_order {
            SortOrder::Status => SortOrder::Cost,
            SortOrder::Cost => SortOrder::Activity,
            SortOrder::Activity => SortOrder::Name,
            SortOrder::Name => SortOrder::Status,
        };
        self.sort_workstreams();
    }

    /// Sort workstreams by current sort order.
    fn sort_workstreams(&mut self) {
        match self.sort_order {
            SortOrder::Status => {
                self.workstreams.sort_by_key(|w| match w.status {
                    TaskStatus::Running => 0,
                    TaskStatus::WaitingForUser => 1,
                    TaskStatus::Blocked => 2,
                    TaskStatus::Queued => 3,
                    TaskStatus::Paused => 4,
                    TaskStatus::Completed => 5,
                    TaskStatus::Failed => 6,
                    TaskStatus::Cancelled => 7,
                });
            }
            SortOrder::Cost => {
                self.workstreams
                    .sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
            }
            SortOrder::Activity => {
                self.workstreams.sort_by_key(|w| std::cmp::Reverse(w.last_activity));
            }
            SortOrder::Name => {
                self.workstreams.sort_by(|a, b| a.name.cmp(&b.name));
            }
        }
    }

    /// Add an event to the timeline.
    pub fn add_timeline_event(&mut self, event: TimelineEvent) {
        self.event_timeline.push_back(event);
        while self.event_timeline.len() > self.max_timeline_events {
            self.event_timeline.pop_front();
        }
    }

    /// Update stats from workstreams.
    pub fn update_stats(&mut self) {
        self.stats = GlobalStats {
            running_count: self
                .workstreams
                .iter()
                .filter(|w| w.status == TaskStatus::Running)
                .count(),
            waiting_count: self
                .workstreams
                .iter()
                .filter(|w| w.status == TaskStatus::WaitingForUser)
                .count(),
            queued_count: self
                .workstreams
                .iter()
                .filter(|w| w.status == TaskStatus::Queued)
                .count(),
            completed_count: self
                .workstreams
                .iter()
                .filter(|w| w.status == TaskStatus::Completed)
                .count(),
            total_cost: self.workstreams.iter().map(|w| w.cost).sum(),
            total_tokens: self.workstreams.iter().map(|w| w.tokens).sum(),
        };
    }

    /// Update idle durations for all workstreams.
    pub fn update_all_idle(&mut self) {
        for workstream in &mut self.workstreams {
            workstream.update_idle();
        }
    }
}

/// Parallel TUI application.
pub struct ParallelTuiApp {
    /// Application state.
    pub state: ParallelTuiState,
    /// Whether the app should quit.
    pub should_quit: bool,
}

impl Default for ParallelTuiApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ParallelTuiApp {
    /// Create a new parallel TUI app.
    pub fn new() -> Self {
        Self {
            state: ParallelTuiState::new(),
            should_quit: false,
        }
    }

    /// Handle input events.
    pub fn handle_event(&mut self, event: Event) {
        if let Event::Key(key) = event {
            self.handle_key(key);
        }
    }

    /// Handle key press.
    fn handle_key(&mut self, key: KeyEvent) {
        // Handle Ctrl+C
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // Handle help overlay escape
        if self.state.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.state.show_help = false;
                }
                _ => {}
            }
            return;
        }

        match key.code {
            // Quit
            KeyCode::Char('q') => self.should_quit = true,

            // Navigation
            KeyCode::Char('j') | KeyCode::Down => self.state.next_workstream(),
            KeyCode::Char('k') | KeyCode::Up => self.state.prev_workstream(),

            // Number keys for quick jump
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let index = c.to_digit(10).unwrap() as usize;
                self.state.jump_to_workstream(index);
            }

            // Layout modes
            KeyCode::Char(' ') => self.state.toggle_layout(LayoutMode::Split),
            KeyCode::Char('g') => self.state.toggle_layout(LayoutMode::Grid),
            KeyCode::Char('d') => self.state.layout_mode = LayoutMode::Dashboard,
            KeyCode::Enter => self.state.toggle_layout(LayoutMode::Focus),

            // Help
            KeyCode::Char('?') => self.state.show_help = true,
            KeyCode::Esc => {
                // Exit focus mode, clear filters
                if self.state.layout_mode == LayoutMode::Focus {
                    self.state.layout_mode = LayoutMode::Dashboard;
                }
                self.state.filter = WorkstreamFilter::default();
            }

            // Sort
            KeyCode::Char('s') => self.state.cycle_sort_order(),

            // Tab to cycle between panes in split/grid mode
            KeyCode::Tab => {
                if self.state.layout_mode == LayoutMode::Split {
                    // Swap primary and secondary focus
                    if let Some(secondary) = self.state.secondary_focus {
                        self.state.secondary_focus = Some(self.state.primary_focus);
                        self.state.primary_focus = secondary;
                    }
                }
            }

            _ => {}
        }
    }

    /// Render the UI.
    pub fn render(&self, frame: &mut Frame) {
        match self.state.layout_mode {
            LayoutMode::Dashboard => self.render_dashboard(frame),
            LayoutMode::Split => self.render_split(frame),
            LayoutMode::Grid => self.render_grid(frame),
            LayoutMode::Focus => self.render_focus(frame),
        }

        // Render help overlay if shown
        if self.state.show_help {
            self.render_help_popup(frame);
        }
    }

    /// Render dashboard layout.
    fn render_dashboard(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Main content
                Constraint::Length(6), // Events strip
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
            .split(main_layout[0]);

        let sidebar_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(6)])
            .split(content_layout[0]);

        // Render components
        self.render_workstream_list(frame, sidebar_layout[0]);
        self.render_stats(frame, sidebar_layout[1]);
        self.render_primary_view(frame, content_layout[1]);
        self.render_events_strip(frame, main_layout[1]);
        self.render_status_bar(frame, main_layout[2]);
    }

    /// Render split layout.
    fn render_split(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Main content
                Constraint::Length(5), // Events strip
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20), // Sidebar
                Constraint::Percentage(40), // Primary
                Constraint::Percentage(40), // Secondary
            ])
            .split(main_layout[0]);

        // Render components
        self.render_workstream_list(frame, content_layout[0]);
        self.render_primary_view(frame, content_layout[1]);
        self.render_secondary_view(frame, content_layout[2]);
        self.render_events_strip(frame, main_layout[1]);
        self.render_status_bar(frame, main_layout[2]);
    }

    /// Render grid layout (2x2).
    fn render_grid(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Main content
                Constraint::Length(4), // Events strip
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        let content_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(15), Constraint::Percentage(85)])
            .split(main_layout[0]);

        let grid_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(content_layout[1]);

        let top_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(grid_layout[0]);

        let bottom_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(grid_layout[1]);

        // Render components
        self.render_workstream_list(frame, content_layout[0]);
        self.render_grid_cell(frame, top_row[0], 0);
        self.render_grid_cell(frame, top_row[1], 1);
        self.render_grid_cell(frame, bottom_row[0], 2);
        self.render_grid_cell(frame, bottom_row[1], 3);
        self.render_events_strip(frame, main_layout[1]);
        self.render_status_bar(frame, main_layout[2]);
    }

    /// Render focus layout (full screen).
    fn render_focus(&self, frame: &mut Frame) {
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(10), Constraint::Length(1)])
            .split(frame.area());

        self.render_primary_view(frame, main_layout[0]);
        self.render_status_bar(frame, main_layout[1]);
    }

    /// Render the workstream list (sidebar).
    fn render_workstream_list(&self, frame: &mut Frame, area: Rect) {
        let filtered = self.state.filtered_workstreams();

        let items: Vec<ListItem> = filtered
            .iter()
            .enumerate()
            .map(|(i, workstream)| {
                let icon = Span::styled(workstream.status_icon(), Style::default().fg(workstream.status_color()));
                let index = Span::styled(format!("[{}]", i + 1), Style::default().fg(Color::DarkGray));
                let name = if workstream.needs_attention {
                    Span::styled(
                        &workstream.name,
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::raw(&workstream.name)
                };

                let line = Line::from(vec![index, " ".into(), icon, " ".into(), name]);
                ListItem::new(line)
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.state.primary_focus));

        let title = format!(" Workstreams ({}) ", filtered.len());
        let list = List::new(items)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_set(border::ROUNDED),
            )
            .highlight_style(Style::default().bg(Color::Rgb(40, 40, 40)).add_modifier(Modifier::BOLD));

        frame.render_stateful_widget(list, area, &mut list_state);
    }

    /// Render the stats panel.
    fn render_stats(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Stats ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

        // Task counts
        let counts = format!(
            "Active: {}  Wait: {}  Queue: {}",
            self.state.stats.running_count, self.state.stats.waiting_count, self.state.stats.queued_count
        );
        frame.render_widget(Paragraph::new(counts), layout[0]);

        // Cost
        let cost = format!("Cost: ${:.2}", self.state.stats.total_cost);
        frame.render_widget(Paragraph::new(cost), layout[1]);

        // Budget gauge
        let budget_percent = (self.state.budget.usage_percent * 100.0) as u16;
        let budget_color = if self.state.budget.exceeded {
            Color::Rgb(220, 20, 60)
        } else if self.state.budget.usage_percent > 0.8 {
            Color::Rgb(255, 215, 0)
        } else {
            Color::Rgb(50, 205, 50)
        };
        let gauge = Gauge::default()
            .label(format!("Budget: {}%", budget_percent))
            .ratio(self.state.budget.usage_percent.min(1.0))
            .gauge_style(Style::default().fg(budget_color));
        frame.render_widget(gauge, layout[2]);
    }

    /// Render the primary workstream view.
    fn render_primary_view(&self, frame: &mut Frame, area: Rect) {
        let workstream = self.state.selected();
        self.render_workstream_detail(frame, area, workstream, true);
    }

    /// Render the secondary workstream view.
    fn render_secondary_view(&self, frame: &mut Frame, area: Rect) {
        let workstream = self.state.secondary_selected();
        self.render_workstream_detail(frame, area, workstream, false);
    }

    /// Render a workstream detail view.
    fn render_workstream_detail(
        &self,
        frame: &mut Frame,
        area: Rect,
        workstream: Option<&Workstream>,
        is_primary: bool,
    ) {
        let (title, border_color) = if let Some(w) = workstream {
            let prefix = if is_primary { "Active" } else { "Secondary" };
            let title = format!(" {}: {} ", prefix, w.name);
            let color = if is_primary { Color::Rgb(0, 191, 255) } else { Color::Rgb(70, 70, 70) };
            (title, color)
        } else {
            (" No workstream selected ".to_string(), Color::Rgb(70, 70, 70))
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if let Some(workstream) = workstream {
            // Render output
            let output_lines: Vec<Line> = workstream
                .output
                .iter()
                .rev()
                .take(inner.height as usize)
                .rev()
                .map(|line| {
                    if line.is_error {
                        Line::styled(&line.text, Style::default().fg(Color::Red))
                    } else {
                        Line::raw(&line.text)
                    }
                })
                .collect();

            let paragraph = Paragraph::new(output_lines).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, inner);

            // Render progress bar at bottom if running
            if workstream.status == TaskStatus::Running && inner.height > 2 {
                let progress_area = Rect {
                    x: inner.x,
                    y: inner.y + inner.height.saturating_sub(1),
                    width: inner.width,
                    height: 1,
                };
                let gauge = Gauge::default()
                    .ratio(workstream.progress as f64)
                    .gauge_style(Style::default().fg(Color::Rgb(0, 255, 127)))
                    .use_unicode(true);
                frame.render_widget(gauge, progress_area);
            }
        } else {
            let placeholder = Paragraph::new("Select a workstream to view...")
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true });
            frame.render_widget(placeholder, inner);
        }
    }

    /// Render a grid cell.
    fn render_grid_cell(&self, frame: &mut Frame, area: Rect, index: usize) {
        let filtered = self.state.filtered_workstreams();
        let workstream = filtered.get(index).copied();
        let is_selected = index == self.state.primary_focus;
        self.render_workstream_detail(frame, area, workstream, is_selected);
    }

    /// Render the events strip.
    fn render_events_strip(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Events ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let events: Vec<Line> = self
            .state
            .event_timeline
            .iter()
            .rev()
            .take(inner.height as usize)
            .map(|event| {
                let time = event.timestamp.format("%H:%M").to_string();
                let icon_color = match event.importance {
                    EventImportance::Critical => Color::Red,
                    EventImportance::High => Color::Yellow,
                    EventImportance::Normal => Color::White,
                    EventImportance::Low => Color::DarkGray,
                };
                Line::from(vec![
                    Span::styled(time, Style::default().fg(Color::DarkGray)),
                    " ".into(),
                    Span::styled(event.icon.to_string(), Style::default().fg(icon_color)),
                    " ".into(),
                    Span::raw(&event.description),
                ])
            })
            .collect();

        let paragraph = Paragraph::new(events);
        frame.render_widget(paragraph, inner);
    }

    /// Render the status bar.
    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let connection_indicator = match self.state.connection_status {
            ConnectionStatus::Connected => "●".green(),
            ConnectionStatus::Disconnected => "●".red(),
            ConnectionStatus::Reconnecting(n) => Span::styled(format!("●({})", n), Style::default().fg(Color::Yellow)),
        };

        let mode_text = match self.state.layout_mode {
            LayoutMode::Dashboard => "Dashboard",
            LayoutMode::Split => "Split",
            LayoutMode::Grid => "Grid",
            LayoutMode::Focus => "Focus",
        };

        let sort_text = match self.state.sort_order {
            SortOrder::Status => "status",
            SortOrder::Cost => "cost",
            SortOrder::Activity => "activity",
            SortOrder::Name => "name",
        };

        let help_text = format!(
            " {} [{}] Sort: {} | [j/k] Nav [Space] Split [g] Grid [Enter] Focus [s] Sort [?] Help [q] Quit ",
            connection_indicator, mode_text, sort_text
        );
        let style = Style::default().bg(Color::Rgb(30, 30, 30)).fg(Color::White);
        let paragraph = Paragraph::new(help_text).style(style);
        frame.render_widget(paragraph, area);
    }

    /// Render help popup overlay.
    fn render_help_popup(&self, frame: &mut Frame) {
        let area = centered_rect(60, 70, frame.area());
        frame.render_widget(Clear, area);

        let help_text = vec![
            Line::from("Keyboard Shortcuts".bold()),
            Line::from(""),
            Line::from(vec!["1-9".cyan(), " - Jump to workstream by index".into()]),
            Line::from(vec!["j/↓".cyan(), " - Next workstream".into()]),
            Line::from(vec!["k/↑".cyan(), " - Previous workstream".into()]),
            Line::from(vec!["Tab".cyan(), " - Cycle focus in split mode".into()]),
            Line::from(""),
            Line::from("Layout Modes".bold()),
            Line::from(vec!["d".cyan(), " - Dashboard (default)".into()]),
            Line::from(vec!["Space".cyan(), " - Toggle split view".into()]),
            Line::from(vec!["g".cyan(), " - Toggle grid view".into()]),
            Line::from(vec!["Enter".cyan(), " - Toggle focus mode".into()]),
            Line::from(""),
            Line::from("Actions".bold()),
            Line::from(vec!["s".cyan(), " - Cycle sort order".into()]),
            Line::from(vec!["?".cyan(), " - Toggle help".into()]),
            Line::from(vec!["q".cyan(), " - Quit".into()]),
            Line::from(vec!["Ctrl+C".cyan(), " - Force quit".into()]),
            Line::from(""),
            Line::from("Press ESC or ? to close".dark_gray()),
        ];

        let block = Block::default()
            .title(" Help ")
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .style(Style::default().bg(Color::Black));

        let paragraph = Paragraph::new(help_text).block(block).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
    }
}

/// Helper function to create a centered rect.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Terminal wrapper for setup/teardown.
pub struct ParallelTui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl ParallelTui {
    /// Initialize the terminal.
    pub fn new() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Draw the UI.
    pub fn draw(&mut self, app: &ParallelTuiApp) -> Result<()> {
        self.terminal.draw(|frame| app.render(frame))?;
        Ok(())
    }

    /// Poll for events with timeout.
    pub fn poll_event(&self, timeout: Duration) -> Result<Option<Event>> {
        if event::poll(timeout)? { Ok(Some(event::read()?)) } else { Ok(None) }
    }

    /// Restore the terminal.
    pub fn restore(&mut self) -> Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
        self.terminal.show_cursor()?;
        Ok(())
    }
}

impl Drop for ParallelTui {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_workstream(id: &str, desc: &str, status: TaskStatus) -> Workstream {
        Workstream::new(TaskId(id.to_string()), desc, status)
    }

    #[test]
    fn test_workstream_name_from_description() {
        assert_eq!(
            Workstream::name_from_description("Implement user authentication"),
            "implement-user"
        );
        assert_eq!(Workstream::name_from_description("Fix the login bug"), "fix-login");
        assert_eq!(
            Workstream::name_from_description("Add a new feature for testing"),
            "add-new"
        );
    }

    #[test]
    fn test_parallel_tui_state_default() {
        let state = ParallelTuiState::default();
        assert!(state.workstreams.is_empty());
        assert_eq!(state.primary_focus, 0);
        assert_eq!(state.layout_mode, LayoutMode::Dashboard);
        assert!(!state.show_help);
    }

    #[test]
    fn test_add_remove_workstream() {
        let mut state = ParallelTuiState::new();

        state.add_workstream(test_workstream("task-1", "Task one", TaskStatus::Running));
        state.add_workstream(test_workstream("task-2", "Task two", TaskStatus::Queued));

        assert_eq!(state.workstreams.len(), 2);
        assert_eq!(state.stats.running_count, 1);
        assert_eq!(state.stats.queued_count, 1);

        state.remove_workstream(&TaskId("task-1".to_string()));
        assert_eq!(state.workstreams.len(), 1);
    }

    #[test]
    fn test_navigation() {
        let mut state = ParallelTuiState::new();
        state.add_workstream(test_workstream("task-1", "Task one", TaskStatus::Running));
        state.add_workstream(test_workstream("task-2", "Task two", TaskStatus::Running));
        state.add_workstream(test_workstream("task-3", "Task three", TaskStatus::Running));

        assert_eq!(state.primary_focus, 0);

        state.next_workstream();
        assert_eq!(state.primary_focus, 1);

        state.next_workstream();
        assert_eq!(state.primary_focus, 2);

        state.next_workstream(); // Wrap around
        assert_eq!(state.primary_focus, 0);

        state.prev_workstream(); // Wrap around
        assert_eq!(state.primary_focus, 2);

        state.jump_to_workstream(2);
        assert_eq!(state.primary_focus, 1);
    }

    #[test]
    fn test_layout_toggle() {
        let mut state = ParallelTuiState::new();
        state.add_workstream(test_workstream("task-1", "Task one", TaskStatus::Running));
        state.add_workstream(test_workstream("task-2", "Task two", TaskStatus::Running));

        assert_eq!(state.layout_mode, LayoutMode::Dashboard);

        state.toggle_layout(LayoutMode::Split);
        assert_eq!(state.layout_mode, LayoutMode::Split);
        assert!(state.secondary_focus.is_some());

        state.toggle_layout(LayoutMode::Split); // Toggle off
        assert_eq!(state.layout_mode, LayoutMode::Dashboard);

        state.toggle_layout(LayoutMode::Focus);
        assert_eq!(state.layout_mode, LayoutMode::Focus);
    }

    #[test]
    fn test_sort_order_cycle() {
        let mut state = ParallelTuiState::new();
        assert_eq!(state.sort_order, SortOrder::Status);

        state.cycle_sort_order();
        assert_eq!(state.sort_order, SortOrder::Cost);

        state.cycle_sort_order();
        assert_eq!(state.sort_order, SortOrder::Activity);

        state.cycle_sort_order();
        assert_eq!(state.sort_order, SortOrder::Name);

        state.cycle_sort_order();
        assert_eq!(state.sort_order, SortOrder::Status);
    }

    #[test]
    fn test_output_buffer_limit() {
        let mut workstream = test_workstream("task-1", "Test", TaskStatus::Running);

        for i in 0..1500 {
            workstream.add_output(OutputLine::new(format!("Line {}", i)));
        }

        assert_eq!(workstream.output.len(), MAX_OUTPUT_LINES);
        assert!(workstream.output.front().unwrap().text.contains("500"));
    }

    #[test]
    fn test_budget_status() {
        let budget = BudgetStatus::new(5.0, 10.0);
        assert!(!budget.exceeded);
        assert!((budget.usage_percent - 0.5).abs() < 0.01);

        let exceeded = BudgetStatus::new(15.0, 10.0);
        assert!(exceeded.exceeded);
        assert!((exceeded.usage_percent - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_filter_workstreams() {
        let mut state = ParallelTuiState::new();
        state.add_workstream(test_workstream("task-1", "Auth feature", TaskStatus::Running));
        state.add_workstream(test_workstream("task-2", "API refactor", TaskStatus::Queued));
        state.add_workstream(test_workstream("task-3", "DB migration", TaskStatus::WaitingForUser));

        // No filter
        assert_eq!(state.filtered_workstreams().len(), 3);

        // Filter by status
        state.filter.status = Some(TaskStatus::Running);
        assert_eq!(state.filtered_workstreams().len(), 1);

        // Filter by name
        state.filter.status = None;
        state.filter.name_filter = Some("API".to_string());
        assert_eq!(state.filtered_workstreams().len(), 1);
    }

    #[test]
    fn test_timeline_event() {
        let mut state = ParallelTuiState::new();

        for i in 0..150 {
            state.add_timeline_event(TimelineEvent::new('●', format!("Event {}", i)));
        }

        assert_eq!(state.event_timeline.len(), 100);
    }

    #[test]
    fn test_app_key_handling() {
        let mut app = ParallelTuiApp::new();
        app.state
            .add_workstream(test_workstream("task-1", "Task one", TaskStatus::Running));
        app.state
            .add_workstream(test_workstream("task-2", "Task two", TaskStatus::Running));

        // Test navigation
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
        assert_eq!(app.state.primary_focus, 1);

        // Test help toggle
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)));
        assert!(app.state.show_help);

        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.state.show_help);

        // Test quit
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(app.should_quit);
    }
}
