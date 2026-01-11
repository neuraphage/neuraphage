//! Knowledge store for learning extraction and injection.
//!
//! Stores extracted learnings and decisions that can be shared across tasks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::Result;
use crate::task::TaskId;

/// Types of knowledge that can be stored.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnowledgeKind {
    /// A learned pattern or technique.
    Learning,
    /// A recorded decision with rationale.
    Decision,
    /// A discovered fact about the codebase.
    Fact,
    /// A preference or convention.
    Preference,
    /// An error and its resolution.
    ErrorResolution,
}

/// A piece of knowledge extracted from task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Knowledge {
    /// Unique identifier.
    pub id: String,
    /// Kind of knowledge.
    pub kind: KnowledgeKind,
    /// Title/summary.
    pub title: String,
    /// Full content/details.
    pub content: String,
    /// Source task that extracted this knowledge.
    pub source_task: Option<TaskId>,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// When the knowledge was created.
    pub created_at: DateTime<Utc>,
    /// Relevance score (0.0-1.0).
    pub relevance: f32,
    /// Number of times this knowledge has been used.
    pub use_count: u32,
}

impl Knowledge {
    /// Create new knowledge.
    pub fn new(kind: KnowledgeKind, title: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            kind,
            title: title.into(),
            content: content.into(),
            source_task: None,
            tags: Vec::new(),
            created_at: Utc::now(),
            relevance: 1.0,
            use_count: 0,
        }
    }

    /// Set the source task.
    pub fn from_task(mut self, task_id: TaskId) -> Self {
        self.source_task = Some(task_id);
        self
    }

    /// Add tags.
    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(|t| t.into()).collect();
        self
    }

    /// Set relevance score.
    pub fn with_relevance(mut self, relevance: f32) -> Self {
        self.relevance = relevance.clamp(0.0, 1.0);
        self
    }
}

/// In-memory knowledge store.
/// For persistence, this would integrate with engram or a database.
pub struct KnowledgeStore {
    /// All stored knowledge.
    items: Arc<Mutex<HashMap<String, Knowledge>>>,
    /// Index by tags.
    tag_index: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Index by kind.
    kind_index: Arc<Mutex<HashMap<KnowledgeKind, Vec<String>>>>,
    /// Storage path for persistence.
    #[allow(dead_code)]
    storage_path: Option<PathBuf>,
}

impl KnowledgeStore {
    /// Create a new in-memory knowledge store.
    pub fn new() -> Self {
        Self {
            items: Arc::new(Mutex::new(HashMap::new())),
            tag_index: Arc::new(Mutex::new(HashMap::new())),
            kind_index: Arc::new(Mutex::new(HashMap::new())),
            storage_path: None,
        }
    }

    /// Create a knowledge store with a storage path.
    pub fn with_storage(path: impl AsRef<Path>) -> Self {
        Self {
            storage_path: Some(path.as_ref().to_path_buf()),
            ..Self::new()
        }
    }

    /// Store a piece of knowledge.
    pub async fn store(&self, knowledge: Knowledge) -> Result<String> {
        let id = knowledge.id.clone();

        // Update indexes
        {
            let mut tag_index = self.tag_index.lock().await;
            for tag in &knowledge.tags {
                tag_index.entry(tag.clone()).or_default().push(id.clone());
            }
        }

        {
            let mut kind_index = self.kind_index.lock().await;
            kind_index.entry(knowledge.kind.clone()).or_default().push(id.clone());
        }

        // Store the knowledge
        {
            let mut items = self.items.lock().await;
            items.insert(id.clone(), knowledge);
        }

        Ok(id)
    }

    /// Get knowledge by ID.
    pub async fn get(&self, id: &str) -> Option<Knowledge> {
        let items = self.items.lock().await;
        items.get(id).cloned()
    }

    /// Get all knowledge of a specific kind.
    pub async fn by_kind(&self, kind: KnowledgeKind) -> Vec<Knowledge> {
        let kind_index = self.kind_index.lock().await;
        let items = self.items.lock().await;

        kind_index
            .get(&kind)
            .map(|ids| ids.iter().filter_map(|id| items.get(id).cloned()).collect())
            .unwrap_or_default()
    }

    /// Get all knowledge with a specific tag.
    pub async fn by_tag(&self, tag: &str) -> Vec<Knowledge> {
        let tag_index = self.tag_index.lock().await;
        let items = self.items.lock().await;

        tag_index
            .get(tag)
            .map(|ids| ids.iter().filter_map(|id| items.get(id).cloned()).collect())
            .unwrap_or_default()
    }

