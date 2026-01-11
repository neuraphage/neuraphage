//! Bubblewrap sandbox for command execution.
//!
//! Provides kernel-level isolation using Linux namespaces via bubblewrap (bwrap).
//! This is a defense-in-depth measure that complements pre-execution validation.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// Result of checking if sandboxing is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxAvailability {
    /// Bubblewrap is available and functional.
    Available,
    /// Bubblewrap binary not found.
    NotInstalled,
    /// Bubblewrap exists but doesn't work (e.g., namespaces disabled).
    NotSupported(String),
}

/// Sandbox operating mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    /// Always use sandbox, fail if unavailable.
    Required,
    /// Use sandbox if available, fall back if not (default).
    #[default]
    Preferred,
    /// Never use sandbox.
    Disabled,
}

/// Configuration for the sandbox environment.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Enable network isolation (--unshare-net).
    pub isolate_network: bool,
    /// Enable PID namespace isolation (--unshare-pid).
    pub isolate_pids: bool,
    /// Paths to mount read-write.
    pub rw_paths: Vec<PathBuf>,
    /// Paths to mount read-only.
    pub ro_paths: Vec<PathBuf>,
    /// Environment variables to pass through.
    pub env_passthrough: Vec<String>,
    /// Working directory inside the sandbox.
    pub working_dir: PathBuf,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            isolate_network: true,
            isolate_pids: true,
            rw_paths: Vec::new(),
            ro_paths: Vec::new(),
            env_passthrough: default_env_passthrough(),
            working_dir: PathBuf::from("/tmp"),
        }
    }
}

fn default_env_passthrough() -> Vec<String> {
    vec![
        "PATH".to_string(),
        "HOME".to_string(),
        "USER".to_string(),
        "TERM".to_string(),
        "LANG".to_string(),
        "LC_ALL".to_string(),
        "RUST_BACKTRACE".to_string(),
        "CARGO_HOME".to_string(),
        "RUSTUP_HOME".to_string(),
    ]
}

fn default_ro_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/bin"),
        PathBuf::from("/sbin"),
        PathBuf::from("/etc/resolv.conf"),
        PathBuf::from("/etc/passwd"),
        PathBuf::from("/etc/group"),
        PathBuf::from("/etc/hosts"),
        PathBuf::from("/etc/ssl"),
        PathBuf::from("/etc/ca-certificates"),
    ]
}

impl SandboxConfig {
    /// Create a sandbox configuration for a specific working directory.
    pub fn for_working_dir(working_dir: &Path) -> Self {
        let mut config = Self {
            working_dir: working_dir.to_path_buf(),
            ..Default::default()
        };

        // Add working directory as read-write
        config.rw_paths.push(working_dir.to_path_buf());

        // Add default read-only system paths
        config.ro_paths = default_ro_paths();

        // Add cargo/rustup if they exist
        if let Ok(home) = std::env::var("HOME") {
            let cargo_home = PathBuf::from(&home).join(".cargo");
            let rustup_home = PathBuf::from(&home).join(".rustup");

            if cargo_home.exists() {
                config.ro_paths.push(cargo_home);
            }
            if rustup_home.exists() {
                config.ro_paths.push(rustup_home);
            }
        }

        config
    }

    /// Build the bwrap command arguments.
    pub fn build_args(&self) -> Vec<String> {
        let mut args = Vec::new();

        // Namespace isolation
        if self.isolate_network {
            args.push("--unshare-net".to_string());
        }
        if self.isolate_pids {
            args.push("--unshare-pid".to_string());
        }

        // Safety flags
        args.push("--die-with-parent".to_string());
        args.push("--new-session".to_string());

        // Read-only mounts
        for path in &self.ro_paths {
            if path.exists() {
                args.push("--ro-bind".to_string());
                args.push(path.display().to_string());
                args.push(path.display().to_string());
            }
        }

        // Read-write mounts
        for path in &self.rw_paths {
            if path.exists() {
                args.push("--bind".to_string());
                args.push(path.display().to_string());
                args.push(path.display().to_string());
            }
        }

        // /tmp as tmpfs
        args.push("--tmpfs".to_string());
        args.push("/tmp".to_string());

        // /dev with standard devices
        args.push("--dev".to_string());
        args.push("/dev".to_string());

        // /proc (needed by many tools)
        args.push("--proc".to_string());
        args.push("/proc".to_string());

        // Working directory
        args.push("--chdir".to_string());
        args.push(self.working_dir.display().to_string());

        // Environment variables
        for var in &self.env_passthrough {
            if let Ok(value) = std::env::var(var) {
                args.push("--setenv".to_string());
                args.push(var.clone());
                args.push(value);
            }
        }

        args
    }
}

