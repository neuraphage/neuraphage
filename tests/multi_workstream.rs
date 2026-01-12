//! Multi-workstream integration tests.
//!
//! These tests exercise concurrent task execution to discover
//! what breaks, what's awkward, and what observability we need.

use std::sync::Arc;
use std::time::Duration;

use neuraphage::coordination::{Event, EventBus, EventKind};
use neuraphage::task::TaskId;
use tokio::sync::Mutex;
use tokio::time::sleep;

/// Scenario 1: Event bus under concurrent load
///
/// Question: Does EventBus handle concurrent publishers?
#[tokio::test]
async fn test_concurrent_event_publishing() {
    let event_bus = Arc::new(EventBus::new());
    let received = Arc::new(Mutex::new(Vec::new()));

    // Subscribe before publishing
    let received_clone = received.clone();
    let mut subscription = event_bus.subscribe();
    let _collector = tokio::spawn(async move {
        while let Some(event) = subscription.recv().await {
            let mut r = received_clone.lock().await;
            r.push(event.kind);
            if r.len() >= 50 {
                break;
            }
        }
    });

    // Give subscriber time to start
    sleep(Duration::from_millis(10)).await;

    // Publish 50 events from 5 concurrent tasks
    let handles: Vec<_> = (0..5)
        .map(|task_num| {
            let bus = event_bus.clone();
            let task_id = TaskId(format!("task-{}", task_num));
            tokio::spawn(async move {
                for _ in 0..10 {
                    bus.task_started(task_id.clone()).await;
                }
            })
        })
        .collect();

    futures::future::join_all(handles).await;

    // Wait for collector
    sleep(Duration::from_millis(100)).await;

    let events = received.lock().await;
    println!("Received {} events from concurrent publishers", events.len());
    assert!(events.len() >= 45, "Should receive most events (got {})", events.len());
}

/// Scenario 2: Task lifecycle events
///
/// Question: Can we observe task lifecycle events?
#[tokio::test]
async fn test_task_lifecycle_events() {
    let event_bus = Arc::new(EventBus::new());
    let received = Arc::new(Mutex::new(Vec::new()));

    // Subscribe
    let received_clone = received.clone();
    let mut subscription = event_bus.subscribe();
    let _collector = tokio::spawn(async move {
        while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), subscription.recv()).await {
            let mut r = received_clone.lock().await;
            r.push((event.kind.clone(), event.source_task.clone()));
        }
    });

    sleep(Duration::from_millis(10)).await;

    // Simulate lifecycle
    let task_id = TaskId("test-task-001".to_string());
    event_bus.task_started(task_id.clone()).await;
    event_bus.file_modified(task_id.clone(), "/test/file.rs").await;
    event_bus.task_completed(task_id.clone(), "Done").await;

    sleep(Duration::from_millis(300)).await;

    let events = received.lock().await;
    println!("Lifecycle events captured:");
    for (kind, source) in events.iter() {
        println!("  {:?} from {:?}", kind, source);
    }

    assert!(events.iter().any(|(k, _)| matches!(k, EventKind::TaskStarted)));
    assert!(events.iter().any(|(k, _)| matches!(k, EventKind::FileModified)));
    assert!(events.iter().any(|(k, _)| matches!(k, EventKind::TaskCompleted)));
}

/// Scenario 3: Event history and querying
///
/// Question: Can we query past events effectively?
#[tokio::test]
async fn test_event_history_query() {
    let event_bus = EventBus::with_history_size(100);

    // Publish events from multiple tasks
    for i in 0..3 {
        let task_id = TaskId(format!("task-{}", i));
        event_bus.task_started(task_id.clone()).await;
        event_bus
            .file_modified(task_id.clone(), &format!("/file_{}.rs", i))
            .await;
        event_bus.task_completed(task_id.clone(), "Done").await;
    }

    // Query all recent events
    let recent = event_bus.recent_events(100).await;
    assert_eq!(recent.len(), 9); // 3 events per task * 3 tasks

    // Query events for specific task
    let task_1_events = event_bus
        .query_events(None, Some(&TaskId("task-1".to_string())), None, 100)
        .await;
    assert_eq!(task_1_events.len(), 3);

    // Query specific event type
    let completed_events = event_bus
        .query_events(Some(&[EventKind::TaskCompleted]), None, None, 100)
        .await;
    assert_eq!(completed_events.len(), 3);

    println!("Event history querying works:");
    println!("  - Total events: {}", recent.len());
    println!("  - Task-1 events: {}", task_1_events.len());
    println!("  - Completed events: {}", completed_events.len());
}

/// Scenario 4: What happens with rapid concurrent events?
///
/// Question: Is there event loss under high concurrency?
#[tokio::test]
async fn test_high_concurrency_event_stress() {
    let event_bus = Arc::new(EventBus::with_history_size(1000));
    let events_to_publish = 100;
    let concurrent_tasks = 10;

    // Publish many events concurrently
    let handles: Vec<_> = (0..concurrent_tasks)
        .map(|task_num| {
            let bus = event_bus.clone();
            let task_id = TaskId(format!("stress-task-{}", task_num));
            tokio::spawn(async move {
                for i in 0..events_to_publish {
                    bus.publish(Event::new(EventKind::Custom(format!("event-{}", i))).from_task(task_id.clone()))
                        .await;
                }
            })
        })
        .collect();

    futures::future::join_all(handles).await;

    // Check history
    let counts = event_bus.event_counts().await;
    let total_custom: usize = counts
        .iter()
        .filter(|(k, _)| matches!(k, EventKind::Custom(_)))
        .map(|(_, count)| count)
        .sum();

    let expected = events_to_publish * concurrent_tasks;
    println!(
        "Stress test: {} events published, {} recorded in counts",
        expected, total_custom
    );

    // History is bounded, but counts should be accurate
    let history = event_bus.recent_events(1000).await;
    println!("History contains {} events (max 1000)", history.len());

    assert_eq!(total_custom, expected, "Event counts should be accurate");
}

