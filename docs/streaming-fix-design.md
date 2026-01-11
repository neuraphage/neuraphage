# Design Document: Fix Streaming UX and Architecture

**Author:** Claude
**Date:** 2026-01-11
**Status:** Ready for Review
**Review Passes:** 5/5

## Summary

Fix the broken streaming implementation where users see no real-time output during LLM execution. The current implementation has critical architecture bugs that cause events to be lost and state to never update. This design addresses the root causes and provides a better UX showing "what the AI is doing."

## Problem Statement

### Background
The streaming feature was implemented but is not working in production. Users see:
- `● Iteration 0 | Tokens: 0 | Cost: $0.0000` that never updates
- No streaming text as the LLM generates responses
- Only completion summaries after everything finishes

### Previous Testing Failures
The original implementation included tests that passed but **failed to catch the bugs**:

| Test | What It Tested | What It Missed |
|------|----------------|----------------|
| `test_default_stream_impl` | MockLlmClient sends chunks | Doesn't test real AnthropicClient SSE |
| `test_iterate_with_streaming_forwards_text_deltas` | Events go to channel | Doesn't test daemon poll_events() |
| `test_sse_*` | JSON parsing of SSE events | Doesn't test actual network stream |
| Integration tests | AgenticLoop with mock | Didn't test full daemon→REPL flow |

**Root cause of test failure:** Tests were too isolated. They tested individual components but never tested the actual path:
```
LLM API → SSE Stream → AnthropicClient → StreamChunk → AgenticLoop → ExecutionEvent
  → event_tx channel → RunningTask → poll_events() → Daemon → REPL display
```

Each piece was tested in isolation, but the integration was broken at:
1. `poll_events()` returning stale state when channel empty
2. State never being updated from `IterationComplete` events
3. `AttachTask` handler discarding all but last event

### Problem
**Critical Bug #1: Stale ExecutionState**
- `ExecutionState` is created once at task start with `iteration: 0, tokens_used: 0, cost: 0.0`
- This state is NEVER UPDATED as events come in
- When REPL polls and channel is empty, it shows this stale state

**Critical Bug #2: Lost Events**
- `poll_events()` uses `try_recv()` which drains the channel
- If REPL polls at wrong time, events are missed
- Only returns last event, discards others (`events.into_iter().last()`)

**Critical Bug #3: Poor UX Design**
- No indication of "what the AI is doing" (searching, thinking, calling tools)
- No elapsed time display
- No visibility into tool execution

### Goals
- Fix event propagation so streaming text actually displays
- Update iteration/tokens/cost in real-time
- Show tool execution status ("Searching the web...", "Reading file...")
- Display elapsed time

### Non-Goals
- Changing the underlying LLM streaming protocol
- Real-time audio/video output
- Multi-user streaming support

## Proposed Solution

### Overview
1. **Fix state updates** - Update `ExecutionState` from events
2. **Buffer events** - Keep recent events in memory instead of losing them
3. **Add activity indicators** - Show what the AI is currently doing
4. **Show elapsed time** - Display time since task started

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                         TaskExecutor                            │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ RunningTask                                              │   │
│  │  ├─ state: ExecutionState (MUTABLE, updated from events) │   │
│  │  ├─ event_buffer: VecDeque<ExecutionEvent> (recent N)    │   │
│  │  ├─ current_activity: Option<Activity>                   │   │
│  │  └─ event_rx: mpsc::Receiver (consumed by state updater) │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                  │
│                              ▼                                  │
│                     Background task updates state               │
│                     from events as they arrive                  │
└─────────────────────────────────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Daemon AttachTask Handler                    │
│  1. Return current ExecutionState (always fresh)                │
│  2. Return buffered events (recent N, not lost)                 │
│  3. Return current Activity ("Searching...", "Thinking...")     │
└─────────────────────────────────────────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────┐
│                         REPL Display                            │
│  ⏱ 12s | Iteration 3 | Tokens: 2,456 | Cost: $0.0234           │
│  🔍 Searching the web for "Colorado Avalanche game"...          │
│  or                                                             │
│  💭 Thinking...                                                  │
│  or                                                             │
│  ⌨ [streaming text appears here word by word]                   │
└─────────────────────────────────────────────────────────────────┘
```

### Data Model

**New `Activity` enum:**
```rust
pub enum Activity {
    /// Calling the LLM API
    Thinking,
    /// Streaming text response
    Streaming,
    /// Executing a tool
    ExecutingTool { name: String },
    /// Waiting for external response (web search, etc)
    WaitingForTool { name: String },
    /// Idle between iterations
    Idle,
}
```

**Updated `ExecutionState`:**
```rust
pub struct ExecutionState {
    pub task_id: TaskId,
    pub status: ExecutionStatus,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub current_activity: Activity,  // NEW
}
```

**Updated `RunningTask`:**
```rust
struct RunningTask {
    handle: JoinHandle<ExecutionResult>,
    input_tx: mpsc::Sender<String>,
    event_rx: mpsc::Receiver<ExecutionEvent>,  // Still single consumer
    state: Arc<RwLock<ExecutionState>>,  // CHANGED: Now mutable via Arc<RwLock>
    event_buffer: VecDeque<ExecutionEvent>,  // NEW: Recent events for replay
    last_poll_idx: usize,  // NEW: Track where client last read from buffer
}
```

**Key architectural decisions:**
1. **Single event consumer**: `poll_events()` is the only consumer of `event_rx`
2. **Immediate state update**: When `poll_events()` reads events, it updates `state` synchronously
3. **Event replay buffer**: Recent events stored in `event_buffer` so slow pollers don't miss them
4. **No background tasks**: Simpler, no additional concurrency complexity

**New events:**
```rust
pub enum ExecutionEvent {
    // ... existing ...