/// Check if bubblewrap is available on the system.
pub fn check_availability() -> SandboxAvailability {
    // Check if bwrap binary exists
    let which_result = Command::new("which").arg("bwrap").output();

    match which_result {
        Ok(output) if output.status.success() => {
            // Binary exists, test if it actually works
            let test_result = Command::new("bwrap")
                .args(["--ro-bind", "/", "/", "--", "true"])
                .output();

            match test_result {
                Ok(output) if output.status.success() => SandboxAvailability::Available,
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    SandboxAvailability::NotSupported(stderr.trim().to_string())
                }
                Err(e) => SandboxAvailability::NotSupported(e.to_string()),
            }
        }
        _ => SandboxAvailability::NotInstalled,
    }
}

/// Wrap a command with bubblewrap sandbox.
///
/// Returns a `Command` configured to run the given shell command inside the sandbox.
pub fn wrap_command(config: &SandboxConfig, command: &str) -> Command {
    let mut cmd = Command::new("bwrap");

    // Add sandbox configuration
    cmd.args(config.build_args());

    // Separator and the actual command
    cmd.arg("--");
    cmd.arg("sh");
    cmd.arg("-c");
    cmd.arg(command);

    cmd
}

/// Async version of wrap_command for tokio.
pub fn wrap_command_async(config: &SandboxConfig, command: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("bwrap");

    // Add sandbox configuration
    cmd.args(config.build_args());

    // Separator and the actual command
    cmd.arg("--");
    cmd.arg("sh");
    cmd.arg("-c");
    cmd.arg(command);

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_default() {
        let config = SandboxConfig::default();
        assert!(config.isolate_network);
        assert!(config.isolate_pids);
        assert!(config.rw_paths.is_empty());
        assert!(config.ro_paths.is_empty());
    }

    #[test]
    fn test_sandbox_config_for_working_dir() {
        let config = SandboxConfig::for_working_dir(Path::new("/tmp/test"));
        assert!(config.rw_paths.contains(&PathBuf::from("/tmp/test")));
        assert!(config.ro_paths.contains(&PathBuf::from("/usr")));
        assert!(config.ro_paths.contains(&PathBuf::from("/bin")));
    }

    #[test]
    fn test_build_args_includes_isolation_flags() {
        let config = SandboxConfig {
            isolate_network: true,
            isolate_pids: true,
            ..Default::default()
        };
        let args = config.build_args();
        assert!(args.contains(&"--unshare-net".to_string()));
        assert!(args.contains(&"--unshare-pid".to_string()));
        assert!(args.contains(&"--die-with-parent".to_string()));
        assert!(args.contains(&"--new-session".to_string()));
    }

    #[test]
    fn test_build_args_without_network_isolation() {
        let config = SandboxConfig {
            isolate_network: false,
            ..Default::default()
        };
        let args = config.build_args();
        assert!(!args.contains(&"--unshare-net".to_string()));
    }

    #[test]
    fn test_build_args_includes_dev_and_proc() {
        let config = SandboxConfig::default();
        let args = config.build_args();

        // Check that --dev /dev appears
        let dev_idx = args.iter().position(|a| a == "--dev");
        assert!(dev_idx.is_some());
        assert_eq!(args.get(dev_idx.unwrap() + 1), Some(&"/dev".to_string()));

        // Check that --proc /proc appears
        let proc_idx = args.iter().position(|a| a == "--proc");
        assert!(proc_idx.is_some());
        assert_eq!(args.get(proc_idx.unwrap() + 1), Some(&"/proc".to_string()));
    }

    #[test]
    fn test_check_availability() {
        // This test will vary based on system configuration
        let result = check_availability();
        // Just ensure it doesn't panic and returns a valid variant
        match result {
            SandboxAvailability::Available => {}
            SandboxAvailability::NotInstalled => {}
            SandboxAvailability::NotSupported(_) => {}
        }
    }

    #[test]
    fn test_wrap_command_structure() {
        let config = SandboxConfig::for_working_dir(Path::new("/tmp"));
        let cmd = wrap_command(&config, "echo hello");

        // Verify the command is bwrap
        assert_eq!(cmd.get_program(), "bwrap");

        // Get args as a vector for easier checking
        let args: Vec<_> = cmd.get_args().map(|s| s.to_string_lossy()).collect();

        // Should end with -- sh -c "echo hello"
        let len = args.len();
        assert!(len >= 4);
        assert_eq!(args[len - 4], "--");
        assert_eq!(args[len - 3], "sh");
        assert_eq!(args[len - 2], "-c");
        assert_eq!(args[len - 1], "echo hello");
    }

    #[test]
    fn test_sandbox_mode_default() {
        let mode = SandboxMode::default();
        assert_eq!(mode, SandboxMode::Preferred);
    }

    #[test]
    #[ignore] // Requires bwrap to be installed
    fn test_sandboxed_echo() {
        let config = SandboxConfig::for_working_dir(Path::new("/tmp"));
        let output = wrap_command(&config, "echo hello").output().unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "hello");
    }

    #[test]
    #[ignore] // Requires bwrap to be installed
    fn test_network_isolation_blocks_network() {
        let config = SandboxConfig {
            isolate_network: true,
            ..SandboxConfig::for_working_dir(Path::new("/tmp"))
        };
        // Try to make a network request - should fail
        let output = wrap_command(&config, "curl -s --max-time 2 https://example.com")
            .output()
            .unwrap();
        assert!(!output.status.success());
    }

    #[test]
    fn test_sandbox_mode_serde() {
        // Test serialization
        assert_eq!(serde_json::to_string(&SandboxMode::Required).unwrap(), "\"required\"");
        assert_eq!(serde_json::to_string(&SandboxMode::Preferred).unwrap(), "\"preferred\"");
        assert_eq!(serde_json::to_string(&SandboxMode::Disabled).unwrap(), "\"disabled\"");

        // Test deserialization
        assert_eq!(
            serde_json::from_str::<SandboxMode>("\"required\"").unwrap(),
            SandboxMode::Required
        );
        assert_eq!(
            serde_json::from_str::<SandboxMode>("\"preferred\"").unwrap(),
            SandboxMode::Preferred
        );
        assert_eq!(
            serde_json::from_str::<SandboxMode>("\"disabled\"").unwrap(),
            SandboxMode::Disabled
        );
    }

    #[test]
    fn test_sandbox_config_env_passthrough() {
        let config = SandboxConfig::default();
        // Should include common environment variables
        assert!(config.env_passthrough.contains(&"PATH".to_string()));
        assert!(config.env_passthrough.contains(&"HOME".to_string()));
        assert!(config.env_passthrough.contains(&"TERM".to_string()));
    }

    #[test]
    fn test_build_args_includes_chdir() {
        let config = SandboxConfig {
            working_dir: PathBuf::from("/home/user/project"),
            ..Default::default()
        };
        let args = config.build_args();

        // Check that --chdir is included
        let chdir_idx = args.iter().position(|a| a == "--chdir");
        assert!(chdir_idx.is_some());
        assert_eq!(
            args.get(chdir_idx.unwrap() + 1),
            Some(&"/home/user/project".to_string())
        );
    }
}
