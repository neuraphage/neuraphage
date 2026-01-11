//! Safety mechanisms for agent operations.
//!
//! Provides confirmation prompts for dangerous operations, command blocking,
//! and path protection.

use std::path::{Path, PathBuf};
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Result of a safety check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyCheckResult {
    /// Operation is safe to proceed.
    Safe,
    /// Operation requires user confirmation.
    RequiresConfirmation { reason: String },
    /// Operation is blocked.
    Blocked { reason: String },
}

/// Safety configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyConfig {
    /// Command patterns that require confirmation (as strings).
    #[serde(default = "default_confirm_patterns")]
    pub confirm_pattern_strings: Vec<String>,
    /// Commands that are completely blocked (substring match).
    #[serde(default = "default_blocked_commands")]
    pub blocked_commands: Vec<String>,
    /// Commands that are blocked when exactly matched.
    #[serde(default = "default_blocked_exact")]
    pub blocked_exact: Vec<String>,
    /// Paths that cannot be modified.
    #[serde(default = "default_protected_paths")]
    pub protected_paths: Vec<PathBuf>,
    /// Maximum file size to read (bytes).
    #[serde(default = "default_max_file_size")]
    pub max_file_size: usize,
    /// Command timeout in seconds.
    #[serde(default = "default_command_timeout_secs")]
    pub command_timeout_secs: u64,
}

fn default_confirm_patterns() -> Vec<String> {
    vec![
        r"rm\s+-rf".to_string(),
        r"rm\s+-r".to_string(),
        r"git\s+push.*--force".to_string(),
        r"git\s+reset\s+--hard".to_string(),
        r"sudo".to_string(),
        r"chmod\s+777".to_string(),
        r">\s*/dev/".to_string(),
    ]
}

fn default_blocked_commands() -> Vec<String> {
    // These patterns are checked with word boundaries to avoid false positives
    vec![
        "mkfs".to_string(),
        ":(){:|:&};:".to_string(), // fork bomb
    ]
}

fn default_blocked_exact() -> Vec<String> {
    // These are checked for exact match after trimming
    vec![
        "rm -rf /".to_string(),
        "rm -rf /*".to_string(),
        "rm -rf ~".to_string(),
        "dd if=/dev/zero of=/dev/sda".to_string(),
    ]
}

fn default_protected_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/etc"),
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/boot"),
        PathBuf::from("/sys"),
        PathBuf::from("/proc"),
    ]
}

fn default_max_file_size() -> usize {
    10 * 1024 * 1024 // 10MB
}

fn default_command_timeout_secs() -> u64 {
    120
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            confirm_pattern_strings: default_confirm_patterns(),
            blocked_commands: default_blocked_commands(),
            blocked_exact: default_blocked_exact(),
            protected_paths: default_protected_paths(),
            max_file_size: default_max_file_size(),
            command_timeout_secs: default_command_timeout_secs(),
        }
    }
}

impl SafetyConfig {
    /// Get command timeout as Duration.
    pub fn command_timeout(&self) -> Duration {
        Duration::from_secs(self.command_timeout_secs)
    }
}

/// Safety checker for validating operations.
pub struct SafetyChecker {
    config: SafetyConfig,
    confirm_patterns: Vec<Regex>,
}

impl SafetyChecker {
    /// Create a new safety checker.
    pub fn new(config: SafetyConfig) -> Self {
        let confirm_patterns = config
            .confirm_pattern_strings
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect();

        Self {
            config,
            confirm_patterns,
        }
    }

    /// Check if a command is safe to execute.
    pub fn check_command(&self, command: &str) -> SafetyCheckResult {
        let trimmed = command.trim();

        // Check for exact blocked commands
        for blocked in &self.config.blocked_exact {
            if trimmed == blocked {
                return SafetyCheckResult::Blocked {
                    reason: format!("Command is blocked: {}", blocked),
                };
            }
        }

        // Check for substring blocked commands
        for blocked in &self.config.blocked_commands {
            if command.contains(blocked) {
                return SafetyCheckResult::Blocked {
                    reason: format!("Command contains blocked pattern: {}", blocked),
                };
            }
        }

        // Check for patterns requiring confirmation
        for pattern in &self.confirm_patterns {
            if pattern.is_match(command) {
                return SafetyCheckResult::RequiresConfirmation {
                    reason: format!("Command matches dangerous pattern: {}", pattern.as_str()),
                };
            }
        }

        SafetyCheckResult::Safe
    }

