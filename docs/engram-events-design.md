# Design Document: Extend Engram with Events

**Author:** Scott Aidler
**Date:** 2026-01-11
**Status:** Ready for Implementation
**Review Passes:** 5/5

## Summary

Extend engram to store coordination events alongside items and edges. Events provide an audit trail for debugging multi-workstream sessions while maintaining engram's existing patterns (JSONL + SQLite cache). This keeps all persistent state in a single system rather than splitting between engram and a separate event log.

## Problem Statement

### Background

Neuraphage coordinates concurrent tasks via an in-memory EventBus. Events like `MainUpdated`, `RebaseRequired`, and `SyncRelayed` flow between components but are ephemeral - lost when the daemon restarts.

Integration tests (`tests/multi_workstream.rs`) validated that EventBus handles:
- 1000+ concurrent events without loss
- Task filtering and history queries
- Coordination event flow

The gap is **persistence for cross-session debugging**.

### Problem

When debugging multi-workstream sessions, there's no record of:
1. What coordination events occurred between tasks
2. The sequence of events leading to a failure
3. Which tasks received which notifications
4. Historical patterns across sessions

### Goals

1. **Extend engram** - Add events as a third type alongside items and edges
2. **Audit trail** - Record coordination events for debugging
3. **Unified system** - Single source of truth for all persistent state
4. **Same patterns** - JSONL + SQLite cache, like items and edges

### Non-Goals

1. **Full event sourcing** - Items/edges remain source of truth; events are a log
2. **Real-time streaming events** - UI events (TextDelta) are not stored
3. **Git-backing for events** - Events are high-volume; may skip git commits
4. **Breaking engram API** - Additive changes only

## Proposed Solution

### Overview

Add `events.jsonl` to engram's `.engram/` directory alongside `items.jsonl` and `edges.jsonl`. Add an `events` table to `engram.db` for indexed queries.

### Architecture

```
.engram/
├── items.jsonl      ← Tasks (existing)
├── edges.jsonl      ← Relationships (existing)
├── events.jsonl     ← Coordination events (NEW)
└── engram.db        ← SQLite cache (add events table)
```

### Data Model

New `Event` type in engram:

```rust
/// A coordination event in the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique event ID (eg-evt-XXXXXXXXXX).
    pub id: String,

    /// Kind of event.
    pub kind: String,

    /// Source task ID (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_task: Option<String>,

    /// Target task ID (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_task: Option<String>,

    /// Event payload (JSON).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub payload: serde_json::Value,

    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
}
```

**Event Kinds** (strings for flexibility):
- `task_started`, `task_completed`, `task_failed`, `task_cancelled`
- `main_updated`, `rebase_required`, `rebase_completed`, `rebase_conflict`
- `nudge_sent`, `sync_relayed`, `task_paused`
- `supervision_degraded`, `supervision_recovered`
- `learning_extracted`
- `lock_acquired`, `lock_released`

### API Design

Extend `engram::Store`:

```rust
impl Store {
    // === Existing API (unchanged) ===
    pub fn create(&mut self, title: &str, ...) -> Result<Item>;
    pub fn get(&self, id: &str) -> Result<Option<Item>>;
    pub fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) -> Result<Edge>;
    // ...

    // === New Event API ===

    /// Record an event.
    pub fn record_event(&mut self, event: Event) -> Result<()>;

    /// Get an event by ID.
    pub fn get_event(&self, id: &str) -> Result<Option<Event>>;

    /// Query events with filters.
    pub fn query_events(&self, filter: EventFilter) -> Result<Vec<Event>>;

    /// Get recent events.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>>;

    /// Get events for a specific task.
    pub fn task_events(&self, task_id: &str, limit: usize) -> Result<Vec<Event>>;
}

/// Filter for querying events.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Filter by event kinds.
    pub kinds: Option<Vec<String>>,
    /// Filter by source task.
    pub source_task: Option<String>,
    /// Filter by target task.
    pub target_task: Option<String>,
    /// Filter by timestamp (events after this time).
    pub since: Option<DateTime<Utc>>,
    /// Maximum number of results.
    pub limit: Option<usize>,
}
```

### SQLite Schema

Add to `engram.db`:

```sql
CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    source_task TEXT,
    target_task TEXT,
    payload TEXT,  -- JSON
    timestamp TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind);
CREATE INDEX IF NOT EXISTS idx_events_source ON events(source_task);
CREATE INDEX IF NOT EXISTS idx_events_target ON events(target_task);
CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
```

### Storage Implementation

Follow existing pattern from `storage.rs`:

