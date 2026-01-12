//! User interface module for Neuraphage.
//!
//! Provides:
//! - TUI dashboard using ratatui
//! - Parallel workstream TUI for multi-task orchestration
//! - Notification system (terminal title, etc.)
//! - UI state management

pub mod notifications;
pub mod parallel_tui;
pub mod tui;

pub use notifications::{Notification, NotificationKind, Notifier};
pub use parallel_tui::{
    AttentionLevel, BudgetStatus, ConfirmAction, ConfirmDialog, ConnectionStatus, EventImportance, GlobalStats,
    InteractionMode, LayoutMode, OutputLine, ParallelTui, ParallelTuiApp, ParallelTuiState, SortOrder, TimelineEvent,
    Workstream, WorkstreamFilter,
};
pub use tui::{App, AppState, Tui};
