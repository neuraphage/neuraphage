//! Example 11: Syncer
//!
//! Demonstrates cross-task learning relay for knowledge sharing.
//!
//! Run with: cargo run --example 11_syncer

use neuraphage::TaskId;
use neuraphage::coordination::{Knowledge, KnowledgeKind};
use neuraphage::personas::{
    SyncMessage, SyncRelevance, SyncUrgency, Syncer, SyncerConfig, TaskRelationship, TaskSummary,
};

fn main() -> neuraphage::Result<()> {
    println!("=== Syncer Example ===\n");

    // Create syncer with default config
    let config = SyncerConfig::default();
    let mut syncer = Syncer::new(config);

    println!("Syncer config:");
    println!("  Interval: {:?}", syncer.interval());
    println!("  Enabled: {}", syncer.is_enabled());
    println!();

    // Create task summaries
    let source_task = TaskId::new();
    let source = TaskSummary {
        task_id: source_task.clone(),
        description: "Implement authentication module".to_string(),
        tags: vec!["auth".to_string(), "security".to_string()],
        current_focus: "Working on JWT validation".to_string(),
        recent_learnings: vec![],
        working_directory: Some("/project/src/auth".to_string()),
        parent_task: None,
    };

    let related_task = TaskId::new();
    let related = TaskSummary {
        task_id: related_task.clone(),
        description: "Add API authentication middleware".to_string(),
        tags: vec!["api".to_string(), "auth".to_string()], // Shares 'auth' tag
        current_focus: "Setting up middleware chain".to_string(),
        recent_learnings: vec![],
        working_directory: Some("/project/src/api".to_string()),
        parent_task: None,
    };

    let unrelated_task = TaskId::new();
    let unrelated = TaskSummary {
        task_id: unrelated_task.clone(),
        description: "Update database schema".to_string(),
        tags: vec!["database".to_string()],
        current_focus: "Adding migration".to_string(),
        recent_learnings: vec![],
        working_directory: Some("/project/migrations".to_string()),
        parent_task: None,
    };

    // Create a Knowledge object
    let learning = Knowledge::new(
        KnowledgeKind::Learning,
        "JWT role claim",
        "JWT tokens should include the user's role claim for authorization checks",
    )
    .from_task(source_task.clone())
    .with_tags(["auth", "jwt", "security"]);

    let targets = vec![source.clone(), related.clone(), unrelated.clone()];

    println!("Evaluating learning from source task:");
    println!("  Learning: \"{}\"", learning.content);
    println!("  Targets: {} tasks\n", targets.len());

    let result = syncer.evaluate_learning(&source, &learning, &targets);

    if !result.recipients.is_empty() {
        println!("Learning will be relayed to {} recipients:", result.recipients.len());
        for msg in &result.recipients {
            println!("  - Task: {}", &msg.target_task.0[..12]);
            println!("    Relevance: {:?}", msg.relevance);
            println!("    Urgency: {:?}", msg.urgency);
            println!();
        }
    } else {
        println!("Learning not relayed: {:?}", result.skip_reason);
    }

    // Create a direct sync message
    println!("--- Creating Direct Sync Message ---\n");
    let message = SyncMessage::new(
        source_task.clone(),
        related_task.clone(),
        "Important pattern discovered",
        "Use bcrypt for password hashing with cost factor 12",
    )
    .with_relevance(SyncRelevance::High)
    .with_urgency(SyncUrgency::Helpful);

    println!("Sync message:");
    println!("  From: {}", &message.source_task.0[..12]);
    println!("  To: {}", &message.target_task.0[..12]);
    println!("  Summary: {}", message.learning_summary);
    println!("  Message: {}", message.message);
    println!("  Formatted for injection:");
    println!("  {}", message.format_for_injection());

    // Demonstrate task relationships
    println!("\n--- Task Relationships ---\n");
    let relationships = [
        TaskRelationship::SameDirectory,
        TaskRelationship::SameParent,
        TaskRelationship::SharedTags,
        TaskRelationship::ExplicitGroup,
        TaskRelationship::None,
    ];
    for rel in &relationships {
        println!("  {:?}", rel);
    }

    println!("\nDone!");
    Ok(())
}
