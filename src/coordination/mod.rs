//! Multi-task coordination module.
//!
//! Provides shared infrastructure for coordinating multiple concurrent tasks:
//! - Scheduler: Priority queue with rate limit management
//! - Lock manager: Resource locking with deadlock prevention
//! - Event bus: Inter-task communication
//! - Knowledge store: Learning extraction and injection

pub mod event_bus;
pub mod knowledge;
pub mod lock_manager;
pub mod scheduler;

pub use event_bus::{Event, EventBus, EventKind};
pub use knowledge::{Knowledge, KnowledgeKind, KnowledgeStore};
pub use lock_manager::{Lock, LockManager, ResourceId};
pub use scheduler::{ScheduleResult, Scheduler, SchedulerConfig};
