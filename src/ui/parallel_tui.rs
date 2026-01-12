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

use crate::config::{TuiLayoutMode, TuiSettings, TuiSortOrder};
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

impl From<TuiLayoutMode> for LayoutMode {
    fn from(mode: TuiLayoutMode) -> Self {
        match mode {
            TuiLayoutMode::Dashboard => LayoutMode::Dashboard,
            TuiLayoutMode::Split => LayoutMode::Split,
            TuiLayoutMode::Grid => LayoutMode::Grid,
            TuiLayoutMode::Focus => LayoutMode::Focus,
        }
    }
}

impl From<LayoutMode> for TuiLayoutMode {
    fn from(mode: LayoutMode) -> Self {
        match mode {
            LayoutMode::Dashboard => TuiLayoutMode::Dashboard,
            LayoutMode::Split => TuiLayoutMode::Split,
            LayoutMode::Grid => TuiLayoutMode::Grid,
            LayoutMode::Focus => TuiLayoutMode::Focus,
        }
    }
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

impl From<TuiSortOrder> for SortOrder {
    fn from(order: TuiSortOrder) -> Self {
        match order {
            TuiSortOrder::Status => SortOrder::Status,
            TuiSortOrder::Cost => SortOrder::Cost,
            TuiSortOrder::Activity => SortOrder::Activity,
            TuiSortOrder::Name => SortOrder::Name,
        }
    }
}

impl From<SortOrder> for TuiSortOrder {
    fn from(order: SortOrder) -> Self {
        match order {
            SortOrder::Status => TuiSortOrder::Status,
            SortOrder::Cost => TuiSortOrder::Cost,
            SortOrder::Activity => TuiSortOrder::Activity,
            SortOrder::Name => TuiSortOrder::Name,
        }
    }
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

/// Attention level for workstreams based on idle time and status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum AttentionLevel {
    /// No attention needed - task is progressing normally.
    #[default]
    None,
    /// Subtle indicator - 30s idle for running tasks.
    Subtle,
    /// Warning - 60s idle, shown with yellow highlight.
    Warning,
    /// Urgent - 2min idle or WaitingForUser, pulsing animation.
    Urgent,
    /// Critical - user input required or task blocked.
    Critical,
}

impl AttentionLevel {
    /// Get attention level from idle duration and status.
    pub fn from_state(status: TaskStatus, idle: Duration, needs_attention: bool) -> Self {
        // Critical states override idle time
        if status == TaskStatus::WaitingForUser || needs_attention {
            return AttentionLevel::Critical;
        }
        if status == TaskStatus::Blocked {
            return AttentionLevel::Urgent;
        }

        // Only running tasks have idle-based attention
        if status != TaskStatus::Running {
            return AttentionLevel::None;
        }

        // Idle time thresholds
        let secs = idle.as_secs();
        if secs >= 120 {
            AttentionLevel::Urgent
        } else if secs >= 60 {
            AttentionLevel::Warning
        } else if secs >= 30 {
            AttentionLevel::Subtle
        } else {
            AttentionLevel::None
        }
    }

    /// Get the attention color.
    pub fn color(&self) -> Color {
        match self {
            AttentionLevel::None => Color::Reset,
            AttentionLevel::Subtle => Color::Rgb(100, 100, 100),
            AttentionLevel::Warning => Color::Rgb(255, 215, 0), // Gold
            AttentionLevel::Urgent => Color::Rgb(255, 140, 0),  // Dark orange
            AttentionLevel::Critical => Color::Rgb(255, 69, 0), // Red-orange
        }
    }

    /// Whether this attention level should pulse.
    pub fn should_pulse(&self) -> bool {
        matches!(self, AttentionLevel::Urgent | AttentionLevel::Critical)
    }
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