```rust
impl Storage {
    /// Append an event to events.jsonl and update SQLite cache.
    pub fn append_event(&mut self, event: &Event) -> Result<()> {
        // 1. Serialize to JSON
        let json = serde_json::to_string(event)?;

        // 2. Append to events.jsonl
        let events_path = self.root.join(ENGRAM_DIR).join("events.jsonl");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&events_path)?;
        writeln!(file, "{}", json)?;

        // 3. Insert into SQLite cache
        self.db.execute(
            "INSERT INTO events (id, kind, source_task, target_task, payload, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.kind,
                event.source_task,
                event.target_task,
                event.payload.to_string(),
                event.timestamp.to_rfc3339(),
            ],
        )?;

        Ok(())
    }
}
```

### Integration with Neuraphage

In neuraphage, bridge EventBus to engram:

```rust
impl EventBus {
    /// Create an event bus that persists to engram.
    pub fn with_engram(store: Arc<Mutex<engram::Store>>) -> Self {
        Self {
            // ... existing fields ...
            engram_store: Some(store),
        }
    }

    pub async fn publish(&self, event: Event) {
        // Persist durable events to engram
        if let Some(store) = &self.engram_store {
            if event.kind.is_durable() {
                let engram_event = to_engram_event(&event);
                if let Ok(mut s) = store.lock().await {
                    let _ = s.record_event(engram_event);
                }
            }
        }

        // Existing: history, counts, broadcast
        // ...
    }
}
```

### Implementation Plan

**Phase 1: Engram Core (in engram crate)**
- Add `Event` type to `types.rs`
- Add `EventFilter` for queries
- Add `events` table to SQLite schema
- Add `events.jsonl` handling to `storage.rs`
- Add event methods to `Store`
- Unit tests

**Phase 2: Engram Query Extensions**
- Add `StoreEventExt` trait for advanced queries
- Event timeline queries
- Aggregate queries (counts by kind)

**Phase 3: Neuraphage Integration**
- Bridge EventBus to engram Store
- Add `is_durable()` to EventKind
- Wire up in daemon initialization

**Phase 4: CLI**
- Add `eg events` command to engram CLI
- Add `np events` command to neuraphage CLI
- Filter by task, kind, time range

## Alternatives Considered

### Alternative 1: Separate event_log.jsonl (Previous Design)

- **Description:** Store events in `~/.local/share/neuraphage/event_log.jsonl` outside engram
- **Pros:** No engram changes, can start immediately
- **Cons:** Two systems, split source of truth, different patterns
- **Why not chosen:** User prefers unified system in engram

### Alternative 2: Full Event Sourcing

- **Description:** Make events the source of truth, derive items/edges from replay
- **Pros:** Full replay capability, time travel
- **Cons:** Major refactor, complex, overkill for current needs
- **Why not chosen:** Audit log is sufficient; can evolve later if needed

### Alternative 3: Store Events in Separate SQLite DB

- **Description:** Events in `events.db`, items/edges in `engram.db`
- **Pros:** Isolates high-volume events from task data
- **Cons:** Two databases, more complex, loses JSONL benefits
- **Why not chosen:** Consistency with existing engram patterns preferred

## Technical Considerations

### Module Structure

Changes to engram crate:

```
engram/src/
├── types.rs          ← Add Event, EventFilter
├── storage.rs        ← Add events.jsonl handling
├── store.rs          ← Add event methods
└── query.rs          ← Add StoreEventExt trait
```

### Dependencies

- **Engram internal:** types, storage, store modules
- **External:** serde_json (already used), chrono (already used)

### Performance

- **Write path:** Append to JSONL + INSERT to SQLite (same as items)
- **Expected rate:** ~1-10 durable events per second
- **Query path:** SQLite indexed queries, efficient for typical filters

### Git Integration

Events are higher volume than items/edges. Options:
1. **Include in git** - Full history, but noisy commits
2. **Exclude via .gitignore** - Cleaner history, but no git backup
3. **Configurable** - Let user decide

Recommendation: Start with option 2 (exclude), make configurable later.

### Testing Strategy

1. **Unit tests in engram:** Event CRUD, queries, JSONL roundtrip
2. **Integration tests:** EventBus → engram persistence
3. **Existing multi_workstream tests:** Verify no regression

### Rollout Plan

1. Implement Phase 1-2 in engram crate
2. Release new engram version
3. Implement Phase 3-4 in neuraphage
4. Default to enabled

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Events.jsonl grows large | Medium | Low | Add retention/rotation later |
| SQLite rebuild slow with many events | Low | Medium | Rebuild only indexes, not full |
| Breaking engram API | Low | High | Additive changes only |
| Git noise from events | Medium | Low | Exclude from git by default |

## Open Questions

- [ ] Should events be included in git? (Recommend: no, by default)
- [ ] Retention policy for old events? (Defer: manual deletion for now)
- [ ] Should event IDs use same prefix as items (eg-) or different (eg-evt-)? (Suggest: eg-evt-)

## References

- `docs/event-logging-design.md` - Previous design (separate event log)
- `tests/multi_workstream.rs` - Validates EventBus works in-process
- `engram/src/storage.rs` - Existing JSONL + SQLite pattern
- `engram/src/types.rs` - Existing Item, Edge types
