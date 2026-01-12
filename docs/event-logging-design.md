# Design Document: Simple Event Logging

**Author:** Scott Aidler
**Date:** 2026-01-11
**Status:** Superseded by engram-events-design.md
**Review Passes:** 5/5

## Summary

Add persistent event logging to neuraphage by writing coordination events to a JSONL file. This provides an audit trail for debugging multi-workstream sessions without the complexity of full event sourcing. Events are logged opportunistically during publish, with the existing in-memory EventBus remaining the primary coordination mechanism.

## Problem Statement

### Background

Neuraphage coordinates multiple concurrent tasks via an in-memory EventBus using tokio broadcast channels. Events like `MainUpdated`, `RebaseRequired`, `SyncRelayed`, and `TaskCompleted` flow between components but are ephemeral - lost when the daemon stops or crashes.

The existing cost tracking system (`cost_ledger.jsonl`) demonstrates a working JSONL persistence pattern in the codebase.

### Problem

When debugging multi-workstream sessions, there's no record of:
1. What coordination events occurred between tasks
2. The sequence of events leading to a failure
3. Which tasks received which notifications
4. Historical patterns across sessions

### Goals

1. **Audit trail** - Record coordination events to a persistent log
2. **Debugging support** - Enable "what happened?" analysis after the fact
3. **Minimal overhead** - Don't slow down the hot path
4. **Simple implementation** - Follow existing JSONL patterns

### Non-Goals

1. **Full event sourcing** - State is NOT derived from the log; engram remains source of truth
2. **Replay capability** - Log is for reading, not for reconstructing state
3. **Real-time streaming** - UI events (TextDelta, ActivityChanged) are not logged
4. **Distributed logging** - Single-node, single-file approach
5. **Log rotation/compaction** - Can be added later if needed

## Proposed Solution

### Overview

Extend `EventBus` with an optional `EventLog` that persists events to a JSONL file. Only "durable" events (coordination-related) are logged; high-volume UI events are skipped.

### Architecture

```
┌─────────────────────────────────────────────────────┐
│                    EventBus                         │
│  ┌─────────────┐         ┌──────────────────────┐  │
│  │ broadcast   │         │ EventLog (optional)  │  │
│  │ channel     │         │                      │  │
│  │ (in-memory) │         │ event_log.jsonl      │  │
│  └──────┬──────┘         └──────────┬───────────┘  │
│         │                           │              │
│         │ publish()                 │ append()     │
│         ▼                           ▼              │
│    All subscribers             Durable events      │
│    (fast, ephemeral)           (persisted)         │
└─────────────────────────────────────────────────────┘
```

### Data Model

The existing `Event` struct is already serializable:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub kind: EventKind,
    pub source_task: Option<TaskId>,
    pub target_task: Option<TaskId>,
    pub payload: serde_json::Value,
    pub timestamp: DateTime<Utc>,
}
```

**Durable EventKinds** (logged):
- `TaskStarted`, `TaskCompleted`, `TaskFailed`, `TaskCancelled`
- `MainUpdated`, `RebaseRequired`, `RebaseCompleted`, `RebaseConflict`
- `NudgeSent`, `SyncRelayed`, `TaskPaused`
- `SupervisionDegraded`, `SupervisionRecovered`
- `LearningExtracted`
- `LockAcquired`, `LockReleased`

**Ephemeral EventKinds** (not logged):
- `TaskWaitingForUser` - transient state
- `FileModified` - high volume, can reconstruct from git
- `RateLimitReached` - operational, not coordination
- `Custom(String)` - depends on use case (could make configurable)

**Example Log Output** (`~/.local/share/neuraphage/event_log.jsonl`):
```json
{"id":"019432a1-...","kind":"TaskStarted","source_task":"eg-abc123","target_task":null,"payload":{},"timestamp":"2026-01-11T15:30:00Z"}
{"id":"019432a2-...","kind":"MainUpdated","source_task":null,"target_task":null,"payload":{"commit":"abc123","branch":"main"},"timestamp":"2026-01-11T15:31:00Z"}
{"id":"019432a3-...","kind":"RebaseRequired","source_task":null,"target_task":"eg-abc123","payload":{"commits_behind":2},"timestamp":"2026-01-11T15:31:01Z"}
{"id":"019432a4-...","kind":"RebaseCompleted","source_task":"eg-abc123","target_task":null,"payload":{"previous":"def456","new":"ghi789"},"timestamp":"2026-01-11T15:31:15Z"}
{"id":"019432a5-...","kind":"TaskCompleted","source_task":"eg-abc123","target_task":null,"payload":{"summary":"Implemented feature X"},"timestamp":"2026-01-11T15:45:00Z"}
```

### API Design

```rust
/// Configuration for event logging.
#[derive(Debug, Clone)]
pub struct EventLogConfig {
    /// Path to the event log file.
    pub path: PathBuf,
    /// Whether logging is enabled.
    pub enabled: bool,
}

