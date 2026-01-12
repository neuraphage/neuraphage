//! TUI module for Neuraphage.
//!
//! Provides:
//! - Single-task TUI dashboard using ratatui
//! - Multi-workstream TUI for parallel task orchestration

pub mod multi;
pub mod single;

pub use multi::{
    AttentionLevel, BudgetStatus, ConfirmAction, ConfirmDialog, ConnectionStatus, EventImportance, GlobalStats,
    InteractionMode, LayoutMode, OutputLine, ParallelTui, ParallelTuiApp, ParallelTuiState, SortOrder, TimelineEvent,
    Workstream, WorkstreamFilter,
};
pub use single::{App, AppState, Tui};