    /// Search knowledge by text content.
    pub async fn search(&self, query: &str) -> Vec<Knowledge> {
        let query_lower = query.to_lowercase();
        let items = self.items.lock().await;

        let mut results: Vec<_> = items
            .values()
            .filter(|k| {
                k.title.to_lowercase().contains(&query_lower)
                    || k.content.to_lowercase().contains(&query_lower)
                    || k.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .cloned()
            .collect();

        // Sort by relevance
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap());
        results
    }

    /// Get relevant knowledge for a task context.
    pub async fn query_relevant(&self, tags: &[String], kinds: &[KnowledgeKind], limit: usize) -> Vec<Knowledge> {
        let items = self.items.lock().await;

        let mut results: Vec<_> = items
            .values()
            .filter(|k| {
                let kind_match = kinds.is_empty() || kinds.contains(&k.kind);
                let tag_match = tags.is_empty() || k.tags.iter().any(|t| tags.contains(t));
                kind_match && tag_match
            })
            .cloned()
            .collect();

        // Sort by relevance * log(use_count + 1) to favor both relevant and useful
        results.sort_by(|a, b| {
            let score_a = a.relevance * (a.use_count as f32 + 1.0).ln();
            let score_b = b.relevance * (b.use_count as f32 + 1.0).ln();
            score_b.partial_cmp(&score_a).unwrap()
        });

        results.truncate(limit);
        results
    }

    /// Record that knowledge was used (for ranking).
    pub async fn record_use(&self, id: &str) {
        let mut items = self.items.lock().await;
        if let Some(knowledge) = items.get_mut(id) {
            knowledge.use_count += 1;
        }
    }

    /// Get all knowledge items.
    pub async fn all(&self) -> Vec<Knowledge> {
        let items = self.items.lock().await;
        items.values().cloned().collect()
    }

    /// Get total count of knowledge items.
    pub async fn count(&self) -> usize {
        self.items.lock().await.len()
    }

    /// Delete knowledge by ID.
    pub async fn delete(&self, id: &str) -> Option<Knowledge> {
        let mut items = self.items.lock().await;
        let removed = items.remove(id);

        if let Some(ref knowledge) = removed {
            // Clean up indexes
            let mut tag_index = self.tag_index.lock().await;
            for tag in &knowledge.tags {
                if let Some(ids) = tag_index.get_mut(tag) {
                    ids.retain(|i| i != id);
                }
            }

            let mut kind_index = self.kind_index.lock().await;
            if let Some(ids) = kind_index.get_mut(&knowledge.kind) {
                ids.retain(|i| i != id);
            }
        }

        removed
    }

    /// Clear all knowledge.
    pub async fn clear(&self) {
        self.items.lock().await.clear();
        self.tag_index.lock().await.clear();
        self.kind_index.lock().await.clear();
    }

    /// Get statistics about stored knowledge.
    pub async fn stats(&self) -> KnowledgeStats {
        let items = self.items.lock().await;
        let kind_index = self.kind_index.lock().await;
        let tag_index = self.tag_index.lock().await;

        let mut by_kind = HashMap::new();
        for (kind, ids) in kind_index.iter() {
            by_kind.insert(kind.clone(), ids.len());
        }

        KnowledgeStats {
            total_items: items.len(),
            by_kind,
            unique_tags: tag_index.len(),
            total_uses: items.values().map(|k| k.use_count as usize).sum(),
        }
    }
}

impl Default for KnowledgeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about the knowledge store.
#[derive(Debug, Clone, Default)]
pub struct KnowledgeStats {
    /// Total number of knowledge items.
    pub total_items: usize,
    /// Items by kind.
    pub by_kind: HashMap<KnowledgeKind, usize>,
    /// Number of unique tags.
    pub unique_tags: usize,
    /// Total number of times knowledge has been used.
    pub total_uses: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id(s: &str) -> TaskId {
        TaskId(s.to_string())
    }

    #[tokio::test]
    async fn test_store_and_get() {
        let store = KnowledgeStore::new();

        let knowledge = Knowledge::new(
            KnowledgeKind::Learning,
            "Test Pattern",
            "This is a test pattern description",
        );
        let id = store.store(knowledge).await.unwrap();

        let retrieved = store.get(&id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().title, "Test Pattern");
    }

    #[tokio::test]
    async fn test_by_kind() {
        let store = KnowledgeStore::new();

        store
            .store(Knowledge::new(KnowledgeKind::Learning, "Learning 1", "Content"))
            .await
            .unwrap();
        store
            .store(Knowledge::new(KnowledgeKind::Decision, "Decision 1", "Content"))
            .await
            .unwrap();
        store
            .store(Knowledge::new(KnowledgeKind::Learning, "Learning 2", "Content"))
            .await
            .unwrap();

        let learnings = store.by_kind(KnowledgeKind::Learning).await;
        assert_eq!(learnings.len(), 2);

        let decisions = store.by_kind(KnowledgeKind::Decision).await;
        assert_eq!(decisions.len(), 1);
    }

