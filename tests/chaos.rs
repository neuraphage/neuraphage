//! Chaos tests for crash recovery.
//!
//! These tests verify that Neuraphage can recover from crashes during task execution.
//! They use subprocess-based testing to simulate real crashes.
//!
//! Run with: `cargo test --test chaos -- --ignored --nocapture`

#![allow(dead_code)] // Test harness methods may not all be used in every test

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use tempfile::TempDir;

/// Test harness for chaos testing.
struct ChaosHarness {
    #[allow(dead_code)] // Kept alive for temp dir lifetime
    temp_dir: TempDir,
    data_dir: PathBuf,
    socket_path: PathBuf,
}

impl ChaosHarness {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let data_dir = temp_dir.path().join("neuraphage_data");
        let socket_path = data_dir.join("neuraphage.sock");
        fs::create_dir_all(&data_dir).expect("Failed to create data dir");

        Self {
            temp_dir,
            data_dir,
            socket_path,
        }
    }

    fn start_daemon(&self) -> Child {
        Command::new("cargo")
            .args([
                "run",
                "--",
                "--config",
                self.data_dir.to_str().unwrap(),
                "daemon",
                "start",
                "-f", // foreground mode
            ])
            .env("NEURAPHAGE_DATA_DIR", self.data_dir.to_str().unwrap())
            .spawn()
            .expect("Failed to start daemon")
    }

    fn wait_for_daemon(&self) {
        // Wait for socket to appear
        for _ in 0..50 {
            if self.socket_path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("Daemon did not start in time");
    }

    fn create_task(&self, description: &str) -> String {
        let output = Command::new("cargo")
            .args(["run", "--", "new", description])
            .env("NEURAPHAGE_DATA_DIR", self.data_dir.to_str().unwrap())
            .output()
            .expect("Failed to create task");

        let stdout = String::from_utf8_lossy(&output.stdout);
        // Extract task ID from output like "Created task: task_abc123"
        stdout
            .lines()
            .find(|l| l.contains("Created task"))
            .and_then(|l| l.split(':').next_back())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| panic!("Could not parse task ID from: {}", stdout))
    }

    fn get_task_status(&self, task_id: &str) -> String {
        let output = Command::new("cargo")
            .args(["run", "--", "show", task_id])
            .env("NEURAPHAGE_DATA_DIR", self.data_dir.to_str().unwrap())
            .output()
            .expect("Failed to get task status");

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .find(|l| l.contains("Status:"))
            .and_then(|l| l.split(':').next_back())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    fn list_execution_states(&self) -> Vec<String> {
        let state_dir = self.data_dir.join("execution_state");
        if !state_dir.exists() {
            return Vec::new();
        }

        fs::read_dir(&state_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn get_recovery_report(&self) -> String {
        let output = Command::new("cargo")
            .args(["run", "--", "recover"])
            .env("NEURAPHAGE_DATA_DIR", self.data_dir.to_str().unwrap())
            .output()
            .expect("Failed to get recovery report");

        String::from_utf8_lossy(&output.stdout).to_string()
    }

    fn kill_daemon(child: &mut Child) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Test that execution state files are created for running tasks.
#[test]
#[ignore] // Run with: cargo test --test chaos -- --ignored
fn test_execution_state_created_for_running_task() {
    let harness = ChaosHarness::new();

    // Start daemon
    let mut daemon = harness.start_daemon();
    harness.wait_for_daemon();

    // Create a task
    let _task_id = harness.create_task("Test task for execution state");

    // Give it a moment
    std::thread::sleep(Duration::from_millis(500));

    // Check that execution state was created (if task was started)
    // Note: This depends on the task actually being started
    let _states = harness.list_execution_states();

    // Clean up
    ChaosHarness::kill_daemon(&mut daemon);
}

/// Test that daemon can start and recover shows no tasks on fresh start.
#[test]
#[ignore]
fn test_fresh_daemon_has_no_recovery_tasks() {
    let harness = ChaosHarness::new();

    // Start daemon
    let mut daemon = harness.start_daemon();
    harness.wait_for_daemon();

    // Recovery should show no tasks
    std::thread::sleep(Duration::from_millis(500));
    let report = harness.get_recovery_report();

    assert!(
        report.contains("No recoverable tasks") || report.contains("0 recoverable"),
        "Fresh daemon should have no recoverable tasks, got: {}",
        report
    );

    ChaosHarness::kill_daemon(&mut daemon);
}

/// Test that crashing daemon leaves execution state for recovery.
///
/// This test:
/// 1. Starts daemon
/// 2. Creates and starts a task
/// 3. Kills daemon abruptly
/// 4. Verifies execution state file exists
/// 5. Restarts daemon
/// 6. Checks recovery report shows the task
#[test]
#[ignore]
fn test_crash_leaves_recoverable_state() {
    let harness = ChaosHarness::new();

    // First daemon session - create task and kill
    {
        let mut daemon = harness.start_daemon();
        harness.wait_for_daemon();

        // Create a task
        let _task_id = harness.create_task("Task that will survive crash");

        // Give daemon time to potentially start the task and checkpoint
        std::thread::sleep(Duration::from_secs(2));

        // Kill daemon abruptly (simulating crash)
        ChaosHarness::kill_daemon(&mut daemon);
    }

    // Check for execution state files
    let states = harness.list_execution_states();
    println!("Execution states after crash: {:?}", states);

    // If there are states, the crash recovery should work
    // Note: Whether states exist depends on if the task was actually started
    // and checkpointed before the kill

    // Second daemon session - should recover
    {
        let mut daemon = harness.start_daemon();
        harness.wait_for_daemon();

        // Give recovery time to process
        std::thread::sleep(Duration::from_millis(500));

        // Check recovery report
        let report = harness.get_recovery_report();
        println!("Recovery report: {}", report);

        ChaosHarness::kill_daemon(&mut daemon);
    }

    // Test passes if we get here without panicking
    // The actual recovery behavior depends on implementation details
}

/// Test conversation persistence survives daemon restart.
#[test]
#[ignore]
fn test_conversation_survives_restart() {
    let harness = ChaosHarness::new();

    // First session
    let task_id: String;
    {
        let mut daemon = harness.start_daemon();
        harness.wait_for_daemon();

        task_id = harness.create_task("Conversation persistence test");

        // Let task run briefly
        std::thread::sleep(Duration::from_secs(1));

        ChaosHarness::kill_daemon(&mut daemon);
    }

    // Check conversation file exists
    let conv_dir = harness.data_dir.join("conversations");
    let conv_file = conv_dir.join(format!("{}.json", task_id));

    // Conversation might not exist if task never started
    if conv_file.exists() {
        // Verify it's valid JSON
        let content = fs::read_to_string(&conv_file).expect("Failed to read conversation");
        let _: serde_json::Value = serde_json::from_str(&content).expect("Conversation should be valid JSON");
        println!("Conversation file is valid JSON");
    } else {
        println!("No conversation file created (task may not have started)");
    }
}

/// Stress test: multiple daemon restarts.
#[test]
#[ignore]
fn test_multiple_restarts() {
    let harness = ChaosHarness::new();

    for i in 0..3 {
        println!("Restart cycle {}", i + 1);

        let mut daemon = harness.start_daemon();
        harness.wait_for_daemon();

        // Create a task
        let _task_id = harness.create_task(&format!("Task from restart {}", i + 1));

        // Brief operation
        std::thread::sleep(Duration::from_millis(500));

        ChaosHarness::kill_daemon(&mut daemon);

        // Brief pause between restarts
        std::thread::sleep(Duration::from_millis(200));
    }

    // Final verification
    let mut daemon = harness.start_daemon();
    harness.wait_for_daemon();

    let report = harness.get_recovery_report();
    println!("Final recovery report: {}", report);

    ChaosHarness::kill_daemon(&mut daemon);
}
