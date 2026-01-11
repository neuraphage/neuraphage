//! User interface module for Neuraphage.
//!
//! Provides:
//! - TUI dashboard using ratatui
//! - Notification system (terminal title, etc.)
//! - UI state management

pub mod notifications;
pub mod tui;

pub use notifications::{Notification, NotificationKind, Notifier};
pub use tui::{App, AppState, Tui};
