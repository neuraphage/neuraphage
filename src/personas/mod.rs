//! Persona system for Neuraphage.
//!
//! Provides:
//! - Persona definitions with system prompts and model tiers
//! - PersonaStore for loading and managing personas
//! - Watcher for stuck task detection
//! - Syncer for cross-task learning relay

pub mod persona;
pub mod syncer;
pub mod watcher;

pub use persona::{ModelTier, Persona, PersonaStore};
pub use syncer::{SyncMessage, SyncResult, Syncer, SyncerConfig};
pub use watcher::{TaskSnapshot, WatchResult, Watcher, WatcherConfig, WatcherRecommendation};
