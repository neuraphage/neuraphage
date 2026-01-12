//! TUI module for Neuraphage.
//!
//! Provides:
//! - Single-task TUI dashboard using ratatui
//! - Multi-workstream TUI for parallel task orchestration
//! - TUI runner for daemon-connected event loop

pub mod multi;
pub mod runner;
pub mod single;

pub use multi::{
    AttentionLevel, BudgetStatus, ConfirmAction, ConfirmDialog, ConnectionStatus, EventImportance, GlobalStats,
    InteractionMode, LayoutMode, OutputLine, ParallelTui, ParallelTuiApp, ParallelTuiState, SortOrder, TimelineEvent,
    Workstream, WorkstreamFilter,
};
pub use runner::TuiRunner;
pub use single::{App, AppState, Tui};
