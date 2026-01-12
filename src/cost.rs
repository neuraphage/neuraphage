//! Cost tracking for budget enforcement.
//!
//! CostTracker manages global cost limits across all tasks, persisting
//! costs to a JSONL ledger file for survival across daemon restarts.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::{BudgetAction, BudgetSettings, ModelPricingMap};
use crate::error::Result;

/// A cost entry in the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostEntry {
    /// When the cost was incurred.
    pub timestamp: DateTime<Utc>,
    /// Task that incurred the cost.
    pub task_id: String,
    /// Model used.
    pub model: String,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Cost in USD.
    pub cost: f64,
}

/// Result of checking budget availability.
#[derive(Debug, Clone)]
pub enum BudgetCheck {
    /// Budget available, can proceed.
    Available {
        /// Current daily spend.
        daily_used: f64,
        /// Current monthly spend.
        monthly_used: f64,
    },
    /// Budget warning threshold crossed.
    Warning {
        budget_type: BudgetType,
        threshold: f64,
        current: f64,
        limit: f64,
    },
    /// Budget exceeded.
    Exceeded {
        budget_type: BudgetType,
        current: f64,
        limit: f64,
    },
}

/// Type of budget limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BudgetType {
    Daily,
    Monthly,
    PerTask,
}

impl std::fmt::Display for BudgetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BudgetType::Daily => write!(f, "daily"),
            BudgetType::Monthly => write!(f, "monthly"),
            BudgetType::PerTask => write!(f, "per-task"),
        }
    }
}

/// In-memory cache of current period totals.
#[derive(Debug, Default)]
struct CostCache {
    /// Total spend today.
    daily_total: f64,
    /// Total spend this month.
    monthly_total: f64,
    /// Per-task totals for active tasks.
    task_totals: HashMap<String, f64>,
    /// Date of daily total (for rollover).
    daily_date: Option<NaiveDate>,
    /// Month of monthly total (for rollover).
    monthly_month: Option<u32>,
}

/// Tracks costs globally across all tasks.
pub struct CostTracker {
    /// Budget configuration.
    settings: BudgetSettings,
    /// Model pricing.
    pricing: ModelPricingMap,
    /// Path to cost ledger file.
    ledger_path: PathBuf,
    /// In-memory cache of current period totals.
    cache: RwLock<CostCache>,
    /// Warnings already emitted (to avoid spam).
    emitted_warnings: RwLock<Vec<(BudgetType, u32)>>, // threshold as permille
}

impl CostTracker {
    /// Create a new cost tracker.
    pub fn new(settings: BudgetSettings, pricing: ModelPricingMap, data_dir: &Path) -> Result<Self> {
        let ledger_path = data_dir.join("cost_ledger.jsonl");

        // Load existing ledger synchronously before creating the tracker
        let cache = Self::load_ledger_sync(&ledger_path)?;

        Ok(Self {
            settings,
            pricing,
            ledger_path,
            cache: RwLock::new(cache),
            emitted_warnings: RwLock::new(Vec::new()),
        })
    }

