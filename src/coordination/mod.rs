//! Multi-task coordination module.
//!
//! Provides shared infrastructure for coordinating multiple concurrent tasks:
//! - Scheduler: Priority queue with rate limit management
//! - Lock manager: Resource locking with deadlock prevention
//! - Event bus: Inter-task communication
//! - Knowledge store: Learning extraction and injection

pub mod events;
pub mod knowledge;
pub mod locks;
pub mod scheduler;

pub use events::{Event, EventBus, EventKind};
pub use knowledge::{Knowledge, KnowledgeKind, KnowledgeStore};
pub use locks::{Lock, LockManager, LockResult, ResourceId};
pub use scheduler::{ScheduleResult, Scheduler, SchedulerConfig};
