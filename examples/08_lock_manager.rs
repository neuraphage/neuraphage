//! Example 08: Lock Manager
//!
//! Demonstrates resource locking to prevent conflicting operations.
//!
//! Run with: cargo run --example 08_lock_manager

use neuraphage::TaskId;
use neuraphage::coordination::{LockManager, LockResult, ResourceId};

#[tokio::main]
async fn main() -> neuraphage::Result<()> {
    println!("=== Lock Manager Example ===\n");

    // Create lock manager
    let manager = LockManager::new();

    let task1 = TaskId::new();
    let task2 = TaskId::new();
    let resource = ResourceId::file("database.db");

    // Task 1 acquires exclusive lock
    println!("Task 1 ({}) acquiring exclusive lock on database.db...", &task1.0[..12]);
    let result = manager.acquire_exclusive(resource.clone(), &task1).await?;
    println!("  Result: {:?}\n", result);

    // Task 2 tries to acquire same lock
    println!("Task 2 ({}) trying to acquire same lock...", &task2.0[..12]);
    let result = manager.acquire_exclusive(resource.clone(), &task2).await?;
    match &result {
        LockResult::Blocked { holder } => {
            println!("  Blocked by: {}\n", &holder.0[..12]);
        }
        _ => println!("  Result: {:?}\n", result),
    }

    // Check who holds the lock
    println!("Checking lock holder...");
    if let Some(holder) = manager.get_holder(&resource).await {
        println!("  Lock held by: {}\n", &holder.0[..12]);
    }

    // Release lock
    println!("Task 1 releasing lock...");
    manager.release(&resource, &task1).await?;
    println!("  Released\n");

    // Now Task 2 can acquire
    println!("Task 2 trying again...");
    let result = manager.acquire_exclusive(resource.clone(), &task2).await?;
    println!("  Result: {:?}\n", result);

    // Demonstrate shared locks
    println!("--- Shared Lock Demo ---\n");
    let file = ResourceId::file("config.yaml");

    println!("Task 1 acquiring shared lock on config.yaml...");
    let r1 = manager.acquire_shared(file.clone(), &task1).await?;
    println!("  Result: {:?}", r1);

    println!("Task 2 acquiring shared lock on config.yaml...");
    let r2 = manager.acquire_shared(file.clone(), &task2).await?;
    println!("  Result: {:?} (both can read)", r2);

    println!("\nTask 1 trying to upgrade to exclusive...");
    let r3 = manager.acquire_exclusive(file.clone(), &task1).await?;
    println!("  Result: {:?} (blocked because Task 2 has shared)", r3);

    println!("\nDone!");
    Ok(())
}