    /// Load cost ledger from disk synchronously.
    /// Called only once during construction.
    fn load_ledger_sync(ledger_path: &Path) -> Result<CostCache> {
        let mut cache = CostCache::default();

        if !ledger_path.exists() {
            return Ok(cache);
        }

        let file = File::open(ledger_path)?;
        let reader = BufReader::new(file);
        let now = Utc::now();
        let today = now.date_naive();
        let this_month = now.month();
        let this_year = now.year();

        cache.daily_date = Some(today);
        cache.monthly_month = Some(this_month);

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("Failed to read ledger line: {}", e);
                    continue;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let entry: CostEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("Skipping invalid ledger entry: {}", e);
                    continue;
                }
            };

            // Add to monthly total if same month and year
            if entry.timestamp.month() == this_month && entry.timestamp.year() == this_year {
                cache.monthly_total += entry.cost;

                // Add to daily total if today
                if entry.timestamp.date_naive() == today {
                    cache.daily_total += entry.cost;
                }
            }

            // Update task total
            *cache.task_totals.entry(entry.task_id).or_insert(0.0) += entry.cost;
        }

        log::debug!(
            "Loaded cost ledger: daily=${:.2}, monthly=${:.2}",
            cache.daily_total,
            cache.monthly_total
        );

        Ok(cache)
    }

    /// Calculate cost for a model and token usage.
    pub fn calculate_cost(&self, model: &str, input_tokens: u64, output_tokens: u64) -> f64 {
        // Find pricing by matching model name (case-insensitive contains)
        // Sort by pattern length descending to match most specific first
        // e.g., "opus-4-5" should match before "opus-4" or "opus"
        let model_lower = model.to_lowercase();
        let mut matches: Vec<_> = self
            .pricing
            .iter()
            .filter(|(pattern, _)| model_lower.contains(&pattern.to_lowercase()))
            .collect();
        matches.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        let pricing = matches.first().map(|(_, p)| *p);

        match pricing {
            Some(p) => {
                let input_cost = (input_tokens as f64 / 1_000_000.0) * p.input;
                let output_cost = (output_tokens as f64 / 1_000_000.0) * p.output;
                input_cost + output_cost
            }
            None => {
                // Default to Sonnet pricing if model unknown
                log::warn!("Unknown model '{}', using Sonnet pricing", model);
                let input_cost = (input_tokens as f64 / 1_000_000.0) * 3.0;
                let output_cost = (output_tokens as f64 / 1_000_000.0) * 15.0;
                input_cost + output_cost
            }
        }
    }

    /// Check if budget is available for a task.
    pub async fn check_budget(&self, task_id: &str) -> BudgetCheck {
        let cache = self.cache.read().await;
        let task_total = cache.task_totals.get(task_id).copied().unwrap_or(0.0);

        // Check per-task limit first
        if self.settings.per_task > 0.0 && task_total >= self.settings.per_task {
            return BudgetCheck::Exceeded {
                budget_type: BudgetType::PerTask,
                current: task_total,
                limit: self.settings.per_task,
            };
        }

        // Check daily limit
        if self.settings.daily > 0.0 {
            if cache.daily_total >= self.settings.daily {
                return BudgetCheck::Exceeded {
                    budget_type: BudgetType::Daily,
                    current: cache.daily_total,
                    limit: self.settings.daily,
                };
            }

            // Check for warnings
            for threshold in &self.settings.warn_at {
                let warn_amount = self.settings.daily * threshold;
                if cache.daily_total >= warn_amount {
                    let threshold_permille = (*threshold * 1000.0) as u32;
                    let mut emitted = self.emitted_warnings.write().await;
                    let key = (BudgetType::Daily, threshold_permille);
                    if !emitted.contains(&key) {
                        emitted.push(key);
                        return BudgetCheck::Warning {
                            budget_type: BudgetType::Daily,
                            threshold: *threshold,
                            current: cache.daily_total,
                            limit: self.settings.daily,
                        };
                    }
                }
            }
        }

        // Check monthly limit
        if self.settings.monthly > 0.0 {
            if cache.monthly_total >= self.settings.monthly {
                return BudgetCheck::Exceeded {
                    budget_type: BudgetType::Monthly,
                    current: cache.monthly_total,
                    limit: self.settings.monthly,
                };
            }

            // Check for warnings
            for threshold in &self.settings.warn_at {
                let warn_amount = self.settings.monthly * threshold;
                if cache.monthly_total >= warn_amount {
                    let threshold_permille = (*threshold * 1000.0) as u32;
                    let mut emitted = self.emitted_warnings.write().await;
                    let key = (BudgetType::Monthly, threshold_permille);
                    if !emitted.contains(&key) {
                        emitted.push(key);
                        return BudgetCheck::Warning {
                            budget_type: BudgetType::Monthly,
                            threshold: *threshold,
                            current: cache.monthly_total,
                            limit: self.settings.monthly,
                        };
                    }
                }
            }
        }

        BudgetCheck::Available {
            daily_used: cache.daily_total,
            monthly_used: cache.monthly_total,
        }
    }

    /// Record a cost entry.
    pub async fn record_cost(&self, entry: CostEntry) -> Result<()> {
        // Update cache
        {
            let mut cache = self.cache.write().await;
            let now = Utc::now();
            let today = now.date_naive();
            let this_month = now.month();

            // Handle day rollover
            if cache.daily_date != Some(today) {
                cache.daily_total = 0.0;
                cache.daily_date = Some(today);
                // Reset daily warnings
                let mut emitted = self.emitted_warnings.write().await;
                emitted.retain(|(bt, _)| *bt != BudgetType::Daily);
            }

            // Handle month rollover
            if cache.monthly_month != Some(this_month) {
                cache.monthly_total = 0.0;
                cache.monthly_month = Some(this_month);
                // Reset monthly warnings
                let mut emitted = self.emitted_warnings.write().await;
                emitted.retain(|(bt, _)| *bt != BudgetType::Monthly);
            }

            cache.daily_total += entry.cost;
            cache.monthly_total += entry.cost;
            *cache.task_totals.entry(entry.task_id.clone()).or_insert(0.0) += entry.cost;
        }

        // Append to ledger file
        let mut file = OpenOptions::new().create(true).append(true).open(&self.ledger_path)?;

        let line = serde_json::to_string(&entry)?;
        writeln!(file, "{}", line)?;

        log::debug!(
            "Recorded cost: task={}, model={}, cost=${:.4}",
            entry.task_id,
            entry.model,
            entry.cost
        );

        Ok(())
    }

    /// Get current usage stats.
    pub async fn get_stats(&self) -> CostStats {
        let cache = self.cache.read().await;
        CostStats {
            daily_used: cache.daily_total,
            daily_limit: if self.settings.daily > 0.0 { Some(self.settings.daily) } else { None },
            monthly_used: cache.monthly_total,
            monthly_limit: if self.settings.monthly > 0.0 { Some(self.settings.monthly) } else { None },
        }
    }

    /// Get cost for a specific task.
    pub async fn get_task_cost(&self, task_id: &str) -> f64 {
        let cache = self.cache.read().await;
        cache.task_totals.get(task_id).copied().unwrap_or(0.0)
    }

    /// Clear task from cache (when task completes).
    pub async fn clear_task(&self, task_id: &str) {
        let mut cache = self.cache.write().await;
        cache.task_totals.remove(task_id);
    }

    /// Get action to take when budget is exceeded.
    pub fn on_exceeded_action(&self) -> BudgetAction {
        self.settings.on_exceeded
    }

    /// Get the ledger path.
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }
}

