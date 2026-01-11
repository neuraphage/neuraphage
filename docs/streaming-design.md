# Design Document: LLM Response Streaming

**Author:** Claude
**Date:** 2026-01-11
**Status:** Ready for Review
**Review Passes:** 5/5

## Summary

Add streaming support to neuraphage so users see LLM output as it's generated, rather than waiting for the complete response. This eliminates the frustrating 10+ second wait while staring at a spinner.

## Problem Statement

### Background

Currently, neuraphage uses a synchronous request/response pattern with the Anthropic API. When a task runs, the CLI shows iteration counts and costs, but users don't see any LLM output until the entire response completes and tools execute.

### Problem

Users experience a poor UX waiting 10-30 seconds with no visible progress while the LLM generates its response. This makes the tool feel unresponsive and creates uncertainty about whether it's working.

### Goals

- Stream LLM text output to users as tokens are generated
- Maintain backward compatibility with existing non-streaming code paths
- Keep latency low (display tokens within milliseconds of receipt)
- Handle tool use blocks correctly (they arrive in chunks too)

### Non-Goals

- Streaming tool execution output (tools already provide output)
- Real-time cost updates during streaming (update at end)
- Streaming to multiple clients simultaneously (single attach)
- WebSocket support (keep Unix socket architecture)

## Proposed Solution

### Overview

Add a `stream()` method to `LlmClient` that uses Anthropic's SSE streaming API. The streaming chunks flow through the existing event channel infrastructure to reach attached clients.

### Architecture

```
┌─────────────────┐     SSE      ┌──────────────┐    mpsc     ┌──────────┐
│ Anthropic API   │ ──────────▶  │ AnthropicClient│ ────────▶ │ AgenticLoop│
└─────────────────┘              └──────────────┘            └──────────┘
                                                                   │
                                                              mpsc │ ExecutionEvent::TextDelta
                                                                   ▼
┌─────────────────┐   Unix sock  ┌──────────────┐    mpsc    ┌──────────┐
│ CLI/REPL        │ ◀──────────  │ Daemon       │ ◀───────── │ Executor │
└─────────────────┘              └──────────────┘            └──────────┘
```

### Data Model

New streaming types in `src/agentic/llm.rs`:

```rust
/// A streaming chunk from the LLM.
pub enum StreamChunk {
    /// Text content delta.
    TextDelta(String),
    /// Tool use started (id and name known).
    ToolUseStart { id: String, name: String },
    /// Tool use input JSON fragment.
    ToolUseDelta { id: String, json_delta: String },
    /// Tool use complete (full input available).
    ToolUseEnd { id: String },
    /// Message complete - final usage stats.
    MessageDone {
        stop_reason: String,
        input_tokens: u64,
        output_tokens: u64,
    },
    /// Error during streaming.
    Error(String),
}
```

New event type in `src/executor.rs`:

```rust
pub enum ExecutionEvent {
    // ... existing variants ...
    /// Text chunk from LLM response (streaming).
    TextDelta { content: String },
}
```

### API Design

Extended `LlmClient` trait:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Non-streaming completion (existing).
    async fn complete(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[Tool]
    ) -> Result<LlmResponse>;

    /// Streaming completion.
    async fn stream(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[Tool],
        chunk_tx: mpsc::Sender<StreamChunk>,
    ) -> Result<LlmResponse>;
}
```

Anthropic SSE request changes:

```rust
// Add to request body
"stream": true
```

**SSE Event Format (from Anthropic):**

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_...","model":"claude-..."}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}

event: message_stop
data: {"type":"message_stop"}
```

**Tool Use Streaming:**

Tool use blocks stream similarly but with `tool_use` type and `input_json_delta`:

```
event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_...","name":"read_file"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"test.txt\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}
```

**JSON Accumulation Strategy:**

Tool input JSON arrives in fragments. We accumulate into a `String` buffer per tool_use block, then parse complete JSON when `content_block_stop` is received:

```rust
struct StreamingToolUse {
    id: String,
    name: String,
    json_buffer: String,
}

// On content_block_start with type=tool_use:
//   Create new StreamingToolUse with id and name
// On input_json_delta:
//   Append partial_json to json_buffer
// On content_block_stop:
//   Parse json_buffer as serde_json::Value
//   Create ToolCall { id, name, arguments: parsed_value }
```

### Implementation Plan

**Phase 1: LLM Layer (llm.rs, anthropic.rs)**
- Add `StreamChunk` enum to llm.rs
- Add `stream()` method to `LlmClient` trait with default impl that falls back to `complete()`
- Implement SSE parsing in `AnthropicClient::stream()`
- Add `reqwest-eventsource` crate for SSE parsing
- Parse all SSE event types, accumulate tool JSON, build final `LlmResponse`

