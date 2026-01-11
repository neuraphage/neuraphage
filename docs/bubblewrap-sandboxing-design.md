# Bubblewrap Sandboxing Design

## Overview

This document describes the design for integrating bubblewrap (bwrap) sandboxing into neuraphage's command execution system. The goal is to provide defense-in-depth by isolating command execution at the kernel level, preventing accidental or malicious operations from affecting the host system.

## Problem Statement

The current command execution in `bash.rs` runs commands directly via `sh -c`, relying entirely on pre-execution validation in `safety.rs`. While this provides blocklists and confirmation prompts, it has fundamental limitations:

1. **Bypassable** - Creative command construction can evade pattern matching
2. **Reactive** - Only blocks known-bad patterns, not unknown-bad
3. **No isolation** - Commands have full access to network, filesystem, and processes
4. **Single layer** - If validation fails, there's no fallback protection

## Solution: Bubblewrap Sandboxing

Bubblewrap uses Linux namespaces (user, mount, network, PID, IPC) to create lightweight, unprivileged containers. This provides:

1. **Whitelist model** - Only explicitly allowed paths/capabilities are available
2. **Proactive** - Unknown operations fail by default
3. **Kernel-enforced** - Cannot be bypassed by command tricks
4. **Defense-in-depth** - Works alongside existing safety checks

## Design Phases

### Phase 1: Core Sandbox Module

Create a new `sandbox.rs` module that wraps commands with bwrap.

**Files to create:**
- `src/sandbox.rs` - Core sandboxing logic

**Key types:**

```rust
/// Sandbox configuration
pub struct SandboxConfig {
    /// Enable network isolation (--unshare-net)
    pub isolate_network: bool,
    /// Enable PID namespace isolation (--unshare-pid)
    pub isolate_pids: bool,
    /// Paths to mount read-write
    pub rw_paths: Vec<PathBuf>,
    /// Paths to mount read-only
    pub ro_paths: Vec<PathBuf>,
    /// Paths to block entirely
    pub blocked_paths: Vec<PathBuf>,
    /// Environment variables to pass through
    pub env_passthrough: Vec<String>,
}

/// Result of checking if sandboxing is available
pub enum SandboxAvailability {
    Available,
    NotInstalled,
    NotSupported(String), // e.g., "namespaces disabled in kernel"
}

impl SandboxConfig {
    /// Create default config for a working directory
    pub fn for_working_dir(working_dir: &Path) -> Self;

    /// Build the bwrap command arguments
    pub fn build_args(&self, command: &str) -> Vec<String>;
}

/// Check if bubblewrap is available
pub fn check_availability() -> SandboxAvailability;

/// Wrap a command with sandbox
pub fn wrap_command(config: &SandboxConfig, command: &str) -> Command;
```

**Default mounts:**

| Path | Mode | Rationale |
|------|------|-----------|
| Working directory | read-write | Agent needs to edit files |
| `/usr` | read-only | System binaries |
| `/lib`, `/lib64` | read-only | Shared libraries |
| `/bin`, `/sbin` | read-only | Core utilities |
| `/etc/resolv.conf` | read-only | DNS resolution (when network allowed) |
| `/etc/passwd`, `/etc/group` | read-only | User/group lookups |
| `/tmp` | read-write (tmpfs) | Temporary files |
| `/dev/null`, `/dev/zero`, `/dev/urandom` | devices | Standard devices |

**Blocked paths:**

| Path | Rationale |
|------|-----------|
| `~/.bashrc`, `~/.bash_profile` | Prevent shell injection |
| `~/.ssh` | Protect SSH keys |
| `~/.gnupg` | Protect GPG keys |
| `.git/hooks/*` | Prevent git hook abuse |
| `/etc/shadow` | System passwords |

### Phase 2: Integration with BashTool

Modify `bash.rs` to use the sandbox when available.

**Changes to `src/agentic/tools/bash.rs`:**