/// Cost usage statistics.
#[derive(Debug, Clone)]
pub struct CostStats {
    pub daily_used: f64,
    pub daily_limit: Option<f64>,
    pub monthly_used: f64,
    pub monthly_limit: Option<f64>,
}

impl CostStats {
    /// Format as human-readable string.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();

        if let Some(limit) = self.daily_limit {
            parts.push(format!(
                "Daily: ${:.2} / ${:.2} ({:.0}%)",
                self.daily_used,
                limit,
                (self.daily_used / limit) * 100.0
            ));
        } else {
            parts.push(format!("Daily: ${:.2} (no limit)", self.daily_used));
        }

        if let Some(limit) = self.monthly_limit {
            parts.push(format!(
                "Monthly: ${:.2} / ${:.2} ({:.0}%)",
                self.monthly_used,
                limit,
                (self.monthly_used / limit) * 100.0
            ));
        } else {
            parts.push(format!("Monthly: ${:.2} (no limit)", self.monthly_used));
        }

        parts.join(" | ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_settings() -> BudgetSettings {
        BudgetSettings {
            daily: 5.0,
            monthly: 50.0,
            per_task: 2.0,
            warn_at: vec![0.5, 0.9],
            on_exceeded: BudgetAction::Pause,
        }
    }

    fn make_pricing() -> ModelPricingMap {
        let mut m = HashMap::new();
        m.insert(
            "sonnet".to_string(),
            crate::config::ModelPricing {
                input: 3.0,
                output: 15.0,
            },
        );
        // Opus 4.5 (more specific, should match first)
        m.insert(
            "opus-4-5".to_string(),
            crate::config::ModelPricing {
                input: 5.0,
                output: 25.0,
            },
        );
        // Opus 4 (legacy)
        m.insert(
            "opus-4".to_string(),
            crate::config::ModelPricing {
                input: 15.0,
                output: 75.0,
            },
        );
        // Generic opus fallback (uses 4.5 pricing)
        m.insert(
            "opus".to_string(),
            crate::config::ModelPricing {
                input: 5.0,
                output: 25.0,
            },
        );
        m
    }

    #[test]
    fn test_calculate_cost_sonnet() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // 1M input + 1M output for sonnet = $3 + $15 = $18
        let cost = tracker.calculate_cost("claude-sonnet-4-20250514", 1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);

