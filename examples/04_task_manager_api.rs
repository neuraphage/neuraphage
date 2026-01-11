//! Example 04: TaskManager API
//!
//! Demonstrates using the TaskManager directly for programmatic task management.
//!
//! Run with: cargo run --example 04_task_manager_api

use neuraphage::{TaskManager, TaskStatus};
use tempfile::tempdir;

fn main() -> neuraphage::Result<()> {
    // Create a temporary directory for the task store
    let temp = tempdir().expect("Failed to create temp dir");
    let store_path = temp.path().to_path_buf();

    // Initialize TaskManager (creates new store)
    let mut manager = TaskManager::init(&store_path)?;

    println!("=== TaskManager API Example ===\n");

    // Create tasks using create_task(description, priority, tags, context)
    println!("Creating tasks...");

    let task1 = manager.create_task("Implement feature X", 1, &["feature"], None)?;
    println!("  Created: {} (priority 1)", task1.id);

    let task2 = manager.create_task("Write tests for feature X", 2, &["testing"], None)?;
    println!("  Created: {} (priority 2)", task2.id);

    let task3 = manager.create_task("Update documentation", 3, &["docs"], None)?;
    println!("  Created: {} (priority 3)", task3.id);

    // Add dependency: tests blocked by feature
    manager.add_dependency(&task2.id, &task1.id)?;
    println!("\n  Added dependency: {} blocked by {}", task2.id, task1.id);

    // Query ready tasks
    println!("\nReady tasks:");
    for task in manager.ready_tasks()? {
        println!("  - {} (priority {})", task.description, task.priority);
    }

    // Query blocked tasks
    println!("\nBlocked tasks:");
    for task in manager.blocked_tasks()? {
        println!("  - {}", task.description);
    }

    // Update task status
    println!("\nStarting feature task...");
    manager.set_status(&task1.id, TaskStatus::Running)?;

    // Get task counts
    let counts = manager.task_counts()?;
    println!("\nTask counts:");
    println!("  Running: {}", counts.running);
    println!("  Queued: {}", counts.queued);
    println!("  Blocked: {}", counts.blocked);

    // Complete the feature task
    println!("\nCompleting feature task...");
    manager.close_task(&task1.id, TaskStatus::Completed, Some("Feature implemented"))?;

    // Now tests should be unblocked
    println!("\nReady tasks after completing feature:");
    for task in manager.ready_tasks()? {
        println!("  - {}", task.description);
    }

    println!("\nDone!");
    Ok(())
}