```rust
pub async fn execute(ctx: &ToolContext, args: &Value) -> Result<ToolResult> {
    let command = args.get("command")...;

    // Build command - sandboxed if available
    let mut cmd = if ctx.sandbox_enabled {
        let config = SandboxConfig::for_working_dir(&ctx.working_dir);
        sandbox::wrap_command(&config, command)
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };

    cmd.current_dir(&ctx.working_dir);

    // Execute with timeout...
}
```

**Changes to `ToolContext`:**

```rust
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub sandbox_enabled: bool,  // New field
    // ...
}
```

### Phase 3: Configuration

Add sandbox settings to the configuration system.

**Changes to `src/config.rs`:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxSettings {
    /// Enable sandboxing (default: auto-detect)
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Isolate network by default
    #[serde(default = "default_true")]
    pub isolate_network: bool,
    /// Additional paths to allow read-write
    #[serde(default)]
    pub extra_rw_paths: Vec<PathBuf>,
    /// Additional paths to allow read-only
    #[serde(default)]
    pub extra_ro_paths: Vec<PathBuf>,
}

pub struct Config {
    // existing fields...
    pub sandbox: SandboxSettings,
}
```

**Example config:**

```toml
[sandbox]
enabled = true
isolate_network = true
extra_rw_paths = ["/home/user/.cargo"]
extra_ro_paths = ["/opt/tools"]
```

### Phase 4: Network Proxy (Optional)

For tasks that need network access to specific hosts, implement a socat-based proxy.

**Note:** This phase is optional and may be deferred. Initial implementation will use `--share-net` for tasks that need network.

**Design sketch:**

```rust
pub struct NetworkProxy {
    allowed_hosts: Vec<String>,
    proxy_port: u16,
}

impl NetworkProxy {
    /// Start proxy that only allows connections to allowed_hosts
    pub async fn start(&self) -> Result<()>;

    /// Stop the proxy
    pub async fn stop(&self);
}
```

This would allow `--unshare-net` while still permitting outbound connections to specific hosts (e.g., `api.github.com`).

### Phase 5: Testing and Documentation

**Unit tests:**
- `sandbox::check_availability()` returns correct result
- `SandboxConfig::build_args()` produces valid bwrap arguments
- Sandboxed commands can read/write working directory
- Sandboxed commands cannot access blocked paths
- Sandboxed commands cannot access network (when isolated)

**Integration tests:**
- Full agentic loop with sandboxing enabled
- Graceful degradation when bwrap unavailable

**Documentation:**
- Add sandbox section to README
- Document configuration options
- Explain security model and limitations

## Implementation Details

### Bwrap Command Construction

Example bwrap invocation:

```bash
bwrap \
  --unshare-net \
  --unshare-pid \
  --die-with-parent \
  --new-session \
  --ro-bind /usr /usr \
  --ro-bind /lib /lib \
  --ro-bind /lib64 /lib64 \
  --ro-bind /bin /bin \
  --ro-bind /etc/resolv.conf /etc/resolv.conf \
  --bind /home/user/project /home/user/project \
  --tmpfs /tmp \
  --dev /dev \
  --proc /proc \
  --chdir /home/user/project \
  -- sh -c "cargo build"
