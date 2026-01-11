//! Example 07: Knowledge Store
//!
//! Demonstrates storing and retrieving learned patterns and solutions.
//!
//! Run with: cargo run --example 07_knowledge_store

use neuraphage::coordination::{Knowledge, KnowledgeKind, KnowledgeStore};

#[tokio::main]
async fn main() -> neuraphage::Result<()> {
    println!("=== Knowledge Store Example ===\n");

    // Create knowledge store
    let store = KnowledgeStore::new();

    // Add some knowledge items
    println!("Adding knowledge items...\n");

    store
        .store(
            Knowledge::new(
                KnowledgeKind::Learning,
                "Error Handling Pattern",
                "Use Result<T, E> with ? operator for propagation. \
             Add context with .context() from eyre.",
            )
            .with_tags(["rust", "patterns"]),
        )
        .await?;

    store
        .store(
            Knowledge::new(
                KnowledgeKind::ErrorResolution,
                "Async Deadlock Fix",
                "Avoid holding locks across await points. \
             Use tokio::sync::Mutex instead of std::sync::Mutex.",
            )
            .with_tags(["rust", "async", "deadlock"]),
        )
        .await?;

    store
        .store(
            Knowledge::new(
                KnowledgeKind::Fact,
                "API Rate Limit",
                "Maximum 60 requests per minute. \
             Use exponential backoff on 429 responses.",
            )
            .with_tags(["api", "limits"]),
        )
        .await?;

    store
        .store(
            Knowledge::new(
                KnowledgeKind::Preference,
                "Code Style",
                "Use cargo fmt for formatting. Run clippy before commits.",
            )
            .with_tags(["rust", "style"]),
        )
        .await?;

    // Search knowledge
    println!("Searching for 'deadlock' related knowledge...\n");
    let results = store.search("deadlock").await;

    for (i, k) in results.iter().enumerate() {
        println!(
            "  {}. [{:?}] {} (relevance: {:.2})",
            i + 1,
            k.kind,
            k.title,
            k.relevance
        );
        println!("     {}\n", k.content);
    }

    // Get by kind
    println!("All Learning entries:");
    for k in store.by_kind(KnowledgeKind::Learning).await {
        println!("  - {}", k.title);
    }

    // Get by tag
    println!("\nEntries tagged 'rust':");
    for k in store.by_tag("rust").await {
        println!("  - {}", k.title);
    }

    // Show statistics
    let stats = store.stats().await;
    println!("\nKnowledge store statistics:");
    println!("  Total items: {}", stats.total_items);
    println!("  Unique tags: {}", stats.unique_tags);
    for (kind, count) in &stats.by_kind {
        println!("  {:?}: {}", kind, count);
    }

    println!("\nDone!");
    Ok(())
}