    /// Create timeline event from coordination event kind.
    pub fn from_event_kind(kind: &crate::coordination::EventKind, source: Option<&TaskId>) -> Self {
        use crate::coordination::EventKind;

        let (icon, description, importance) = match kind {
            EventKind::TaskStarted => ('●', "Task started", EventImportance::Normal),
            EventKind::TaskCompleted => ('✓', "Task completed", EventImportance::Normal),
            EventKind::TaskFailed => ('✗', "Task failed", EventImportance::High),
            EventKind::TaskCancelled => ('⊘', "Task cancelled", EventImportance::Normal),
            EventKind::TaskPaused => ('◑', "Task paused", EventImportance::Normal),
            EventKind::TaskWaitingForUser => ('?', "Waiting for user", EventImportance::Critical),
            EventKind::MainUpdated => ('↑', "Main branch updated", EventImportance::Normal),
            EventKind::RebaseRequired => ('↻', "Rebase required", EventImportance::High),
            EventKind::RebaseCompleted => ('✓', "Rebase completed", EventImportance::Normal),
            EventKind::RebaseConflict => ('⚠', "Rebase conflict", EventImportance::Critical),
            EventKind::NudgeSent => ('→', "Nudge sent", EventImportance::Low),
            EventKind::SyncRelayed => ('⚡', "Learning synced", EventImportance::Normal),
            EventKind::LearningExtracted => ('💡', "Learning extracted", EventImportance::Low),
            EventKind::LockAcquired => ('🔒', "Lock acquired", EventImportance::Low),
            EventKind::LockReleased => ('🔓', "Lock released", EventImportance::Low),
            EventKind::SupervisionDegraded => ('⚠', "Supervision degraded", EventImportance::High),
            EventKind::SupervisionRecovered => ('✓', "Supervision recovered", EventImportance::Normal),
            EventKind::RateLimitReached => ('⏱', "Rate limit reached", EventImportance::High),
            EventKind::FileModified => ('📝', "File modified", EventImportance::Low),
            EventKind::Custom(msg) => ('•', msg.as_str(), EventImportance::Normal),
        };

        let mut event = Self::new(icon, description).with_importance(importance);
        if let Some(task_id) = source {
            event = event.with_tasks(vec![task_id.clone()]);
        }
        event
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
    /// Target progress indicator (0.0-1.0) - the actual progress.
    pub progress: f32,
    /// Display progress for smooth animation (lerps toward progress).
    display_progress: f32,
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
            display_progress: 0.0,
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

    /// Set the target progress and start animating toward it.
    pub fn set_progress(&mut self, progress: f32) {
        self.progress = progress.clamp(0.0, 1.0);
    }

    /// Get the display progress (animated value).
    pub fn display_progress(&self) -> f32 {
        self.display_progress
    }

    /// Tick animation - call once per frame for smooth progress bar.
    /// Uses exponential lerp for natural-feeling animation.
    pub fn tick_animation(&mut self) {
        const LERP_FACTOR: f32 = 0.15;
        self.display_progress += (self.progress - self.display_progress) * LERP_FACTOR;
        // Snap to target when close enough to avoid drift
        if (self.progress - self.display_progress).abs() < 0.001 {
            self.display_progress = self.progress;
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

    /// Get the current attention level based on status and idle time.
    pub fn attention_level(&self) -> AttentionLevel {
        AttentionLevel::from_state(self.status, self.idle_duration, self.needs_attention)
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

/// Interaction mode for the TUI.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InteractionMode {
    /// Normal navigation mode.
    #[default]
    Normal,
    /// Input mode - sending text to a workstream.
    Input(String),
    /// Search/filter mode.
    Search(String),
    /// Confirmation dialog.
    Confirm(ConfirmDialog),
    /// Task-specific events viewer.
    TaskEvents,
}

/// Confirmation dialog for destructive actions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmDialog {
    /// Dialog title.
    pub title: String,
    /// Dialog message.
    pub message: String,
    /// Action to perform if confirmed.
    pub action: ConfirmAction,
    /// Currently selected option (true = confirm, false = cancel).
    pub selected: bool,
}

impl ConfirmDialog {
    /// Create a new confirmation dialog.
    pub fn new(title: impl Into<String>, message: impl Into<String>, action: ConfirmAction) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            action,
            selected: false, // Default to cancel for safety
        }
    }

    /// Create a cancel task dialog.
    pub fn cancel_task(task_id: &TaskId) -> Self {
        Self::new(
            "Cancel Task",
            format!("Are you sure you want to cancel task '{}'?", task_id.0),
            ConfirmAction::CancelTask(task_id.clone()),
        )
    }

    /// Create a quit dialog.
    pub fn quit_with_running() -> Self {
        Self::new(
            "Quit",
            "There are still running tasks. Are you sure you want to quit?",
            ConfirmAction::Quit,
        )
    }
}

/// Action to perform after confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Cancel a task.
    CancelTask(TaskId),
    /// Quit the TUI.
    Quit,
    /// Pause a task.
    PauseTask(TaskId),
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
    /// Frame counter for animations (wraps at 60).
    frame_counter: u8,
    /// Current interaction mode.
    pub interaction_mode: InteractionMode,
}

impl Default for ParallelTuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl ParallelTuiState {
    /// Create a new parallel TUI state with default settings.
    pub fn new() -> Self {
        Self::from_settings(&TuiSettings::default())
    }

    /// Create a new parallel TUI state from config settings.
    pub fn from_settings(settings: &TuiSettings) -> Self {
        Self {
            workstreams: Vec::new(),
            primary_focus: 0,
            secondary_focus: None,
            layout_mode: settings.default_layout.into(),
            event_timeline: VecDeque::with_capacity(settings.max_timeline_events),
            stats: GlobalStats::default(),
            budget: BudgetStatus::default(),
            notifications: VecDeque::with_capacity(50),
            show_help: false,
            filter: WorkstreamFilter::default(),
            sort_order: settings.default_sort.into(),
            connection_status: ConnectionStatus::Connected,
            max_timeline_events: settings.max_timeline_events,
            frame_counter: 0,
            interaction_mode: InteractionMode::Normal,
        }
    }

    /// Enter input mode for the selected workstream.
    pub fn enter_input_mode(&mut self) {
        self.interaction_mode = InteractionMode::Input(String::new());
    }