impl EventLogConfig {
    /// Create config with path relative to data directory.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("event_log.jsonl"),
            enabled: true,
        }
    }
}

/// Append-only event log.
///
/// Uses synchronous file I/O intentionally - append writes are fast (<1ms)
/// and we want durability guarantees. The mutex ensures thread-safety
/// without async complexity.
pub struct EventLog {
    config: EventLogConfig,
    file: std::sync::Mutex<std::fs::File>,  // Sync mutex, not tokio
}

impl EventLog {
    /// Create or open an event log.
    /// Opens file with append mode; creates if doesn't exist.
    pub fn open(config: EventLogConfig) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config.path)?;
        Ok(Self {
            config,
            file: std::sync::Mutex::new(file),
        })
    }

    /// Append an event to the log.
    /// Serializes to JSON, writes line, relies on OS buffering.
    /// Truncates payloads larger than 10KB to prevent log bloat.
    pub fn append(&self, event: &Event) -> Result<()>;

    /// Flush any buffered writes to disk.
    /// Called on daemon shutdown for durability.
    pub fn flush(&self) -> Result<()>;

    /// Read all events from the log (opens file separately for reading).
    pub fn read_all(&self) -> Result<Vec<Event>>;

    /// Read events matching a filter.
    /// Matches if event's source_task OR target_task equals task_id.
    pub fn query(
        &self,
        kinds: Option<&[EventKind]>,
        task_id: Option<&TaskId>,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<Event>>;
}

impl EventKind {
    /// Returns true if this event kind should be persisted.
    pub fn is_durable(&self) -> bool;
}
```

**Modified EventBus:**

```rust
pub struct EventBus {
    sender: Sender<Event>,
    history: Arc<Mutex<Vec<Event>>>,
    max_history: usize,
    counts: Arc<Mutex<HashMap<EventKind, usize>>>,
    // NEW: Optional event log
    event_log: Option<EventLog>,
}

impl EventBus {
    /// Create an event bus with logging.
    pub fn with_logging(log_config: EventLogConfig) -> Result<Self>;

    /// Publish an event (logs durable events).
    pub async fn publish(&self, event: Event) {
        // Log durable events first (fire-and-forget)
        if let Some(log) = &self.event_log {
            if event.kind.is_durable() {
                if let Err(e) = log.append(&event) {
                    log::warn!("Failed to log event: {}", e);
                }
            }
        }

        // Existing logic: history, counts, broadcast
        // ...
    }
}
```

### Implementation Plan

**Phase 1: EventLog struct**
- Create `EventLog` with `open()`, `append()`, `read_all()`
- Add `is_durable()` to `EventKind`
- Unit tests for serialization roundtrip

**Phase 2: EventBus integration**
- Add `event_log: Option<EventLog>` field
- Add `with_logging()` constructor
- Modify `publish()` to log durable events
- Integration tests

**Phase 3: Daemon wiring**
- Add `EventLogConfig` to `DaemonConfig`
- Wire up in `Daemon::new()`
- Add config file support

**Phase 4: CLI query command**
- Add `np events` command to query the log
- Filter by task, kind, time range
- Human-readable output

**Example CLI Usage:**
```bash
# Show all events
np events

# Show events for a specific task
np events --task eg-abc123

# Show only rebase-related events
np events --kind RebaseRequired --kind RebaseCompleted

# Show events from the last hour
np events --since 1h

