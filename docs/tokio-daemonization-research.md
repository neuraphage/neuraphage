# Tokio + Daemonization Research

**Date:** 2026-01-10
**Author:** Scott Aidler
**Status:** Implemented

## Summary

This document captures research on safely combining Tokio async runtime with Unix daemon processes. The conclusion: **fork before creating the Tokio runtime**.

## The Problem

Tokio uses global state (thread pools, I/O drivers, timers) that cannot survive a Unix `fork()` system call. Attempting to fork after creating a Tokio runtime leads to:

- Deadlocks from inherited locks
- Unresponsive TCP servers
- Zombie processes stuck on `epoll_wait`

## Authoritative Sources

### 1. Tokio Maintainer Statement

**Carl Lerche** (Tokio maintainer) in [GitHub Issue #1541](https://github.com/tokio-rs/tokio/issues/1541):

> "Threads + fork is bad news... you *can* fork **if** you immediately exec and do not allocate memory or perform any other operation that may have been corrupted by the fork."

**Alec Mocatta** (community member) explained the root cause:

> "Forking when there's more than one thread running is inadvisable in general as it easily deadlocks: thread B takes out a lock (e.g. in the malloc implementation), thread A forks, thread B releases the lock, new process C never has that lock unlocked."

### 2. Rust Users Forum - Official Recommendation

From [Tokio 0.2 and daemonize process](https://users.rust-lang.org/t/tokio-0-2-and-daemonize-process/42427):

> "**You must create the Tokio runtime after daemonizing it. The Tokio runtime can't survive a fork.**"

### 3. Privileged Ports Discussion

From [Tokio & Daemonize w/Privileged Ports](https://users.rust-lang.org/t/tokio-daemonize-w-privileged-ports/81603):

> "Fork _before_ starting the runtime... maintain a single runtime throughout the application lifecycle rather than creating multiple runtime instances."

### 4. The `fork` Crate

The [fork crate](https://crates.io/crates/fork) (v0.6.0, Rust Edition 2024) provides proper daemonization:

- Double-fork pattern (prevents controlling terminal acquisition)
- Uses `_exit` to avoid running destructors post-fork (POSIX-safe)
- Redirects stdio to `/dev/null`
- Changes working directory to `/`

### 5. Building a Daemon Using Rust (Nov 2024)

[Tutorial by Cogs and Levers](https://tuttlem.github.io/2024/11/16/building-a-daemon-using-rust.html):

> "Daemons - long-running background processes - are the backbone of many server applications and system utilities."

The tutorial confirms the double-fork + setsid pattern using the `nix` crate.

### 6. Modern Alternatives

[Armin Ronacher on systemfd](https://lucumr.pocoo.org/2025/1/19/what-is-systemfd/) (Jan 2025):

> "The solution comes from systemd... passing file descriptors from one process to another through environment variables."

Systemd socket activation eliminates the need for manual daemonization on modern Linux.

## Solution Approaches

| Approach | When to Use | Complexity |
|----------|-------------|------------|
| **Fork-then-Tokio** | Standalone daemon, no systemd | Medium |
| **Systemd socket activation** | Modern Linux with systemd | Low |
| **Foreground + supervisor** | Docker, systemd Type=simple | Lowest |

## Implementation

Neuraphage uses the `fork` crate for daemonization:

```rust
use fork::{daemon, Fork};

fn daemonize(config: &DaemonConfig) -> Result<()> {
    match daemon(false, false) {
        Ok(Fork::Child) => {
            // We are now the daemon process (grandchild after double-fork)
            // Safe to create tokio runtime here

            // Write PID file
            let pid = std::process::id();
            // ...

            // Create tokio runtime and run daemon
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async {
                let daemon = Daemon::new(config.clone())?;
                daemon.run().await
            })
        }
        Ok(Fork::Parent(_)) => {
            println!("Daemon started in background");
            std::process::exit(0);
        }
        Err(e) => Err(eyre::eyre!("Failed to daemonize: {:?}", e)),
    }
}
```

### Key Points

1. **Parse CLI args first** - No Tokio yet
2. **Fork/daemonize** - Using `fork` crate's double-fork
3. **Create fresh Tokio runtime** - In the child process only
4. **Run daemon logic** - Now safe to use async

### The `daemon()` Function Parameters

```rust
daemon(nochdir, noclose)
```

- `nochdir = false`: Changes working directory to `/`
- `noclose = false`: Redirects stdin/stdout/stderr to `/dev/null`

## References

- [Is the tokio threadpool fork safe? - GitHub #1541](https://github.com/tokio-rs/tokio/issues/1541)
- [Tokio 0.2 and daemonize process - Rust Forum](https://users.rust-lang.org/t/tokio-0-2-and-daemonize-process/42427)
- [Tokio & Daemonize w/Privileged Ports - Rust Forum](https://users.rust-lang.org/t/tokio-daemonize-w-privileged-ports/81603)
- [fork crate - crates.io](https://crates.io/crates/fork)
- [fork crate documentation - lib.rs](https://lib.rs/crates/fork)
- [Building a Daemon using Rust - Cogs and Levers (Nov 2024)](https://tuttlem.github.io/2024/11/16/building-a-daemon-using-rust.html)
- [What is systemfd? - Armin Ronacher (Jan 2025)](https://lucumr.pocoo.org/2025/1/19/what-is-systemfd/)
- [Tokio Runtime Documentation](https://docs.rs/tokio/latest/tokio/runtime/index.html)
- [systemd_socket crate](https://crates.io/crates/systemd_socket)
- [listenfd crate](https://crates.io/crates/listenfd)

## Conclusion

The "fork before Tokio" pattern is well-established and recommended by Tokio maintainers. The `fork` crate provides a clean, tested implementation of Unix daemonization that integrates safely with async Rust.
