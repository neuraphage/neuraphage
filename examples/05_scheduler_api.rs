//! Example 05: Scheduler API
//!
//! Demonstrates using the Scheduler for concurrent task execution control.
//!
//! Run with: cargo run --example 05_scheduler_api

use std::time::Duration;

use neuraphage::Task;
use neuraphage::coordination::{ScheduleResult, Scheduler, SchedulerConfig};

#[tokio::main]
async fn main() -> neuraphage::Result<()> {
    println!("=== Scheduler API Example ===\n");

    // Create a scheduler with custom config
    let config = SchedulerConfig {
        max_concurrent: 2,           // Max 2 tasks running at once
        max_requests_per_minute: 60, // Max 60 API calls per minute
        rate_window: Duration::from_secs(60),
    };
    let scheduler = Scheduler::new(config);

    // Create tasks with different priorities
    let tasks = vec![
        Task::new("Critical security fix", 0), // Priority 0 (critical)
        Task::new("Sprint feature", 1),        // Priority 1 (high)
        Task::new("Code refactor", 2),         // Priority 2 (medium)
        Task::new("Update docs", 3),           // Priority 3 (low)
    ];

    // Schedule tasks
    println!("Scheduling tasks (max 2 concurrent)...\n");
    for task in &tasks {
        let result = scheduler.schedule(task).await?;
        match result {
            ScheduleResult::Ready => {
                println!("  {} -> READY (can start)", task.description);
                scheduler.start_task(&task.id).await?;
            }
            ScheduleResult::Queued { position } => {
                println!("  {} -> QUEUED (position {})", task.description, position);
            }
            ScheduleResult::RateLimited { retry_after } => {
                println!("  {} -> RATE LIMITED (retry in {:?})", task.description, retry_after);
            }
            ScheduleResult::Rejected { reason } => {
                println!("  {} -> REJECTED: {}", task.description, reason);
            }
        }
    }

    // Simulate task completion
    println!("\nCompleting 'Critical security fix'...");
    let next = scheduler.finish_task(&tasks[0].id).await?;
    if let Some(next_id) = next {
        println!("  Next task to run: {}", next_id.0);
    }

    // Complete another
    println!("\nCompleting 'Sprint feature'...");
    let next = scheduler.finish_task(&tasks[1].id).await?;
    if let Some(next_id) = next {
        println!("  Next task to run: {}", next_id.0);
    }

    println!("\nDone!");
    Ok(())
}