**Phase 2: Event Layer (executor.rs, daemon.rs)**
- Add `TextDelta { content: String }` variant to `ExecutionEvent`
- Add `TextDelta { content: String }` variant to `ExecutionEventDto`
- Implement `From<ExecutionEvent>` for the new variant
- Update `LlmClientWrapper` to implement `stream()` (delegates to inner)

**Phase 3: Agentic Loop Integration (agentic/mod.rs, executor.rs)**
- Add `event_tx: Option<mpsc::Sender<ExecutionEvent>>` to `AgenticLoop`
- Modify `iterate()` to use streaming when `event_tx` is Some
- Forward `TextDelta` chunks through event channel as they arrive
- Final `LlmResponse` still used for tool processing and conversation history
- Update `execute_task()` to pass event sender to agentic loop

**Phase 4: CLI/REPL Display (repl.rs, main.rs)**
- Handle `ExecutionEventDto::TextDelta` in `wait_for_task()`
- Print text delta immediately with `print!()` (no newline)
- Flush stdout after each delta
- Track whether we're mid-line for proper newline handling before other output

**Phase 5: Testing & Polish**
- Add mock streaming responses to `MockLlmClient`
- Test SSE error handling (disconnect, malformed data)
- Test tool JSON accumulation edge cases
- Verify conversation history includes full response text

## Alternatives Considered

### Alternative 1: Polling with Partial Results

**Description:** Store partial response in executor state, have clients poll for updates.

**Pros:**
- No new trait methods needed
- Simpler daemon protocol

**Cons:**
- High latency (polling interval)
- Inefficient (constant polling even when idle)
- Complex state management

**Why not chosen:** Streaming is the natural fit; polling adds complexity and latency.

### Alternative 2: WebSocket Connection

**Description:** Replace Unix socket with WebSocket for bidirectional streaming.

**Pros:**
- Industry standard for streaming
- Better for future web UI

**Cons:**
- Major architecture change
- Overkill for local CLI
- Adds HTTP server dependency

**Why not chosen:** Unix sockets work fine; can add WebSocket later if needed.

### Alternative 3: Separate Streaming Channel

**Description:** Create dedicated mpsc channel just for text chunks, separate from events.

**Pros:**
- Cleaner separation of concerns
- Could have different buffer sizes

**Cons:**
- More channels to manage
- More complex client handling
- Duplicates existing infrastructure

**Why not chosen:** Reusing event channel is simpler and sufficient.

## Technical Considerations

### Dependencies

- `eventsource-stream` crate (or `reqwest-eventsource`) for SSE parsing
- `futures` for stream combinators
- No changes to existing Unix socket protocol (JSON lines)

### Performance

- Text deltas are small (typically 1-10 tokens, <100 bytes)
- Event channel has 100-message buffer (sufficient)
- Network is the bottleneck, not local processing

### Security

- No new attack surface (same API, same auth)
- SSE parsing must handle malformed data gracefully

### Testing Strategy

- Unit tests with mock SSE responses
- Integration test with real API (manual)
- Test error cases: disconnect mid-stream, malformed SSE

### Rollout Plan

- All changes behind existing code paths initially
- AgenticLoop gets `iterate_streaming()` alongside `iterate()`
- Enable streaming as default once stable
- Keep non-streaming as fallback

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| SSE parsing bugs | Medium | Medium | Use well-tested `reqwest-eventsource` crate |
| Mid-stream disconnects | Medium | Low | Return partial response as error, retry at agentic level |
| Tool JSON fragmentation | Low | Medium | Accumulate in buffer, parse on block_stop only |
| Event channel backpressure | Low | Low | Channel has 100-message buffer, deltas are small |
| Incomplete tool JSON on disconnect | Low | High | Treat as error, do not execute partial tool call |
| Extended thinking blocks | Low | Low | Handle same as text blocks (different content type) |

## Open Questions

- [x] Which SSE parsing crate to use? → `reqwest-eventsource` (maintained, works with reqwest)
- [x] Should non-streaming `complete()` be removed eventually? → Keep both; `complete()` is simpler for tests and fallback
- [x] Do we need a config option to disable streaming? → No, streaming is always better; keep `complete()` for internal simplicity only

## References

- [Anthropic Streaming Messages API](https://docs.anthropic.com/en/api/messages-streaming)
- [reqwest-eventsource crate](https://crates.io/crates/reqwest-eventsource)
- Current implementation: `src/agentic/anthropic.rs`
