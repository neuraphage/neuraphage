//! Neuraphage: Multi-task AI orchestrator daemon.
//!
//! Neuraphage manages N concurrent AI agent tasks within a single daemon process,
//! using engram for task graph persistence.

pub mod agentic;
pub mod config;
pub mod coordination;
pub mod daemon;
pub mod error;
pub mod task;
pub mod task_manager;
pub mod ui;

pub use config::Config;
pub use daemon::{Daemon, DaemonConfig};
pub use error::{Error, Result};
pub use task::{Task, TaskId, TaskStatus};
pub use task_manager::TaskManager;

// Re-export agentic types
pub use agentic::{AgenticConfig, AgenticLoop, Conversation, IterationResult, Message, MessageRole};
pub use agentic::{LlmClient, LlmConfig, LlmResponse};
pub use agentic::{Tool, ToolCall, ToolExecutor, ToolResult};

// Re-export coordination types
pub use coordination::{Event, EventBus, EventKind};
pub use coordination::{Knowledge, KnowledgeKind, KnowledgeStore};
pub use coordination::{Lock, LockManager, ResourceId};
pub use coordination::{ScheduleResult, Scheduler, SchedulerConfig};

// Re-export UI types
pub use ui::{App, AppState, Notification, NotificationKind, Notifier, Tui};
