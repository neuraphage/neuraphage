//! Configuration for Neuraphage.

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::sandbox::SandboxMode;

/// Neuraphage configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Data directory for engram store and daemon files.
    pub data_dir: PathBuf,
    /// Maximum concurrent running tasks.
    pub max_concurrent_tasks: usize,
    /// Default task priority (0-4).
    pub default_priority: u8,
    /// Enable debug mode.
    pub debug: bool,
    /// Daemon configuration.
    pub daemon: DaemonSettings,
    /// API configuration.
    pub api: ApiSettings,
    /// Sandbox configuration.
    pub sandbox: SandboxSettings,
}

impl Default for Config {
    fn default() -> Self {
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("neuraphage");

        Self {
            data_dir,
            max_concurrent_tasks: 5,
            default_priority: 2,
            debug: false,
            daemon: DaemonSettings::default(),
            api: ApiSettings::default(),
            sandbox: SandboxSettings::default(),
        }
    }
}

impl Config {
    /// Load configuration with fallback chain.
    pub fn load(config_path: Option<&PathBuf>) -> Result<Self> {
        // If explicit config path provided, try to load it
        if let Some(path) = config_path {
            return Self::load_from_file(path).context(format!("Failed to load config from {}", path.display()));
        }

        // Try primary location: ~/.config/neuraphage/neuraphage.yml
        if let Some(config_dir) = dirs::config_dir() {
            let primary_config = config_dir.join("neuraphage").join("neuraphage.yml");
            if primary_config.exists() {
                match Self::load_from_file(&primary_config) {
                    Ok(config) => return Ok(config),
                    Err(e) => {
                        log::warn!("Failed to load config from {}: {}", primary_config.display(), e);
                    }
                }
            }
        }

        // Try fallback location: ./neuraphage.yml
        let fallback_config = PathBuf::from("neuraphage.yml");
        if fallback_config.exists() {
            match Self::load_from_file(&fallback_config) {
                Ok(config) => return Ok(config),
                Err(e) => {
                    log::warn!("Failed to load config from {}: {}", fallback_config.display(), e);
                }
            }
        }

        // No config file found, use defaults
        log::info!("No config file found, using defaults");
        Ok(Self::default())
    }

    fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(&path).context("Failed to read config file")?;

        let config: Self = serde_yaml::from_str(&content).context("Failed to parse config file")?;

        log::info!("Loaded config from: {}", path.as_ref().display());
        Ok(config)
    }

    /// Get the socket path for the daemon.
    pub fn socket_path(&self) -> PathBuf {
        self.data_dir.join("neuraphage.sock")
    }

    /// Get the PID file path.
    pub fn pid_path(&self) -> PathBuf {
        self.data_dir.join("neuraphage.pid")
    }

    /// Convert to DaemonConfig.
    pub fn to_daemon_config(&self) -> crate::daemon::DaemonConfig {
        crate::daemon::DaemonConfig {
            socket_path: self.socket_path(),
            pid_path: self.pid_path(),
            data_path: self.data_dir.clone(),
        }
    }
}

/// Daemon-specific settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Auto-start daemon if not running.
    pub auto_start: bool,
    /// Shutdown daemon after idle timeout (seconds, 0 = never).
    pub idle_timeout: u64,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            auto_start: true,
            idle_timeout: 0,
        }
    }
}

/// API settings for LLM providers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ApiSettings {
    /// Anthropic API key (or use ANTHROPIC_API_KEY env var).
    pub anthropic_key: Option<String>,
    /// Default model to use.
    pub default_model: String,
    /// Rate limit (requests per minute).
    pub rate_limit: u32,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self {
            anthropic_key: None,
            default_model: "claude-sonnet-4-20250514".to_string(),
            rate_limit: 60,
        }
    }
}

/// Sandbox settings for command execution isolation.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SandboxSettings {
    /// Sandbox mode: required, preferred (default), or disabled.
    pub mode: SandboxMode,
    /// Isolate network by default.
    pub isolate_network: bool,
    /// Isolate PID namespace by default.
    pub isolate_pids: bool,
    /// Additional paths to allow read-write access.
    pub extra_rw_paths: Vec<PathBuf>,
    /// Additional paths to allow read-only access.
    pub extra_ro_paths: Vec<PathBuf>,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            mode: SandboxMode::Preferred,
            isolate_network: true,
            isolate_pids: true,
            extra_rw_paths: Vec::new(),
            extra_ro_paths: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.max_concurrent_tasks, 5);
        assert_eq!(config.default_priority, 2);
        assert!(!config.debug);
    }

    #[test]
    fn test_config_paths() {
        let config = Config {
            data_dir: PathBuf::from("/tmp/test"),
            ..Default::default()
        };

        assert_eq!(config.socket_path(), PathBuf::from("/tmp/test/neuraphage.sock"));
        assert_eq!(config.pid_path(), PathBuf::from("/tmp/test/neuraphage.pid"));
    }

    #[test]
    fn test_load_from_file() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join("config.yml");

        let config_content = r#"
data_dir: /custom/path
max_concurrent_tasks: 10
default_priority: 1
debug: true
daemon:
  auto_start: false
  idle_timeout: 300
api:
  default_model: claude-opus-4-20250514
  rate_limit: 30
"#;
        fs::write(&config_path, config_content).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/custom/path"));
        assert_eq!(config.max_concurrent_tasks, 10);
        assert_eq!(config.default_priority, 1);
        assert!(config.debug);
        assert!(!config.daemon.auto_start);
        assert_eq!(config.daemon.idle_timeout, 300);
        assert_eq!(config.api.default_model, "claude-opus-4-20250514");
        assert_eq!(config.api.rate_limit, 30);
    }

    #[test]
    fn test_default_when_no_config() {
        let config = Config::load(None).unwrap();
        assert_eq!(config.max_concurrent_tasks, 5);
    }

    #[test]
    fn test_sandbox_settings_default() {
        let settings = SandboxSettings::default();
        assert_eq!(settings.mode, SandboxMode::Preferred);
        assert!(settings.isolate_network);
        assert!(settings.isolate_pids);
        assert!(settings.extra_rw_paths.is_empty());
        assert!(settings.extra_ro_paths.is_empty());
    }

    #[test]
    fn test_config_with_sandbox() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join("config.yml");

        let config_content = r#"
sandbox:
  mode: required
  isolate_network: false
  isolate_pids: true
  extra_rw_paths:
    - /home/user/.cargo
  extra_ro_paths:
    - /opt/tools
"#;
        fs::write(&config_path, config_content).unwrap();

        let config = Config::load_from_file(&config_path).unwrap();
        assert_eq!(config.sandbox.mode, SandboxMode::Required);
        assert!(!config.sandbox.isolate_network);
        assert!(config.sandbox.isolate_pids);
        assert_eq!(config.sandbox.extra_rw_paths, vec![PathBuf::from("/home/user/.cargo")]);
        assert_eq!(config.sandbox.extra_ro_paths, vec![PathBuf::from("/opt/tools")]);
    }
}
