//! Example 06: Event Bus
//!
//! Demonstrates the event system for inter-component communication.
//!
//! Run with: cargo run --example 06_event_bus

use std::time::Duration;

use neuraphage::TaskId;
use neuraphage::coordination::{Event, EventBus, EventKind};

#[tokio::main]
async fn main() -> neuraphage::Result<()> {
    println!("=== Event Bus Example ===\n");

    // Create event bus
    let bus = EventBus::new();

    // Subscribe to all events
    let mut subscription = bus.subscribe();

    // Spawn a task to receive events
    let handle = tokio::spawn(async move {
        let mut count = 0;
        // Use try_recv in a loop with small sleep to poll for events
        loop {
            if let Some(event) = subscription.try_recv() {
                count += 1;
                println!(
                    "  Received: {:?} from {:?}",
                    event.kind,
                    event.source_task.as_ref().map(|t| &t.0[..12])
                );
                if count >= 4 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    // Publish some events
    let task_id = TaskId::new();
    println!("Publishing events for task {}...\n", &task_id.0[..20]);

    tokio::time::sleep(Duration::from_millis(10)).await;
    bus.publish(Event::new(EventKind::TaskStarted).from_task(task_id.clone()))
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    bus.publish(
        Event::new(EventKind::Custom("progress".to_string()))
            .from_task(task_id.clone())
            .with_payload(serde_json::json!({"iteration": 1, "message": "Processing..."})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    bus.publish(Event::new(EventKind::LearningExtracted).from_task(task_id.clone()))
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    bus.publish(Event::new(EventKind::TaskCompleted).from_task(task_id))
        .await;

    // Wait for receiver to finish
    handle.await.expect("Receiver task failed");

    println!("\nDone!");
    Ok(())
}
