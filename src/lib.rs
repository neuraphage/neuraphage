//! Neuraphage: Multi-task AI orchestrator daemon.
//!
//! Neuraphage manages N concurrent AI agent tasks within a single daemon process,
//! using engram for task graph persistence.

pub mod config;
pub mod daemon;
pub mod error;
pub mod task;
pub mod task_manager;

pub use config::Config;
pub use daemon::{Daemon, DaemonConfig};
pub use error::{Error, Result};
pub use task::{Task, TaskId, TaskStatus};
pub use task_manager::TaskManager;