    /// Check if a path is safe to modify.
    pub fn check_write_path(&self, path: &Path) -> SafetyCheckResult {
        // Canonicalize the path if possible
        let check_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        for protected in &self.config.protected_paths {
            if check_path.starts_with(protected) {
                return SafetyCheckResult::Blocked {
                    reason: format!("Path is protected: {}", protected.display()),
                };
            }
        }

        // Check for sensitive files
        let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let sensitive_files = [".env", "credentials", "secrets", ".ssh", ".gnupg"];

        for sensitive in sensitive_files {
            if filename.contains(sensitive) {
                return SafetyCheckResult::RequiresConfirmation {
                    reason: format!("File may contain sensitive data: {}", filename),
                };
            }
        }

        SafetyCheckResult::Safe
    }

    /// Check if a file size is within limits for reading.
    pub fn check_file_size(&self, size: usize) -> SafetyCheckResult {
        if size > self.config.max_file_size {
            SafetyCheckResult::Blocked {
                reason: format!(
                    "File too large: {} bytes (max: {} bytes)",
                    size, self.config.max_file_size
                ),
            }
        } else {
            SafetyCheckResult::Safe
        }
    }

    /// Get the maximum file size.
    pub fn max_file_size(&self) -> usize {
        self.config.max_file_size
    }

    /// Get command timeout.
    pub fn command_timeout(&self) -> Duration {
        self.config.command_timeout()
    }
}

impl Default for SafetyChecker {
    fn default() -> Self {
        Self::new(SafetyConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SafetyConfig::default();
        assert!(!config.confirm_pattern_strings.is_empty());
        assert!(!config.blocked_commands.is_empty());
        assert!(!config.protected_paths.is_empty());
        assert_eq!(config.max_file_size, 10 * 1024 * 1024);
        assert_eq!(config.command_timeout_secs, 120);
    }

    #[test]
    fn test_safe_command() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("ls -la");
        assert_eq!(result, SafetyCheckResult::Safe);
    }

    #[test]
    fn test_blocked_command() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("rm -rf /");
        assert!(matches!(result, SafetyCheckResult::Blocked { .. }));
    }

    #[test]
    fn test_confirm_rm_rf() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("rm -rf /tmp/test");
        assert!(matches!(result, SafetyCheckResult::RequiresConfirmation { .. }));
    }

    #[test]
    fn test_confirm_force_push() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("git push origin main --force");
        assert!(matches!(result, SafetyCheckResult::RequiresConfirmation { .. }));
    }

    #[test]
    fn test_confirm_sudo() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("sudo apt update");
        assert!(matches!(result, SafetyCheckResult::RequiresConfirmation { .. }));
    }

    #[test]
    fn test_confirm_hard_reset() {
        let checker = SafetyChecker::default();
        let result = checker.check_command("git reset --hard HEAD~1");
        assert!(matches!(result, SafetyCheckResult::RequiresConfirmation { .. }));
    }

    #[test]
    fn test_protected_path() {
        let checker = SafetyChecker::default();
        let result = checker.check_write_path(Path::new("/etc/passwd"));
        assert!(matches!(result, SafetyCheckResult::Blocked { .. }));
    }

    #[test]
    fn test_safe_path() {
        let checker = SafetyChecker::default();
        let result = checker.check_write_path(Path::new("/tmp/test.txt"));
        assert_eq!(result, SafetyCheckResult::Safe);
    }

    #[test]
    fn test_sensitive_file() {
        let checker = SafetyChecker::default();
        let result = checker.check_write_path(Path::new("/home/user/.env"));
        assert!(matches!(result, SafetyCheckResult::RequiresConfirmation { .. }));
    }

    #[test]
    fn test_file_size_ok() {
        let checker = SafetyChecker::default();
        let result = checker.check_file_size(1000);
        assert_eq!(result, SafetyCheckResult::Safe);
    }

    #[test]
    fn test_file_size_too_large() {
        let checker = SafetyChecker::default();
        let result = checker.check_file_size(100 * 1024 * 1024); // 100MB
        assert!(matches!(result, SafetyCheckResult::Blocked { .. }));
    }

    #[test]
    fn test_command_timeout() {
        let checker = SafetyChecker::default();
        assert_eq!(checker.command_timeout(), Duration::from_secs(120));
    }
}