/// Scenario 5: Coordination events (rebase, sync)
///
/// Question: Do coordination events flow correctly?
#[tokio::test]
async fn test_coordination_events() {
    let event_bus = Arc::new(EventBus::new());
    let received = Arc::new(Mutex::new(Vec::new()));

    let received_clone = received.clone();
    let mut subscription = event_bus.subscribe_to(vec![
        EventKind::MainUpdated,
        EventKind::RebaseRequired,
        EventKind::RebaseCompleted,
        EventKind::SyncRelayed,
    ]);

    let _collector = tokio::spawn(async move {
        while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(200), subscription.recv()).await {
            let mut r = received_clone.lock().await;
            r.push(event.kind.clone());
        }
    });

    sleep(Duration::from_millis(10)).await;

    // Simulate coordination sequence
    event_bus
        .publish(Event::new(EventKind::MainUpdated).with_payload(serde_json::json!({"commit": "abc123"})))
        .await;

    let task_id = TaskId("task-needs-rebase".to_string());
    event_bus
        .publish(
            Event::new(EventKind::RebaseRequired)
                .to_task(task_id.clone())
                .with_payload(serde_json::json!({"commits_behind": 3})),
        )
        .await;

    event_bus
        .publish(Event::new(EventKind::RebaseCompleted).from_task(task_id.clone()))
        .await;

    event_bus
        .publish(
            Event::new(EventKind::SyncRelayed)
                .from_task(TaskId("task-a".to_string()))
                .to_task(TaskId("task-b".to_string()))
                .with_payload(serde_json::json!({"learning": "use async"})),
        )
        .await;

    sleep(Duration::from_millis(300)).await;

    let events = received.lock().await;
    println!("Coordination events captured: {:?}", events);

    assert!(events.contains(&EventKind::MainUpdated));
    assert!(events.contains(&EventKind::RebaseRequired));
    assert!(events.contains(&EventKind::RebaseCompleted));
    assert!(events.contains(&EventKind::SyncRelayed));
}

/// Scenario 6: Event filtering by task
///
/// Question: Can we isolate events for specific tasks?
#[tokio::test]
async fn test_event_filtering_by_task() {
    let event_bus = Arc::new(EventBus::new());

    let task_a = TaskId("task-a".to_string());
    let task_b = TaskId("task-b".to_string());

    // Subscribe only to task_a events
    let mut sub_a = event_bus.subscribe_to_tasks(vec![task_a.clone()]);

    // Publish events from both tasks
    event_bus.task_started(task_a.clone()).await;
    event_bus.task_started(task_b.clone()).await;
    event_bus.file_modified(task_a.clone(), "/a.rs").await;
    event_bus.file_modified(task_b.clone(), "/b.rs").await;
    event_bus.task_completed(task_a.clone(), "Done A").await;
    event_bus.task_completed(task_b.clone(), "Done B").await;

    // Collect events for task_a
    let mut task_a_events = Vec::new();
    while let Some(event) = sub_a.try_recv() {
        task_a_events.push(event);
    }

    println!("Task A received {} events (should be 3)", task_a_events.len());
    assert_eq!(task_a_events.len(), 3, "Should only receive task-a events");

    for event in &task_a_events {
        assert_eq!(event.source_task.as_ref(), Some(&task_a));
    }
}

/// Print summary of observations
#[tokio::test]
async fn test_zz_print_observations() {
    println!("\n========================================");
    println!("MULTI-WORKSTREAM TEST OBSERVATIONS");
    println!("========================================\n");

    println!("What Works (validated by tests):");
    println!("  [x] EventBus handles concurrent publishers");
    println!("  [x] Task lifecycle events flow correctly");
    println!("  [x] Event history and querying works");
    println!("  [x] Event counts accurate under stress (1000 events)");
    println!("  [x] Coordination events (rebase, sync) work");
    println!("  [x] Event filtering by task works");
    println!();

    println!("What EventBus Provides:");
    println!("  - In-memory pub/sub (tokio broadcast)");
    println!("  - Bounded history (configurable size)");
    println!("  - Query by kind, task, timestamp");
    println!("  - Accurate event counts");
    println!();

    println!("What's MISSING (no persistence):");
    println!("  - Events lost on daemon restart");
    println!("  - No cross-session audit trail");
    println!("  - Can't answer 'what happened yesterday?'");
    println!();

    println!("What We Can't Test Here:");
    println!("  - Real LLM execution coordination");
    println!("  - Git worktree conflicts");
    println!("  - Actual rebase behavior");
    println!("  - Learning extraction/sharing");
    println!();

    println!("Recommendation:");
    println!("  EventBus is solid for in-process coordination.");
    println!("  Add event_log.jsonl for cross-session debugging.");
    println!("========================================\n");
}