    /// Enter search mode.
    pub fn enter_search_mode(&mut self) {
        self.interaction_mode = InteractionMode::Search(String::new());
    }

    /// Enter task events view for the selected workstream.
    pub fn enter_task_events_mode(&mut self) {
        self.interaction_mode = InteractionMode::TaskEvents;
    }

    /// Show a confirmation dialog.
    pub fn show_confirm(&mut self, dialog: ConfirmDialog) {
        self.interaction_mode = InteractionMode::Confirm(dialog);
    }

    /// Exit current interaction mode back to normal.
    pub fn exit_interaction_mode(&mut self) {
        self.interaction_mode = InteractionMode::Normal;
    }

    /// Check if in normal navigation mode.
    pub fn is_normal_mode(&self) -> bool {
        matches!(self.interaction_mode, InteractionMode::Normal)
    }

    /// Apply current search filter.
    pub fn apply_search(&mut self) {
        if let InteractionMode::Search(ref query) = self.interaction_mode {
            if query.is_empty() {
                self.filter.name_filter = None;
            } else {
                self.filter.name_filter = Some(query.clone());
            }
        }
    }

    /// Advance the frame counter (call once per frame for animations).
    pub fn tick(&mut self) {
        self.frame_counter = self.frame_counter.wrapping_add(1) % 60;
    }

    /// Get current pulse intensity (0.0-1.0) for pulsing animations.
    /// Uses a sine wave over 60 frames for smooth pulsing.
    pub fn pulse_intensity(&self) -> f32 {
        let t = (self.frame_counter as f32) / 60.0 * std::f32::consts::TAU;
        (t.sin() + 1.0) / 2.0
    }

    /// Check if pulse is in the "on" phase (intensity > 0.5).
    pub fn pulse_on(&self) -> bool {
        self.pulse_intensity() > 0.5
    }

    /// Get the current layout as a config type (for persistence).
    pub fn layout_as_config(&self) -> TuiLayoutMode {
        self.layout_mode.into()
    }

    /// Get the current sort order as a config type (for persistence).
    pub fn sort_order_as_config(&self) -> TuiSortOrder {
        self.sort_order.into()
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
    /// Create a new parallel TUI app with default settings.
    pub fn new() -> Self {
        Self {
            state: ParallelTuiState::new(),
            should_quit: false,
        }
    }

    /// Create a new parallel TUI app from config settings.
    pub fn from_settings(settings: &TuiSettings) -> Self {
        Self {
            state: ParallelTuiState::from_settings(settings),
            should_quit: false,
        }
    }

    /// Tick all animations - call once per frame.
    pub fn tick(&mut self) {
        self.state.tick();
        for workstream in &mut self.state.workstreams {
            workstream.tick_animation();
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
        // Handle Ctrl+C always
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }

        // Handle based on current interaction mode
        match &self.state.interaction_mode {
            InteractionMode::Normal => self.handle_normal_mode(key),
            InteractionMode::Input(_) => self.handle_input_mode(key),
            InteractionMode::Search(_) => self.handle_search_mode(key),
            InteractionMode::Confirm(_) => self.handle_confirm_mode(key),
            InteractionMode::TaskEvents => self.handle_task_events_mode(key),
        }
    }

    /// Handle keys in normal navigation mode.
    fn handle_normal_mode(&mut self, key: KeyEvent) {
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
            // Quit (with confirm if tasks running)
            KeyCode::Char('q') => {
                if self.state.stats.running_count > 0 {
                    self.state.show_confirm(ConfirmDialog::quit_with_running());
                } else {
                    self.should_quit = true;
                }
            }

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

            // Interaction modes
            KeyCode::Char('a') => self.state.enter_input_mode(),
            KeyCode::Char('/') => self.state.enter_search_mode(),
            KeyCode::Char('t') => self.state.enter_task_events_mode(),

            // Actions
            KeyCode::Char('x') => {
                // Cancel selected task (with confirm)
                if let Some(workstream) = self.state.selected() {
                    self.state.show_confirm(ConfirmDialog::cancel_task(&workstream.task_id));
                }
            }
            KeyCode::Char('f') => {
                // Toggle attention filter
                self.state.filter.attention_only = !self.state.filter.attention_only;
            }

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
                if self.state.layout_mode == LayoutMode::Split
                    && let Some(secondary) = self.state.secondary_focus
                {
                    self.state.secondary_focus = Some(self.state.primary_focus);
                    self.state.primary_focus = secondary;
                }
            }

            _ => {}
        }
    }