# Combine filters
np events --task eg-abc123 --since 1h
```

**Example Output:**
```
2026-01-11 15:30:00  TaskStarted      eg-abc123  →
2026-01-11 15:31:00  MainUpdated                 → commit abc123 on main
2026-01-11 15:31:01  RebaseRequired              → eg-abc123 (2 commits behind)
2026-01-11 15:31:15  RebaseCompleted  eg-abc123  → def456..ghi789
2026-01-11 15:45:00  TaskCompleted    eg-abc123  → Implemented feature X
```

## Alternatives Considered

### Alternative 1: Async file writes with channel

- **Description:** Use an mpsc channel to queue events, background task writes to file
- **Pros:** Zero latency on publish path, batched writes
- **Cons:** More complex, events could be lost if daemon crashes before flush
- **Why not chosen:** Simplicity wins; sync writes are fast enough for our event rate

### Alternative 2: SQLite instead of JSONL

- **Description:** Store events in SQLite with proper indexing
- **Pros:** Efficient queries, proper indexing, ACID guarantees
- **Cons:** More complex, different pattern from cost_ledger, overkill for append-only log
- **Why not chosen:** JSONL is simpler, matches existing patterns, good enough for audit trail

### Alternative 3: Extend engram with events

- **Description:** Add event storage to engram alongside items and edges
- **Pros:** Single source of truth, git-backed
- **Cons:** Couples coordination to task storage, engram changes needed
- **Why not chosen:** Events are separate concern; don't want to modify engram

### Alternative 4: Full event sourcing (beads)

- **Description:** Make events the source of truth, derive state from replay
- **Pros:** Full replay, consistency guarantees, Yegge-style architecture
- **Cons:** Major architectural change, complex, overkill for current scale
- **Why not chosen:** We want audit trail, not event sourcing; can evolve later if needed

## Technical Considerations

### Module Structure

`EventLog` will live in `src/coordination/events.rs` alongside `EventBus` since they're tightly coupled. This keeps the coordination module self-contained.

```
src/coordination/
├── mod.rs
├── events.rs      ← EventBus + EventLog (new)
├── knowledge.rs
├── locks.rs
└── scheduler.rs
```

### Dependencies

- **Internal:** EventBus, DaemonConfig
- **External:** serde_json (already used), std::fs

### Future Evolution Path

This design is compatible with evolution toward full event sourcing (beads):

1. **Current:** Audit log only, state in engram
2. **Next:** Add replay capability for debugging (read log, reconstruct timeline)
3. **Future:** Full beads - events become source of truth

The JSONL format and Event struct are the same in all stages; only the consumer changes.

### Performance

- **Write path:** Single mutex-protected file append per durable event
- **Expected rate:** ~1-10 durable events per second during active execution
- **Latency impact:** <1ms per event (file append to OS buffer)
- **Disk I/O:** Relies on OS buffering; explicit flush on daemon shutdown

### Security

- Event log contains task IDs and payloads
- Stored in user's data directory with standard permissions
- No secrets should be in event payloads (tool results are not logged)

### Testing Strategy

1. **Unit tests:** EventLog serialization, is_durable() classification
2. **Integration tests:** EventBus with logging enabled
3. **Manual testing:** Run multi-task session, verify log contents

### Rollout Plan

1. Implement behind `enabled: bool` config flag
2. Default to enabled in next release
3. Add `np events` CLI command for querying

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| File grows unbounded | Medium | Low | Document; add rotation later if needed |
| Slow file I/O impacts publish | Low | Medium | Async fallback if latency measured |
| Log corruption | Low | Low | Skip corrupted lines on read (like cost_ledger) |
| Disk full | Low | Medium | Graceful degradation; warn and continue |
| Concurrent daemon instances | Low | Low | File append is atomic on POSIX; logs may interleave but won't corrupt |
| Serialization failure | Very Low | Low | Log warning, skip event, continue |
| Log file deleted at runtime | Low | Low | Next append will fail; log warning, disable logging |
| Very large payload | Low | Low | Truncate payloads >10KB before logging |

## Open Questions

- [x] Which event kinds are "durable"? → Listed above
- [ ] Should `Custom(String)` events be logged? → Default no, could make configurable
- [ ] File rotation strategy? → Defer; manual deletion acceptable for now
- [ ] Session markers? → Could add `SessionStarted`/`SessionEnded` events

## References

- `src/coordination/events.rs` - Current EventBus implementation
- `src/cost.rs` - JSONL persistence pattern (cost_ledger.jsonl)
- `src/recovery.rs` - PersistedExecutionState pattern
- `docs/proactive-rebase-design.md` - Related coordination design
