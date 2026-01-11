//! Example 10: Watcher
//!
//! Demonstrates monitoring tasks for stuck/unhealthy conditions.
//!
//! Run with: cargo run --example 10_watcher

use std::time::Duration;

use neuraphage::TaskId;
use neuraphage::personas::{TaskSnapshot, Watcher, WatcherConfig, WatcherRecommendation};

fn main() -> neuraphage::Result<()> {
    println!("=== Watcher Example ===\n");

    // Create watcher with default config
    let config = WatcherConfig::default();
    let watcher = Watcher::new(config);

    println!("Watcher config:");
    println!("  Check interval: {:?}", watcher.interval());
    println!(
        "  Max iterations without progress: {}",
        watcher.config().max_iterations_without_progress
    );
    println!(
        "  Max tokens without changes: {}",
        watcher.config().max_tokens_without_changes
    );
    println!();

    // Create a healthy task snapshot
    let healthy_task = TaskId::new();
    let healthy_snapshot = TaskSnapshot {
        task_id: healthy_task.clone(),
        iteration: 3,
        tokens_used: 5000,
        recent_tool_calls: vec![],
        time_since_progress: Duration::from_secs(10),
        recent_conversation: "Implementing feature X...".to_string(),
        has_file_changes: true,
    };

    println!("Evaluating healthy task...");
    let result = watcher.evaluate(&healthy_snapshot);
    println!("  Status: {:?}", result.status);
    println!("  Diagnosis: {}", result.diagnosis);
    println!();

    // Create a stuck task snapshot
    let stuck_task = TaskId::new();
    let stuck_snapshot = TaskSnapshot {
        task_id: stuck_task.clone(),
        iteration: 15,      // Over threshold
        tokens_used: 60000, // Over tokens threshold
        recent_tool_calls: vec![],
        time_since_progress: Duration::from_secs(600), // 10 minutes
        recent_conversation: "Still thinking...".to_string(),
        has_file_changes: false,
    };

    println!("Evaluating stuck task...");
    let result = watcher.evaluate(&stuck_snapshot);
    println!("  Status: {:?}", result.status);
    println!("  Diagnosis: {}", result.diagnosis);
    println!("  Recommendation: {:?}", result.recommendation);
    println!("  Nudge: {:?}", result.nudge_message);
    println!("  Confidence: {:.2}", result.confidence);

    // Demonstrate different recommendations
    println!("\n--- Recommendation Types ---\n");
    let recommendations = [
        WatcherRecommendation::Continue,
        WatcherRecommendation::Nudge,
        WatcherRecommendation::Escalate,
        WatcherRecommendation::Pause,
        WatcherRecommendation::Abort,
    ];
    for rec in &recommendations {
        println!("  {:?}", rec);
    }

    println!("\nDone!");
    Ok(())
}