    /// Handle keys in input mode.
    fn handle_input_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.state.exit_interaction_mode(),
            KeyCode::Enter => {
                // Send input to workstream (placeholder - daemon integration needed)
                // For now, just exit input mode
                self.state.exit_interaction_mode();
            }
            KeyCode::Backspace => {
                if let InteractionMode::Input(ref mut text) = self.state.interaction_mode {
                    text.pop();
                }
            }
            KeyCode::Char(c) => {
                if let InteractionMode::Input(ref mut text) = self.state.interaction_mode {
                    text.push(c);
                }
            }
            _ => {}
        }
    }

    /// Handle keys in search mode.
    fn handle_search_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state.filter.name_filter = None;
                self.state.exit_interaction_mode();
            }
            KeyCode::Enter => {
                self.state.apply_search();
                self.state.exit_interaction_mode();
            }
            KeyCode::Backspace => {
                if let InteractionMode::Search(ref mut text) = self.state.interaction_mode {
                    text.pop();
                    // Apply search in real-time
                    self.state.filter.name_filter = if text.is_empty() { None } else { Some(text.clone()) };
                }
            }
            KeyCode::Char(c) => {
                if let InteractionMode::Search(ref mut text) = self.state.interaction_mode {
                    text.push(c);
                    // Apply search in real-time
                    self.state.filter.name_filter = Some(text.clone());
                }
            }
            _ => {}
        }
    }

    /// Handle keys in confirmation dialog.
    fn handle_confirm_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.state.exit_interaction_mode();
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                // Execute the confirmed action
                if let InteractionMode::Confirm(ref dialog) = self.state.interaction_mode {
                    match &dialog.action {
                        ConfirmAction::Quit => self.should_quit = true,
                        ConfirmAction::CancelTask(_task_id) => {
                            // Placeholder - daemon integration needed
                        }
                        ConfirmAction::PauseTask(_task_id) => {
                            // Placeholder - daemon integration needed
                        }
                    }
                }
                self.state.exit_interaction_mode();
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                // Toggle selection
                if let InteractionMode::Confirm(ref mut dialog) = self.state.interaction_mode {
                    dialog.selected = !dialog.selected;
                }
            }
            _ => {}
        }
    }

    /// Handle keys in task events mode.
    fn handle_task_events_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('t') => {
                self.state.exit_interaction_mode();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                // Scroll down in events (placeholder)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                // Scroll up in events (placeholder)
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

        // Render interaction mode overlays
        match &self.state.interaction_mode {
            InteractionMode::Input(text) => self.render_input_bar(frame, text),
            InteractionMode::Search(text) => self.render_search_bar(frame, text),
            InteractionMode::Confirm(dialog) => self.render_confirm_dialog(frame, dialog),
            InteractionMode::TaskEvents => self.render_task_events_overlay(frame),
            InteractionMode::Normal => {}
        }
    }

    /// Render input bar at bottom of screen.
    fn render_input_bar(&self, frame: &mut Frame, text: &str) {
        let area = frame.area();
        let bar_area = Rect::new(0, area.height.saturating_sub(2), area.width, 2);

        let task_name = self.state.selected().map(|w| w.name.as_str()).unwrap_or("none");

        let block = Block::default()
            .title(format!(" Send to [{}] ", task_name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(bar_area);

        // Clear the area first
        frame.render_widget(Clear, bar_area);
        frame.render_widget(block, bar_area);

        // Render input text with cursor
        let input = Paragraph::new(format!("{}_", text)).style(Style::default().fg(Color::White));
        frame.render_widget(input, inner);
    }

    /// Render search bar at bottom of screen.
    fn render_search_bar(&self, frame: &mut Frame, text: &str) {
        let area = frame.area();
        let bar_area = Rect::new(0, area.height.saturating_sub(2), area.width, 2);

        let matches = self.state.filtered_workstreams().len();

        let block = Block::default()
            .title(format!(" Search ({} matches) ", matches))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let inner = block.inner(bar_area);

        frame.render_widget(Clear, bar_area);
        frame.render_widget(block, bar_area);

        let input = Paragraph::new(format!("/{}_", text)).style(Style::default().fg(Color::White));
        frame.render_widget(input, inner);
    }

    /// Render confirmation dialog.
    fn render_confirm_dialog(&self, frame: &mut Frame, dialog: &ConfirmDialog) {
        let area = frame.area();
        let dialog_width = 50.min(area.width.saturating_sub(4));
        let dialog_height = 6;

        let x = (area.width.saturating_sub(dialog_width)) / 2;
        let y = (area.height.saturating_sub(dialog_height)) / 2;
        let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

        let block = Block::default()
            .title(format!(" {} ", dialog.title))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red));

        let inner = block.inner(dialog_area);

        frame.render_widget(Clear, dialog_area);
        frame.render_widget(block, dialog_area);

        // Message and buttons layout
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let message = Paragraph::new(&*dialog.message).wrap(Wrap { trim: true });
        frame.render_widget(message, layout[0]);

        // Buttons
        let yes_style = if dialog.selected {
            Style::default().fg(Color::White).bg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let no_style = if !dialog.selected {
            Style::default().fg(Color::White).bg(Color::Blue)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let buttons = Line::from(vec![
            Span::styled(" [Y]es ", yes_style),
            Span::raw("  "),
            Span::styled(" [N]o ", no_style),
        ]);
        frame.render_widget(
            Paragraph::new(buttons).alignment(ratatui::layout::Alignment::Center),
            layout[1],
        );
    }

    /// Render task events overlay.
    fn render_task_events_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        let overlay_width = (area.width as f32 * 0.6) as u16;
        let overlay_height = (area.height as f32 * 0.7) as u16;

        let x = (area.width.saturating_sub(overlay_width)) / 2;
        let y = (area.height.saturating_sub(overlay_height)) / 2;
        let overlay_area = Rect::new(x, y, overlay_width, overlay_height);

        let task_name = self.state.selected().map(|w| w.name.as_str()).unwrap_or("none");

        let block = Block::default()
            .title(format!(" Events for [{}] (press t/Esc to close) ", task_name))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(overlay_area);

        frame.render_widget(Clear, overlay_area);
        frame.render_widget(block, overlay_area);

        // Get events for selected task
        let selected_task = self.state.selected().map(|w| &w.task_id);
        let events: Vec<_> = self
            .state
            .event_timeline
            .iter()
            .filter(|e| {
                if let Some(task_id) = selected_task {
                    e.task_ids.contains(task_id) || e.task_ids.is_empty()
                } else {
                    true
                }
            })
            .collect();

        let items: Vec<ListItem> = events
            .iter()
            .rev()
            .take(inner.height as usize)
            .map(|e| {
                let time = e.timestamp.format("%H:%M:%S").to_string();
                let line = Line::from(vec![
                    Span::styled(format!("{} ", e.icon), Style::default().fg(Color::Cyan)),
                    Span::styled(time, Style::default().fg(Color::DarkGray)),
                    Span::raw(" "),
                    Span::raw(&e.description),
                ]);
                ListItem::new(line)
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, inner);
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
        let pulse_on = self.state.pulse_on();

        let items: Vec<ListItem> = filtered
            .iter()
            .enumerate()
            .map(|(i, workstream)| {
                let attention = workstream.attention_level();
                let should_show_attention = !attention.should_pulse() || pulse_on;

                // Status icon with color
                let icon = Span::styled(workstream.status_icon(), Style::default().fg(workstream.status_color()));
                let index = Span::styled(format!("[{}]", i + 1), Style::default().fg(Color::DarkGray));

                // Name styling based on attention level
                let name_style = if should_show_attention && attention >= AttentionLevel::Warning {
                    Style::default().fg(attention.color()).add_modifier(Modifier::BOLD)
                } else if attention == AttentionLevel::Subtle {
                    Style::default().fg(Color::Rgb(200, 200, 200))
                } else {
                    Style::default()
                };
                let name = Span::styled(&workstream.name, name_style);

                // Attention indicator suffix
                let attention_suffix = if should_show_attention {
                    match attention {
                        AttentionLevel::Critical => Span::styled(
                            " !",
                            Style::default().fg(attention.color()).add_modifier(Modifier::BOLD),
                        ),
                        AttentionLevel::Urgent => Span::styled(" ⚠", Style::default().fg(attention.color())),
                        AttentionLevel::Warning => Span::styled(" •", Style::default().fg(attention.color())),
                        _ => Span::raw(""),
                    }
                } else {
                    Span::raw("")
                };

                let line = Line::from(vec![index, " ".into(), icon, " ".into(), name, attention_suffix]);
                ListItem::new(line)
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.state.primary_focus));

        // Count tasks needing attention
        let attention_count = filtered
            .iter()
            .filter(|w| w.attention_level() >= AttentionLevel::Warning)
            .count();

        let title = if attention_count > 0 {
            format!(" Workstreams ({}) [{}!] ", filtered.len(), attention_count)
        } else {
            format!(" Workstreams ({}) ", filtered.len())
        };

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

        // Show mode-specific status
        let mode_indicator = match &self.state.interaction_mode {
            InteractionMode::Normal => "".into(),
            InteractionMode::Input(_) => " [INPUT] ".yellow().bold(),
            InteractionMode::Search(_) => " [SEARCH] ".cyan().bold(),
            InteractionMode::Confirm(_) => " [CONFIRM] ".red().bold(),
            InteractionMode::TaskEvents => " [EVENTS] ".blue().bold(),
        };

        // Filter indicator
        let filter_indicator = if self.state.filter.name_filter.is_some() || self.state.filter.attention_only {
            " [FILTERED]".yellow()
        } else {
            "".into()
        };

        // Build status line with spans for proper rendering
        let status_line = Line::from(vec![
            " ".into(),
            connection_indicator,
            " ".into(),
            Span::styled(mode_text, Style::default().fg(Color::Cyan)),
            " │ Sort: ".dark_gray(),
            Span::styled(sort_text, Style::default().fg(Color::White)),
            mode_indicator,
            filter_indicator,
            " │ ".dark_gray(),
            "?".cyan(),
            " Help ".dark_gray(),
            "q".cyan(),
            " Quit".dark_gray(),
        ]);

        let style = Style::default().bg(Color::Rgb(30, 30, 30));
        let paragraph = Paragraph::new(status_line).style(style);
        frame.render_widget(paragraph, area);
    }

    /// Render help popup overlay.
    fn render_help_popup(&self, frame: &mut Frame) {
        let area = centered_rect(65, 80, frame.area());
        frame.render_widget(Clear, area);

        let help_text = vec![
            Line::from(vec![
                "─────────── ".dark_gray(),
                "Navigation".bold().cyan(),
                " ───────────".dark_gray(),
            ]),
            Line::from(vec!["  1-9".cyan(), " │ Jump to workstream by index".into()]),
            Line::from(vec!["  j/↓".cyan(), " │ Next workstream".into()]),
            Line::from(vec!["  k/↑".cyan(), " │ Previous workstream".into()]),
            Line::from(vec!["  Tab".cyan(), " │ Cycle focus in split mode".into()]),
            Line::from(""),
            Line::from(vec![
                "─────────── ".dark_gray(),
                "Layout Modes".bold().cyan(),
                " ─────────".dark_gray(),
            ]),
            Line::from(vec!["    d".cyan(), " │ Dashboard (default)".into()]),
            Line::from(vec!["Space".cyan(), " │ Toggle split view".into()]),
            Line::from(vec!["    g".cyan(), " │ Toggle grid view".into()]),
            Line::from(vec!["Enter".cyan(), " │ Toggle focus mode".into()]),
            Line::from(""),
            Line::from(vec![
                "─────────── ".dark_gray(),
                "Interaction".bold().cyan(),
                " ──────────".dark_gray(),
            ]),
            Line::from(vec!["    a".cyan(), " │ Send input to workstream".into()]),
            Line::from(vec!["    /".cyan(), " │ Search/filter workstreams".into()]),
            Line::from(vec!["    t".cyan(), " │ View task events".into()]),
            Line::from(vec!["    f".cyan(), " │ Toggle attention filter".into()]),
            Line::from(""),
            Line::from(vec![
                "─────────── ".dark_gray(),
                "Actions".bold().cyan(),
                " ───────────────".dark_gray(),
            ]),
            Line::from(vec!["    s".cyan(), " │ Cycle sort order".into()]),
            Line::from(vec!["    x".cyan(), " │ Cancel task (with confirm)".into()]),
            Line::from(vec!["    ?".cyan(), " │ Toggle help".into()]),
            Line::from(vec!["    q".cyan(), " │ Quit (confirm if running)".into()]),
            Line::from(vec!["Esc  ".cyan(), " │ Exit mode / Clear filter".into()]),
            Line::from(vec!["Ctrl+C".cyan(), "│ Force quit".into()]),
            Line::from(""),
            Line::from(vec![
                "─────────── ".dark_gray(),
                "Mode Indicators".bold().cyan(),
                " ────────".dark_gray(),
            ]),
            Line::from(vec!["  ●".green(), " Running".into()]),
            Line::from(vec!["  ?".yellow(), " Waiting for user".into()]),
            Line::from(vec!["  ○".dark_gray(), " Queued".into()]),
            Line::from(vec!["  ✓".green(), " Completed".into()]),
            Line::from(vec!["  ✗".red(), " Failed".into()]),
            Line::from(""),
            Line::from("Press ESC or ? to close".dark_gray().italic()),
        ];

        let block = Block::default()
            .title(" Help ")
            .title_style(Style::default().bold().cyan())
            .borders(Borders::ALL)
            .border_set(border::DOUBLE)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Rgb(20, 20, 30)));

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

        // Test quit - with running tasks, should show confirm dialog
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(matches!(app.state.interaction_mode, InteractionMode::Confirm(_)));
        // Confirm quit
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert!(app.should_quit);
    }

    #[test]
    fn test_from_tui_settings() {
        let settings = TuiSettings {
            default_layout: TuiLayoutMode::Split,
            default_sort: TuiSortOrder::Cost,
            max_timeline_events: 50,
            ..Default::default()
        };

        let state = ParallelTuiState::from_settings(&settings);
        assert_eq!(state.layout_mode, LayoutMode::Split);
        assert_eq!(state.sort_order, SortOrder::Cost);
        assert_eq!(state.max_timeline_events, 50);
    }

    #[test]
    fn test_layout_mode_conversion() {
        // Test From<TuiLayoutMode> for LayoutMode
        assert_eq!(LayoutMode::from(TuiLayoutMode::Dashboard), LayoutMode::Dashboard);
        assert_eq!(LayoutMode::from(TuiLayoutMode::Split), LayoutMode::Split);
        assert_eq!(LayoutMode::from(TuiLayoutMode::Grid), LayoutMode::Grid);
        assert_eq!(LayoutMode::from(TuiLayoutMode::Focus), LayoutMode::Focus);

        // Test From<LayoutMode> for TuiLayoutMode
        assert_eq!(TuiLayoutMode::from(LayoutMode::Dashboard), TuiLayoutMode::Dashboard);
        assert_eq!(TuiLayoutMode::from(LayoutMode::Split), TuiLayoutMode::Split);
        assert_eq!(TuiLayoutMode::from(LayoutMode::Grid), TuiLayoutMode::Grid);
        assert_eq!(TuiLayoutMode::from(LayoutMode::Focus), TuiLayoutMode::Focus);
    }

    #[test]
    fn test_sort_order_conversion() {
        // Test From<TuiSortOrder> for SortOrder
        assert_eq!(SortOrder::from(TuiSortOrder::Status), SortOrder::Status);
        assert_eq!(SortOrder::from(TuiSortOrder::Cost), SortOrder::Cost);
        assert_eq!(SortOrder::from(TuiSortOrder::Activity), SortOrder::Activity);
        assert_eq!(SortOrder::from(TuiSortOrder::Name), SortOrder::Name);

        // Test From<SortOrder> for TuiSortOrder
        assert_eq!(TuiSortOrder::from(SortOrder::Status), TuiSortOrder::Status);
        assert_eq!(TuiSortOrder::from(SortOrder::Cost), TuiSortOrder::Cost);
        assert_eq!(TuiSortOrder::from(SortOrder::Activity), TuiSortOrder::Activity);
        assert_eq!(TuiSortOrder::from(SortOrder::Name), TuiSortOrder::Name);
    }

    #[test]
    fn test_layout_as_config() {
        let mut state = ParallelTuiState::new();
        assert_eq!(state.layout_as_config(), TuiLayoutMode::Dashboard);

        state.toggle_layout(LayoutMode::Grid);
        assert_eq!(state.layout_as_config(), TuiLayoutMode::Grid);
    }

    #[test]
    fn test_sort_order_as_config() {
        let mut state = ParallelTuiState::new();
        assert_eq!(state.sort_order_as_config(), TuiSortOrder::Status);

        state.cycle_sort_order();
        assert_eq!(state.sort_order_as_config(), TuiSortOrder::Cost);
    }

    #[test]
    fn test_app_from_settings() {
        let settings = TuiSettings {
            default_layout: TuiLayoutMode::Focus,
            default_sort: TuiSortOrder::Activity,
            ..Default::default()
        };

        let app = ParallelTuiApp::from_settings(&settings);
        assert_eq!(app.state.layout_mode, LayoutMode::Focus);
        assert_eq!(app.state.sort_order, SortOrder::Activity);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_attention_level_from_state() {
        // Running task with no idle time = None
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Running, Duration::ZERO, false),
            AttentionLevel::None
        );

        // Running task 30s idle = Subtle
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Running, Duration::from_secs(30), false),
            AttentionLevel::Subtle
        );

        // Running task 60s idle = Warning
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Running, Duration::from_secs(60), false),
            AttentionLevel::Warning
        );

        // Running task 120s idle = Urgent
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Running, Duration::from_secs(120), false),
            AttentionLevel::Urgent
        );

        // WaitingForUser always Critical
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::WaitingForUser, Duration::ZERO, false),
            AttentionLevel::Critical
        );

        // Blocked = Urgent
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Blocked, Duration::ZERO, false),
            AttentionLevel::Urgent
        );

        // needs_attention flag = Critical
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Running, Duration::ZERO, true),
            AttentionLevel::Critical
        );

        // Completed tasks don't get attention
        assert_eq!(
            AttentionLevel::from_state(TaskStatus::Completed, Duration::from_secs(120), false),
            AttentionLevel::None
        );
    }

    #[test]
    fn test_attention_level_color_and_pulse() {
        assert!(!AttentionLevel::None.should_pulse());
        assert!(!AttentionLevel::Subtle.should_pulse());
        assert!(!AttentionLevel::Warning.should_pulse());
        assert!(AttentionLevel::Urgent.should_pulse());
        assert!(AttentionLevel::Critical.should_pulse());

        // Just check colors don't panic
        let _ = AttentionLevel::None.color();
        let _ = AttentionLevel::Warning.color();
        let _ = AttentionLevel::Critical.color();
    }

    #[test]
    fn test_workstream_attention_level() {
        let mut workstream = test_workstream("task-1", "Test", TaskStatus::Running);
        assert_eq!(workstream.attention_level(), AttentionLevel::None);

        // Simulate 60s idle
        workstream.idle_duration = Duration::from_secs(60);
        assert_eq!(workstream.attention_level(), AttentionLevel::Warning);

        // Change status to WaitingForUser
        workstream.status = TaskStatus::WaitingForUser;
        assert_eq!(workstream.attention_level(), AttentionLevel::Critical);
    }

    #[test]
    fn test_pulse_animation() {
        let mut state = ParallelTuiState::new();

        // Tick through frames and verify pulse changes
        let mut saw_on = false;
        let mut saw_off = false;
        for _ in 0..60 {
            if state.pulse_on() {
                saw_on = true;
            } else {
                saw_off = true;
            }
            state.tick();
        }

        assert!(saw_on, "Pulse should be on at some point");
        assert!(saw_off, "Pulse should be off at some point");
    }

    #[test]
    fn test_timeline_event_from_event_kind() {
        use crate::coordination::EventKind;

        let event = TimelineEvent::from_event_kind(&EventKind::TaskFailed, None);
        assert_eq!(event.icon, '✗');
        assert_eq!(event.importance, EventImportance::High);
        assert!(event.task_ids.is_empty());

        let task_id = TaskId("test-task".to_string());
        let event_with_task = TimelineEvent::from_event_kind(&EventKind::TaskStarted, Some(&task_id));
        assert_eq!(event_with_task.task_ids, vec![task_id]);
    }

    #[test]
    fn test_attention_level_ordering() {
        // Test that attention levels are ordered correctly
        assert!(AttentionLevel::None < AttentionLevel::Subtle);
        assert!(AttentionLevel::Subtle < AttentionLevel::Warning);
        assert!(AttentionLevel::Warning < AttentionLevel::Urgent);
        assert!(AttentionLevel::Urgent < AttentionLevel::Critical);
    }

    #[test]
    fn test_interaction_mode_default() {
        let state = ParallelTuiState::new();
        assert_eq!(state.interaction_mode, InteractionMode::Normal);
        assert!(state.is_normal_mode());
    }

    #[test]
    fn test_enter_input_mode() {
        let mut state = ParallelTuiState::new();
        state.enter_input_mode();
        assert!(matches!(state.interaction_mode, InteractionMode::Input(_)));
        assert!(!state.is_normal_mode());

        state.exit_interaction_mode();
        assert_eq!(state.interaction_mode, InteractionMode::Normal);
    }

    #[test]
    fn test_enter_search_mode() {
        let mut state = ParallelTuiState::new();
        state.add_workstream(test_workstream("task-1", "Auth feature", TaskStatus::Running));
        state.add_workstream(test_workstream("task-2", "API refactor", TaskStatus::Queued));

        state.enter_search_mode();
        assert!(matches!(state.interaction_mode, InteractionMode::Search(_)));

        // Simulate typing
        if let InteractionMode::Search(ref mut text) = state.interaction_mode {
            text.push_str("API");
        }
        state.apply_search();

        // Search should filter results
        assert_eq!(state.filter.name_filter, Some("API".to_string()));
        assert_eq!(state.filtered_workstreams().len(), 1);
    }

    #[test]
    fn test_confirm_dialog() {
        let dialog = ConfirmDialog::cancel_task(&TaskId("test-task".to_string()));
        assert_eq!(dialog.title, "Cancel Task");
        assert!(dialog.message.contains("test-task"));
        assert!(!dialog.selected); // Default to cancel for safety
    }

    #[test]
    fn test_show_confirm() {
        let mut state = ParallelTuiState::new();
        state.show_confirm(ConfirmDialog::quit_with_running());

        assert!(matches!(state.interaction_mode, InteractionMode::Confirm(_)));
        if let InteractionMode::Confirm(ref dialog) = state.interaction_mode {
            assert_eq!(dialog.title, "Quit");
        }
    }

    #[test]
    fn test_task_events_mode() {
        let mut state = ParallelTuiState::new();
        state.enter_task_events_mode();
        assert!(matches!(state.interaction_mode, InteractionMode::TaskEvents));

        state.exit_interaction_mode();
        assert!(state.is_normal_mode());
    }

    #[test]
    fn test_input_mode_key_handling() {
        let mut app = ParallelTuiApp::new();
        app.state
            .add_workstream(test_workstream("task-1", "Test", TaskStatus::Running));

        // Enter input mode
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(matches!(app.state.interaction_mode, InteractionMode::Input(_)));

        // Type some text
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)));
        if let InteractionMode::Input(ref text) = app.state.interaction_mode {
            assert_eq!(text, "hi");
        }

        // Backspace
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        if let InteractionMode::Input(ref text) = app.state.interaction_mode {
            assert_eq!(text, "h");
        }

        // Escape to exit
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.state.is_normal_mode());
    }

    #[test]
    fn test_search_mode_key_handling() {
        let mut app = ParallelTuiApp::new();
        app.state
            .add_workstream(test_workstream("task-1", "Auth feature", TaskStatus::Running));
        app.state
            .add_workstream(test_workstream("task-2", "API refactor", TaskStatus::Queued));

        // Enter search mode
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)));
        assert!(matches!(app.state.interaction_mode, InteractionMode::Search(_)));

        // Type search query
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE)));
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('I'), KeyModifiers::NONE)));

        // Real-time filtering
        assert_eq!(app.state.filtered_workstreams().len(), 1);

        // Enter to confirm
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.state.is_normal_mode());
        assert_eq!(app.state.filter.name_filter, Some("API".to_string()));
    }

    #[test]
    fn test_confirm_dialog_key_handling() {
        let mut app = ParallelTuiApp::new();
        app.state.stats.running_count = 1; // Simulate running task

        // Try to quit - should show confirm
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(matches!(app.state.interaction_mode, InteractionMode::Confirm(_)));

        // Press N to cancel
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)));
        assert!(app.state.is_normal_mode());
        assert!(!app.should_quit);

        // Show dialog again and confirm with Y
        app.state.show_confirm(ConfirmDialog::quit_with_running());
        app.handle_event(Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert!(app.should_quit);
    }
}
