//! Integration tests for CostTracker.

use chrono::Utc;
use neuraphage::config::{BudgetAction, BudgetSettings, default_model_pricing};
use neuraphage::cost::{BudgetCheck, CostEntry, CostTracker};
use tempfile::TempDir;

#[tokio::test]
async fn test_cost_calculation_sonnet() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings::default();
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Sonnet: $3/M input, $15/M output
    let cost = tracker.calculate_cost("claude-sonnet-4-20250514", 1_000_000, 500_000);
    // (1M * $3/M) + (500K * $15/M) = $3 + $7.50 = $10.50
    assert!((cost - 10.50).abs() < 0.01, "Sonnet cost mismatch: got {}", cost);
}

#[tokio::test]
async fn test_cost_calculation_opus() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings::default();
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Opus: $15/M input, $75/M output
    let cost = tracker.calculate_cost("claude-opus-4", 1_000_000, 500_000);
    // (1M * $15/M) + (500K * $75/M) = $15 + $37.50 = $52.50
    assert!((cost - 52.50).abs() < 0.01, "Opus cost mismatch: got {}", cost);
}

#[tokio::test]
async fn test_cost_calculation_haiku() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings::default();
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Haiku: $0.25/M input, $1.25/M output
    let cost = tracker.calculate_cost("claude-haiku-3", 1_000_000, 500_000);
    // (1M * $0.25/M) + (500K * $1.25/M) = $0.25 + $0.625 = $0.875
    assert!((cost - 0.875).abs() < 0.01, "Haiku cost mismatch: got {}", cost);
}

#[tokio::test]
async fn test_record_and_get_stats() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings {
        daily: 50.0,
        monthly: 200.0,
        per_task: 10.0,
        warn_at: vec![0.5, 0.75, 0.9],
        on_exceeded: BudgetAction::Pause,
    };
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Record first entry
    let entry1 = CostEntry {
        timestamp: Utc::now(),
        task_id: "task-001".to_string(),
        model: "claude-sonnet-4".to_string(),
        input_tokens: 100_000,
        output_tokens: 50_000,
        cost: 1.05,
    };
    tracker.record_cost(entry1).await.unwrap();

    // Record second entry
    let entry2 = CostEntry {
        timestamp: Utc::now(),
        task_id: "task-002".to_string(),
        model: "claude-sonnet-4".to_string(),
        input_tokens: 200_000,
        output_tokens: 100_000,
        cost: 2.10,
    };
    tracker.record_cost(entry2).await.unwrap();

    // Check stats
    let stats = tracker.get_stats().await;
    assert!(
        (stats.daily_used - 3.15).abs() < 0.01,
        "Daily total mismatch: got {}",
        stats.daily_used
    );
    assert!(
        (stats.monthly_used - 3.15).abs() < 0.01,
        "Monthly total mismatch: got {}",
        stats.monthly_used
    );
}

#[tokio::test]
async fn test_per_task_cost() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings::default();
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Record entries for different tasks
    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "task-A".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cost: 1.50,
        })
        .await
        .unwrap();

    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "task-B".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 2000,
            output_tokens: 1000,
            cost: 3.00,
        })
        .await
        .unwrap();

    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "task-A".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 500,
            output_tokens: 250,
            cost: 0.75,
        })
        .await
        .unwrap();

    // Check per-task costs
    let task_a_cost = tracker.get_task_cost("task-A").await;
    let task_b_cost = tracker.get_task_cost("task-B").await;

    assert!(
        (task_a_cost - 2.25).abs() < 0.01,
        "Task A cost mismatch: got {}",
        task_a_cost
    );
    assert!(
        (task_b_cost - 3.00).abs() < 0.01,
        "Task B cost mismatch: got {}",
        task_b_cost
    );
}

#[tokio::test]
async fn test_budget_check_available() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings {
        daily: 100.0,
        monthly: 500.0,
        per_task: 20.0,
        warn_at: vec![0.5, 0.75, 0.9],
        on_exceeded: BudgetAction::Pause,
    };
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Small cost, should be available
    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "task-1".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cost: 1.00,
        })
        .await
        .unwrap();

    let check = tracker.check_budget("task-1").await;
    assert!(
        matches!(check, BudgetCheck::Available { .. }),
        "Expected Available, got {:?}",
        check
    );
}

#[tokio::test]
async fn test_budget_check_exceeded() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings {
        daily: 5.0, // Low limit
        monthly: 500.0,
        per_task: 20.0,
        warn_at: vec![0.5, 0.75, 0.9],
        on_exceeded: BudgetAction::Pause,
    };
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    // Exceed daily budget
    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "task-1".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cost: 6.00, // Over $5 daily limit
        })
        .await
        .unwrap();

    let check = tracker.check_budget("task-1").await;
    assert!(
        matches!(check, BudgetCheck::Exceeded { .. }),
        "Expected Exceeded, got {:?}",
        check
    );
}

#[tokio::test]
async fn test_ledger_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let data_path = temp_dir.path().to_path_buf();

    // Create tracker and record data
    {
        let settings = BudgetSettings::default();
        let pricing = default_model_pricing();
        let tracker = CostTracker::new(settings, pricing, &data_path).unwrap();

        tracker
            .record_cost(CostEntry {
                timestamp: Utc::now(),
                task_id: "persist-test".to_string(),
                model: "sonnet".to_string(),
                input_tokens: 1000,
                output_tokens: 500,
                cost: 5.55,
            })
            .await
            .unwrap();
    }
    // Tracker dropped here

    // Create new tracker and verify data persisted
    let settings2 = BudgetSettings::default();
    let pricing2 = default_model_pricing();
    let tracker2 = CostTracker::new(settings2, pricing2, &data_path).unwrap();

    let stats = tracker2.get_stats().await;
    assert!(
        (stats.daily_used - 5.55).abs() < 0.01,
        "Data not persisted: got {}",
        stats.daily_used
    );

    let task_cost = tracker2.get_task_cost("persist-test").await;
    assert!(
        (task_cost - 5.55).abs() < 0.01,
        "Task cost not persisted: got {}",
        task_cost
    );
}

#[tokio::test]
async fn test_ledger_file_format() {
    let temp_dir = TempDir::new().unwrap();
    let settings = BudgetSettings::default();
    let pricing = default_model_pricing();
    let tracker = CostTracker::new(settings, pricing, temp_dir.path()).unwrap();

    tracker
        .record_cost(CostEntry {
            timestamp: Utc::now(),
            task_id: "format-test".to_string(),
            model: "claude-sonnet-4".to_string(),
            input_tokens: 12345,
            output_tokens: 6789,
            cost: 1.23,
        })
        .await
        .unwrap();

    // Read and parse ledger file
    let ledger_path = temp_dir.path().join("cost_ledger.jsonl");
    let contents = std::fs::read_to_string(&ledger_path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();

    assert_eq!(lines.len(), 1, "Expected 1 line in ledger");

    let entry: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(entry["task_id"], "format-test");
    assert_eq!(entry["model"], "claude-sonnet-4");
    assert_eq!(entry["input_tokens"], 12345);
    assert_eq!(entry["output_tokens"], 6789);
    assert!((entry["cost"].as_f64().unwrap() - 1.23).abs() < 0.01);
}