```

Key flags:
- `--unshare-net`: Network namespace isolation
- `--unshare-pid`: PID namespace (can't see host processes)
- `--die-with-parent`: Terminate if parent dies
- `--new-session`: New session ID (can't send signals to parent)
- `--ro-bind`: Read-only bind mount
- `--bind`: Read-write bind mount
- `--tmpfs`: In-memory filesystem
- `--dev`: Minimal /dev with standard devices
- `--proc`: /proc filesystem (needed by many tools)

### Error Handling

```rust
pub enum SandboxError {
    NotAvailable(String),
    ConfigError(String),
    ExecutionFailed(std::io::Error),
    Timeout,
}
```

When sandboxing fails, the system should:
1. Log the failure reason
2. Fall back to unsandboxed execution if configured
3. Or fail the command if strict mode is enabled

### Graceful Degradation

```rust
pub enum SandboxMode {
    /// Always use sandbox, fail if unavailable
    Required,
    /// Use sandbox if available, fall back if not
    Preferred,
    /// Never use sandbox
    Disabled,
}
```

Default: `Preferred` - use sandboxing when available but don't fail on systems without bwrap.

## Security Considerations

### What This Protects Against

1. **Accidental damage** - `rm -rf /` fails because `/` isn't mounted
2. **Credential theft** - `~/.ssh` and `~/.gnupg` are blocked
3. **Network exfiltration** - Network isolated by default
4. **Git hook attacks** - `.git/hooks` is blocked
5. **Shell injection** - RC files are blocked
6. **Process interference** - PID namespace isolation

### What This Does NOT Protect Against

1. **Malicious working directory operations** - The working directory must be writable
2. **CPU/memory exhaustion** - bwrap doesn't provide cgroups limits
3. **Kernel exploits** - If there's a kernel vuln, sandboxing can be escaped
4. **Symlink attacks within working dir** - Files in working dir can symlink elsewhere

### Defense in Depth

The sandbox complements but does not replace:
- Pre-execution command validation (`safety.rs`)
- Confirmation prompts for dangerous patterns
- Task-level permissions (future work)

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_sandbox_config_default() {
    let config = SandboxConfig::for_working_dir(Path::new("/home/user/project"));
    assert!(config.rw_paths.contains(&PathBuf::from("/home/user/project")));
    assert!(config.ro_paths.contains(&PathBuf::from("/usr")));
}

#[test]
fn test_build_args_includes_unshare_net() {
    let config = SandboxConfig {
        isolate_network: true,
        ..Default::default()
    };
    let args = config.build_args("echo hello");
    assert!(args.contains(&"--unshare-net".to_string()));
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_sandboxed_command_succeeds() {
    let config = SandboxConfig::for_working_dir(&temp_dir);
    let mut cmd = sandbox::wrap_command(&config, "echo hello");
    let output = cmd.output().await.unwrap();
    assert!(output.status.success());
}

#[tokio::test]
async fn test_sandbox_blocks_home_ssh() {
    let config = SandboxConfig::for_working_dir(&temp_dir);
    let mut cmd = sandbox::wrap_command(&config, "cat ~/.ssh/id_rsa");
    let output = cmd.output().await.unwrap();
    assert!(!output.status.success()); // Should fail - path not mounted
}

#[tokio::test]
async fn test_sandbox_network_isolated() {
    let config = SandboxConfig {
        isolate_network: true,
        ..SandboxConfig::for_working_dir(&temp_dir)
    };
    let mut cmd = sandbox::wrap_command(&config, "curl https://example.com");
    let output = cmd.output().await.unwrap();
    assert!(!output.status.success()); // Should fail - no network
}
```

## File Changes Summary

| File | Change |
|------|--------|
| `src/sandbox.rs` | New - core sandboxing logic |
| `src/lib.rs` | Add `pub mod sandbox;` |
| `src/agentic/tools/bash.rs` | Use sandbox when available |
| `src/agentic/tools/mod.rs` | Add `sandbox_enabled` to `ToolContext` |
| `src/config.rs` | Add `SandboxSettings` |
| `src/agentic/loop.rs` | Initialize sandbox config |
| `Cargo.toml` | No new dependencies (uses std::process) |

## Open Questions

1. **Should we support macOS sandbox-exec?** - macOS has a different sandboxing model. Initial implementation will be Linux-only with graceful degradation.

2. **What about Docker/Podman?** - Some users may prefer container isolation. This could be a future alternative backend.

3. **Resource limits?** - bwrap doesn't handle CPU/memory limits. Should we integrate with cgroups or rely on command timeouts?

## Timeline and Dependencies

- **Phase 1** (Core module): No dependencies
- **Phase 2** (BashTool integration): Depends on Phase 1
- **Phase 3** (Configuration): Depends on Phase 2
- **Phase 4** (Network proxy): Optional, can be deferred
- **Phase 5** (Testing): Parallel with implementation

## Success Criteria

1. Commands execute successfully in sandbox with same behavior as unsandboxed
2. Blocked paths are inaccessible from sandboxed commands
3. Network isolation works when enabled
4. Graceful fallback on systems without bwrap
5. No measurable performance impact for typical commands (<10ms overhead)
6. All existing tests pass with sandboxing enabled