        // 10K input + 5K output for sonnet = $0.03 + $0.075 = $0.105
        let cost = tracker.calculate_cost("sonnet", 10_000, 5_000);
        assert!((cost - 0.105).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost_opus_4() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // Opus 4: 1M input + 1M output = $15 + $75 = $90
        let cost = tracker.calculate_cost("claude-opus-4-20250514", 1_000_000, 1_000_000);
        assert!((cost - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost_opus_4_5() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // Opus 4.5: 1M input + 1M output = $5 + $25 = $30
        let cost = tracker.calculate_cost("claude-opus-4-5-20251101", 1_000_000, 1_000_000);
        assert!((cost - 30.0).abs() < 0.001);

        // Generic "opus" should use 4.5 pricing (current default)
        let cost = tracker.calculate_cost("opus", 1_000_000, 1_000_000);
        assert!((cost - 30.0).abs() < 0.001);
    }

    #[test]
    fn test_calculate_cost_unknown_model() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // Unknown model defaults to sonnet pricing
        let cost = tracker.calculate_cost("unknown-model", 1_000_000, 1_000_000);
        assert!((cost - 18.0).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_record_and_retrieve_cost() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        let entry = CostEntry {
            timestamp: Utc::now(),
            task_id: "task-123".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 10_000,
            output_tokens: 5_000,
            cost: 0.105,
        };

        tracker.record_cost(entry).await.unwrap();

        let task_cost = tracker.get_task_cost("task-123").await;
        assert!((task_cost - 0.105).abs() < 0.001);

        let stats = tracker.get_stats().await;
        assert!((stats.daily_used - 0.105).abs() < 0.001);
        assert!((stats.monthly_used - 0.105).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_budget_check_available() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        let check = tracker.check_budget("task-new").await;
        assert!(matches!(check, BudgetCheck::Available { .. }));
    }

    #[tokio::test]
    async fn test_budget_check_per_task_exceeded() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // Record cost that exceeds per-task limit ($2)
        let entry = CostEntry {
            timestamp: Utc::now(),
            task_id: "task-expensive".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 100_000,
            output_tokens: 100_000,
            cost: 2.5,
        };
        tracker.record_cost(entry).await.unwrap();

        let check = tracker.check_budget("task-expensive").await;
        assert!(matches!(
            check,
            BudgetCheck::Exceeded {
                budget_type: BudgetType::PerTask,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_budget_check_daily_warning() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        // Record cost that triggers 50% warning ($5 daily * 0.5 = $2.50)
        let entry = CostEntry {
            timestamp: Utc::now(),
            task_id: "task-1".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 100_000,
            output_tokens: 100_000,
            cost: 2.6,
        };
        tracker.record_cost(entry).await.unwrap();

        let check = tracker.check_budget("task-other").await;
        assert!(matches!(
            check,
            BudgetCheck::Warning {
                budget_type: BudgetType::Daily,
                threshold,
                ..
            } if (threshold - 0.5).abs() < 0.01
        ));

        // Second check should not return warning again (already emitted)
        let check2 = tracker.check_budget("task-other").await;
        assert!(matches!(check2, BudgetCheck::Available { .. }));
    }

    #[tokio::test]
    async fn test_ledger_persistence() {
        let temp = TempDir::new().unwrap();

        // First session - record cost
        {
            let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

            let entry = CostEntry {
                timestamp: Utc::now(),
                task_id: "task-persist".to_string(),
                model: "sonnet".to_string(),
                input_tokens: 10_000,
                output_tokens: 5_000,
                cost: 0.50,
            };
            tracker.record_cost(entry).await.unwrap();
        }

        // Second session - should load from ledger
        {
            let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

            let stats = tracker.get_stats().await;
            assert!((stats.daily_used - 0.50).abs() < 0.001);
            assert!((stats.monthly_used - 0.50).abs() < 0.001);
        }
    }

    #[test]
    fn test_cost_stats_display() {
        let stats = CostStats {
            daily_used: 2.50,
            daily_limit: Some(5.0),
            monthly_used: 25.0,
            monthly_limit: Some(50.0),
        };

        let display = stats.display();
        assert!(display.contains("Daily: $2.50 / $5.00 (50%)"));
        assert!(display.contains("Monthly: $25.00 / $50.00 (50%)"));
    }

    #[test]
    fn test_cost_stats_display_no_limit() {
        let stats = CostStats {
            daily_used: 2.50,
            daily_limit: None,
            monthly_used: 25.0,
            monthly_limit: None,
        };

        let display = stats.display();
        assert!(display.contains("Daily: $2.50 (no limit)"));
        assert!(display.contains("Monthly: $25.00 (no limit)"));
    }

    #[tokio::test]
    async fn test_clear_task() {
        let temp = TempDir::new().unwrap();
        let tracker = CostTracker::new(make_settings(), make_pricing(), temp.path()).unwrap();

        let entry = CostEntry {
            timestamp: Utc::now(),
            task_id: "task-clear".to_string(),
            model: "sonnet".to_string(),
            input_tokens: 10_000,
            output_tokens: 5_000,
            cost: 0.10,
        };
        tracker.record_cost(entry).await.unwrap();

        assert!(tracker.get_task_cost("task-clear").await > 0.0);

        tracker.clear_task("task-clear").await;

        assert!(tracker.get_task_cost("task-clear").await < 0.001);
    }
}