    #[tokio::test]
    async fn test_by_tag() {
        let store = KnowledgeStore::new();

        store
            .store(
                Knowledge::new(KnowledgeKind::Learning, "Rust Pattern", "Content").with_tags(vec!["rust", "patterns"]),
            )
            .await
            .unwrap();
        store
            .store(
                Knowledge::new(KnowledgeKind::Learning, "Python Pattern", "Content")
                    .with_tags(vec!["python", "patterns"]),
            )
            .await
            .unwrap();

        let rust = store.by_tag("rust").await;
        assert_eq!(rust.len(), 1);

        let patterns = store.by_tag("patterns").await;
        assert_eq!(patterns.len(), 2);
    }

    #[tokio::test]
    async fn test_search() {
        let store = KnowledgeStore::new();

        store
            .store(Knowledge::new(
                KnowledgeKind::Learning,
                "Error Handling Pattern",
                "Use Result and proper error propagation",
            ))
            .await
            .unwrap();
        store
            .store(Knowledge::new(
                KnowledgeKind::Decision,
                "Use async-trait",
                "Decided to use async-trait crate for async traits",
            ))
            .await
            .unwrap();

        let results = store.search("error").await;
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("Error"));

        let results = store.search("async").await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_query_relevant() {
        let store = KnowledgeStore::new();

        store
            .store(
                Knowledge::new(KnowledgeKind::Learning, "High Relevance", "Content")
                    .with_tags(vec!["test"])
                    .with_relevance(0.9),
            )
            .await
            .unwrap();
        store
            .store(
                Knowledge::new(KnowledgeKind::Learning, "Low Relevance", "Content")
                    .with_tags(vec!["test"])
                    .with_relevance(0.1),
            )
            .await
            .unwrap();

        let results = store
            .query_relevant(&["test".to_string()], &[KnowledgeKind::Learning], 10)
            .await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "High Relevance");
    }

    #[tokio::test]
    async fn test_record_use() {
        let store = KnowledgeStore::new();

        let id = store
            .store(Knowledge::new(KnowledgeKind::Learning, "Test", "Content"))
            .await
            .unwrap();

        assert_eq!(store.get(&id).await.unwrap().use_count, 0);

        store.record_use(&id).await;
        store.record_use(&id).await;

        assert_eq!(store.get(&id).await.unwrap().use_count, 2);
    }

    #[tokio::test]
    async fn test_delete() {
        let store = KnowledgeStore::new();

        let id = store
            .store(Knowledge::new(KnowledgeKind::Learning, "To Delete", "Content").with_tags(vec!["test"]))
            .await
            .unwrap();

        assert!(store.get(&id).await.is_some());
        assert_eq!(store.by_tag("test").await.len(), 1);

        let deleted = store.delete(&id).await;
        assert!(deleted.is_some());
        assert!(store.get(&id).await.is_none());
        assert_eq!(store.by_tag("test").await.len(), 0);
    }

    #[tokio::test]
    async fn test_stats() {
        let store = KnowledgeStore::new();

        store
            .store(Knowledge::new(KnowledgeKind::Learning, "L1", "Content").with_tags(vec!["tag1", "tag2"]))
            .await
            .unwrap();
        store
            .store(Knowledge::new(KnowledgeKind::Decision, "D1", "Content").with_tags(vec!["tag1"]))
            .await
            .unwrap();

        let stats = store.stats().await;
        assert_eq!(stats.total_items, 2);
        assert_eq!(stats.by_kind.get(&KnowledgeKind::Learning), Some(&1));
        assert_eq!(stats.by_kind.get(&KnowledgeKind::Decision), Some(&1));
        assert_eq!(stats.unique_tags, 2);
    }

    #[test]
    fn test_knowledge_builder() {
        let knowledge = Knowledge::new(KnowledgeKind::ErrorResolution, "Fix Bug", "Solution details")
            .from_task(task_id("task-1"))
            .with_tags(vec!["bugs", "rust"])
            .with_relevance(0.8);

        assert_eq!(knowledge.source_task, Some(task_id("task-1")));
        assert_eq!(knowledge.tags, vec!["bugs", "rust"]);
        assert_eq!(knowledge.relevance, 0.8);
    }

    #[test]
    fn test_relevance_clamping() {
        let k1 = Knowledge::new(KnowledgeKind::Learning, "Test", "Content").with_relevance(1.5);
        assert_eq!(k1.relevance, 1.0);

        let k2 = Knowledge::new(KnowledgeKind::Learning, "Test", "Content").with_relevance(-0.5);
        assert_eq!(k2.relevance, 0.0);
    }
}