    /// Activity changed (for UX updates)
    ActivityChanged { activity: Activity },
    /// Tool execution started
    ToolStarted { name: String },
    /// Tool execution completed
    ToolCompleted { name: String, result: String },
}
```

### API Design

**Updated daemon response:**
```rust
pub enum DaemonResponse {
    // ... existing ...

    /// Execution update with full context
    ExecutionUpdateV2 {
        task_id: String,
        elapsed_secs: u64,
        iteration: u32,
        tokens_used: u64,
        cost: f64,
        activity: ActivityDto,
        events: Vec<ExecutionEventDto>,  // Multiple events, not just one
    },
}
```

### Implementation Plan

**Phase 1: Fix State Updates**
- Change `RunningTask.state` to use `Arc<RwLock<ExecutionState>>` for mutability
- In `poll_events()`, update state from `IterationComplete` events before returning
- No background task needed - update happens during poll

**Phase 2: Buffer Events**
- Add `event_buffer: VecDeque<ExecutionEvent>` to `RunningTask`
- Buffer recent N events (e.g., 100)
- Return all buffered events since last poll, not just last one

**Phase 3: Add Activity Tracking**
- Add `Activity` enum and `current_activity` to state
- Send `ActivityChanged` events from agentic loop
- Send `ToolStarted`/`ToolCompleted` events from tool executor

**Phase 4: Update CLI Display**
- Show elapsed time: `⏱ 12s`
- Show activity: `🔍 Searching...` or `💭 Thinking...`
- Stream text inline as TextDelta events arrive
- Update iteration/tokens/cost on IterationComplete

**Phase 5: Testing**
- Integration test that verifies state updates from events
- Test that buffered events aren't lost
- Test activity transitions
- Manual testing of real UX

## Alternatives Considered

### Alternative 1: Use broadcast channel instead of mpsc
- **Description:** Replace `mpsc::channel` with `tokio::sync::broadcast`
- **Pros:** Multiple receivers can get all events
- **Cons:** More memory usage, lagging receivers lose events, unnecessary complexity
- **Why not chosen:** Event buffering is simpler and more controlled

### Alternative 2: WebSocket for real-time updates
- **Description:** Replace polling with persistent WebSocket connection
- **Pros:** True real-time, no polling overhead
- **Cons:** Major architecture change, breaks Unix socket design
- **Why not chosen:** Too much change; polling with good buffering works fine

### Alternative 3: Event sourcing
- **Description:** Store all events in append-only log, replay for state
- **Pros:** Perfect state reconstruction, audit trail
- **Cons:** Overkill for this use case, storage overhead
- **Why not chosen:** We only need recent state, not full history

## Technical Considerations

### Dependencies
- No new external dependencies
- Uses existing tokio sync primitives

### Performance
- Event buffer limited to N entries (e.g., 100)
- State updates are fast (just field assignments)
- Polling interval stays at 100ms

### Security
- No security implications (internal daemon communication)

### Testing Strategy

**Required tests that MUST pass before declaring done:**

1. **End-to-end daemon test** (CRITICAL)
   - Start daemon, submit task, attach, verify events flow to REPL
   - Use mock LLM that sends TextDelta chunks with delays
   - Assert: iteration/tokens/cost update on each IterationComplete
   - Assert: TextDelta events display inline

2. **State update test**
   - Submit task, let it run one iteration
   - Call `poll_events()` multiple times
   - Assert: `get_state()` returns updated iteration/tokens/cost

3. **Event buffer test**
   - Submit task that produces 50 events rapidly
   - Poll once after all events sent
   - Assert: all 50 events returned (not just last one)

4. **Slow poller test**
   - Submit task, wait 500ms before first poll
   - Assert: buffered events still available, none lost

5. **Real API integration test** (can be `#[ignore]`)
   - Call real Anthropic API with streaming
   - Assert: TextDelta events received during generation
   - Assert: final response matches accumulated text

6. **Manual smoke test checklist**
   - [ ] Run `np` REPL, ask question requiring long response
   - [ ] See text stream word-by-word
   - [ ] See iteration/tokens/cost update
   - [ ] Ask question requiring web search
   - [ ] See "Searching..." activity indicator
   - [ ] See tool result after search completes

### Rollout Plan
1. Fix bugs in dev
2. Test with real usage
3. Deploy

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| State race conditions | Medium | High | Use RwLock, single writer (event consumer) |
| Event buffer overflow | Low | Medium | Bounded buffer (100), drop oldest events |
| Performance regression | Low | Low | Profile hot paths, benchmark |
| Display flicker | Medium | Low | Rate-limit UI updates to 10Hz max |
| SSE disconnect mid-stream | Medium | Medium | Detect in anthropic.rs, send Error event |
| Slow REPL polling misses events | High | Medium | Buffer solves this - events retained |
| Task completes before REPL attaches | Medium | Low | Return completion state from buffered events |
| Channel full (backpressure) | Low | Low | Use unbounded buffer in RunningTask, bounded in LLM stream |

## Open Questions

- [x] What causes the "Iteration 0" to never update? **ANSWERED: State never updated from events**
- [x] Why aren't TextDelta events showing? **ANSWERED: Events lost due to `try_recv()` timing + state stale**
- [x] Should we show tool output inline or in a separate section? **DECIDED: Inline with indicator, e.g. `→ search_web: searching...`**
- [x] What's the right buffer size for events? **DECIDED: 100 events - enough for ~10 iterations with tool calls**

## References

- Original streaming design: `docs/streaming-design.md`
- Claude Code UX for comparison
- tokio sync primitives documentation
