# Design Document: Neuraphage Completion - Claude Code Replacement

**Author:** Scott Aidler
**Date:** 2026-01-11
**Status:** Draft
**Review Passes:** 1/5

## Summary

This document specifies the remaining work to make neuraphage a functional Claude Code replacement. The core infrastructure exists (daemon, task management, agentic loop structure, tools), but critical integration is missing: no real LLM client, no task execution in the daemon, and no interactive mode.

## Problem Statement

### Background

Neuraphage is designed as a multi-task AI orchestrator with crash-recoverable architecture. It has:
- A daemon process managing tasks via Unix socket
- Task persistence via engram (JSONL + SQLite)
- An `AgenticLoop` struct with tool execution capability
- 7 tools (read_file, write_file, list_directory, run_command, ask_user, complete_task, fail_task)
- Conversation persistence and context management

### Problem

Despite having the building blocks, neuraphage cannot execute AI tasks because:
1. **No LLM integration** - Only a `MockLlmClient` exists; no real Anthropic API calls
2. **Loop never executes** - Daemon manages task metadata but never runs `AgenticLoop`
3. **No interactive mode** - Users cannot watch/interact with running tasks

### Goals

- Implement real Anthropic Claude API client
- Wire task execution into daemon lifecycle
- Provide interactive/attached mode for real-time interaction
- Handle user input flow when `ask_user` tool is called

### Non-Goals

- Multi-provider LLM support (OpenAI, etc.) - Anthropic only for now
- Web UI - CLI/TUI only
- Distributed execution - single daemon only
- Git worktree isolation - use current working directory

## Proposed Solution

### Overview

Four components need implementation:

```
┌─────────────────────────────────────────────────────────────┐
│                         Daemon                               │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Task Manager │  │  Executor    │  │ Connection Pool  │  │
│  │  (exists)    │  │  (NEW)       │  │    (exists)      │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
│         │                 │                   │             │
│         └────────────────┼───────────────────┘             │
│                          │                                  │
│                 ┌────────▼────────┐                        │
│                 │  AgenticLoop    │                        │
│                 │   (exists)      │                        │
│                 └────────┬────────┘                        │
│                          │                                  │
│                 ┌────────▼────────┐                        │
│                 │ AnthropicClient │                        │
│                 │     (NEW)       │                        │
│                 └────────┬────────┘                        │
│                          │                                  │
└──────────────────────────│──────────────────────────────────┘
                           │
                  ┌────────▼────────┐
                  │ Anthropic API   │
                  │ api.anthropic.com│
                  └─────────────────┘
```

### Architecture

#### Component 1: AnthropicClient (NEW)

Implements `LlmClient` trait for real API calls.

**Location:** `src/agentic/anthropic.rs`

```rust
pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(
        &self,
        model: &str,
        messages: &[Message],
        tools: &[Tool],
    ) -> Result<LlmResponse>;
}
```

**Key responsibilities:**
- Build Anthropic Messages API request body
- Convert internal `Message` format to Anthropic format
- Convert internal `Tool` definitions to Anthropic tool schemas
- Parse response, extracting content and tool_use blocks
- Calculate token usage and cost
- Handle rate limiting and retries

#### Component 2: TaskExecutor (NEW)

Manages execution of tasks through the agentic loop.

**Location:** `src/executor.rs`

```rust
pub struct TaskExecutor {
    config: ExecutorConfig,
    llm: Arc<dyn LlmClient>,
    running_tasks: HashMap<TaskId, JoinHandle<ExecutionResult>>,
}

impl TaskExecutor {
    /// Start executing a task
    pub async fn start_task(&mut self, task: Task) -> Result<()>;

    /// Check task status and collect results
    pub async fn poll_tasks(&mut self) -> Vec<(TaskId, ExecutionResult)>;

    /// Provide user input to a waiting task
    pub async fn provide_input(&mut self, task_id: TaskId, input: String) -> Result<()>;

    /// Cancel a running task
    pub async fn cancel_task(&mut self, task_id: TaskId) -> Result<()>;
}
```

**Key responsibilities:**
- Spawn `AgenticLoop` for each task in a tokio task
- Monitor running tasks for completion/failure/user-input-needed
- Route user input to waiting tasks
- Enforce concurrency limits
- Persist conversation state between iterations

#### Component 3: Daemon Integration (MODIFY)

Extend daemon to use TaskExecutor.

**Location:** `src/daemon.rs` (modify existing)

New request types:
```rust
pub enum DaemonRequest {
    // ... existing ...

    /// Start executing a task
    StartTask { id: String },

    /// Provide user input to waiting task
    ProvideInput { id: String, input: String },

    /// Attach to task execution (stream updates)
    AttachTask { id: String },

    /// Get execution status
    ExecutionStatus { id: String },
}
```

New response types:
```rust
pub enum DaemonResponse {
    // ... existing ...

    /// Execution update (for attached clients)
    ExecutionUpdate {
        task_id: String,
        iteration: u32,
        event: ExecutionEvent,
    },

    /// Task waiting for input
    WaitingForInput {
        task_id: String,
        prompt: String,
    },
}
```

#### Component 4: Interactive Mode (NEW)

CLI command for attached task execution.

**Location:** `src/cli.rs` (modify), new `src/interactive.rs`

```rust
// New CLI command
Command::Run {
    /// Task description (creates and starts task)
    description: String,
    /// Working directory
    #[arg(short, long)]
    dir: Option<PathBuf>,
}

Command::Attach {
    /// Task ID to attach to
    id: String,
}
```

**Interactive mode features:**
- Real-time display of LLM responses
- Tool execution output
- User input prompts when needed
- Ctrl+C to pause/cancel
- Status bar showing iteration/tokens/cost

### Data Model

#### ExecutionState (NEW)

```rust
pub struct ExecutionState {
    pub task_id: TaskId,
    pub status: ExecutionStatus,
    pub iteration: u32,
    pub tokens_used: u64,
    pub cost: f64,
    pub conversation_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

pub enum ExecutionStatus {
    Running,
    WaitingForUser { prompt: String },
    Completed { reason: String },
    Failed { error: String },
    Cancelled,
    Paused,
}
```

#### Anthropic API Types

```rust
// Request
#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    tools: Vec<AnthropicTool>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,  // "user" | "assistant"
    content: AnthropicContent,
}

// Response
#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
    stop_reason: String,
    usage: Usage,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: Value },
}
```

### API Design

#### Anthropic Messages API Integration

Endpoint: `POST https://api.anthropic.com/v1/messages`

Headers:
```
x-api-key: <ANTHROPIC_API_KEY>
anthropic-version: 2023-06-01
content-type: application/json
```

Tool definition format:
```json
{
  "name": "read_file",
  "description": "Read contents of a file",
  "input_schema": {
    "type": "object",
    "properties": {
      "path": {
        "type": "string",
        "description": "Path to the file"
      }
    },
    "required": ["path"]
  }
}
```

Tool result format (in user message):
```json
{
  "role": "user",
  "content": [
    {
      "type": "tool_result",
      "tool_use_id": "call_123",
      "content": "file contents here..."
    }
  ]
}
```

### Implementation Plan

**Phase 1: Anthropic Client**
1. Create `src/agentic/anthropic.rs`
2. Implement request/response types
3. Implement `LlmClient` trait
4. Add API key loading from config/env
5. Add basic retry logic
6. Write integration tests with real API

**Phase 2: Task Executor**
1. Create `src/executor.rs`
2. Implement task spawning with `AgenticLoop`
3. Implement task monitoring and result collection
4. Implement user input routing
5. Wire into daemon's run loop
6. Add execution state persistence

**Phase 3: Daemon Integration**
1. Add new request/response types
2. Implement `StartTask`, `ProvideInput`, `AttachTask` handlers
3. Add execution status tracking
4. Implement streaming updates for attached clients

**Phase 4: Interactive Mode**
1. Add `run` and `attach` CLI commands
2. Implement real-time output display
3. Implement user input handling
4. Add status bar with metrics
5. Handle Ctrl+C gracefully

## Alternatives Considered

### Alternative 1: Use anthropic-rs crate

- **Description:** Use existing `anthropic` crate from crates.io
- **Pros:** Less code to write, maintained by others
- **Cons:** May not support latest API features, adds dependency, less control
- **Why not chosen:** The API is simple enough that a custom implementation gives us more control and fewer dependencies

### Alternative 2: Subprocess-based execution

- **Description:** Run each task in a separate process
- **Pros:** Better isolation, crash recovery per-task
- **Cons:** Complex IPC, harder to implement user input flow, more overhead
- **Why not chosen:** Tokio tasks provide sufficient isolation for now; can revisit if stability issues arise

### Alternative 3: Skip daemon, direct execution

- **Description:** Run tasks directly in CLI process, no daemon
- **Pros:** Simpler architecture, no IPC
- **Cons:** No concurrent tasks, no background execution, no crash recovery
- **Why not chosen:** Multi-task orchestration is core to neuraphage's value proposition

## Technical Considerations

### Dependencies

**New crate dependencies:**
```toml
reqwest = { version = "0.12", features = ["json"] }
```

**Existing dependencies used:**
- `tokio` - async runtime (already present)
- `serde` / `serde_json` - serialization (already present)
- `async-trait` - async trait support (already present)

### Performance

- **API latency:** Claude API calls typically 1-30 seconds; use async properly
- **Concurrent tasks:** Default limit 5, configurable
- **Token tracking:** Accumulate per-task, display in status
- **Conversation size:** Persist to disk, load last N messages for context window

### Security

- **API key storage:** Environment variable `ANTHROPIC_API_KEY` or config file
- **Tool execution:** `run_command` executes arbitrary shell; document risk
- **File access:** Tools can read/write any file; runs as user's permissions
- **Future:** Consider sandboxing options (firejail, containers)

### Testing Strategy

1. **Unit tests:** Mock `LlmClient` for `AgenticLoop` tests (exists)
2. **Integration tests:** Real API calls with simple tasks
3. **End-to-end tests:** CLI commands through daemon
4. **Manual testing:** Interactive mode with real tasks

### Rollout Plan

1. Implement and test AnthropicClient in isolation
2. Add executor without daemon integration (direct testing)
3. Integrate with daemon, test via socket
4. Add interactive mode
5. Update documentation and examples
6. Release v0.2.0

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| API rate limiting | Medium | Medium | Implement exponential backoff, respect 429 responses |
| Token cost runaway | Medium | High | Enforce max_tokens and max_cost limits per task |
| Stuck tasks | Medium | Medium | Watcher integration, iteration limits, timeout |
| Conversation too large | Low | Medium | Truncate old messages, summarization (future) |
| Tool execution failures | High | Low | Error handling, retry logic, clear error messages |

## Open Questions

- [x] Use existing anthropic crate or custom implementation? → Custom
- [ ] Should we support streaming responses from Claude? → Defer to future
- [ ] How to handle very long-running tasks (hours)? → Rely on persistence
- [ ] Add cost confirmation before starting expensive tasks? → Consider for v0.3

## References

- [Anthropic Messages API](https://docs.anthropic.com/en/api/messages)
- [Anthropic Tool Use](https://docs.anthropic.com/en/docs/tool-use)
- [Tokio Daemonization Research](./tokio-daemonization-research.md)
- [Neuraphage Design Doc](./neuraphage-design.md)
- [Neuraphage Agentic Loop](./neuraphage-agentic-loop.md)

---

## Review Pass 1: Completeness

Checking all sections...

**Findings:**
- Summary: OK
- Problem Statement: OK
- Proposed Solution: OK - all 4 components described
- Architecture: OK - diagram and component descriptions present
- Data Model: OK - key types defined
- API Design: OK - Anthropic API format documented
- Implementation Plan: OK - phased with estimates
- Alternatives: OK - 3 alternatives considered
- Technical Considerations: OK - deps, perf, security, testing covered
- Risks: OK - 5 risks identified
- Open Questions: OK - 4 questions listed

**Changes made:**
- Added diagram in Architecture section
- Added specific Anthropic API formats for tool use
- Added token/cost tracking to ExecutionState
- Added rollout plan

---

## Review Pass 2: Correctness

Checking for technical accuracy...

**Findings:**
- Anthropic API version header is `2023-06-01` for latest features - CORRECT
- Tool result must be in user message with `tool_use_id` - CORRECT
- `stop_reason` values: `end_turn`, `tool_use`, `max_tokens` - CORRECT
- Model name format `claude-sonnet-4-20250514` - CORRECT
- reqwest version should be 0.12 for latest - CORRECT

**Changes made:**
- Fixed tool result format (must include `type: tool_result`)
- Added `anthropic-version` header requirement
- Clarified that tool results go in user messages, not separate role

---

## Review Pass 3: Edge Cases

Checking error handling and failure modes...

**Findings:**
- API key missing: Should check at startup, fail fast
- Network errors: Need retry with backoff
- Invalid tool arguments: Claude can produce malformed JSON
- Tool execution timeout: Long-running commands
- Conversation persistence failure: What if disk full?
- Task cancellation during API call: Clean shutdown needed

**Changes made:**
- Added retry logic mention to AnthropicClient responsibilities
- Added timeout consideration for run_command tool
- Added graceful cancellation to TaskExecutor interface
- Added disk space consideration to risks

---

## Review Pass 4: Architecture

Checking system fit and scalability...

**Findings:**
- Executor as separate component: Good, allows testing without daemon
- Arc<dyn LlmClient>: Good for dependency injection
- HashMap for running tasks: Fine for 5-10 concurrent, not thousands
- Streaming updates: Current design is polling-based, could add channels

**Changes made:**
- Clarified concurrent task limit rationale
- Added note about future streaming consideration
- Confirmed tokio::spawn is appropriate for task isolation

---

## Review Pass 5: Clarity

Checking implementability...

**Findings:**
- Implementation plan has clear phases
- Code examples are concrete enough to implement
- Data types are fully specified
- API formats have examples
- One ambiguity: How does user input reach waiting task?

**Changes made:**
- Added `ProvideInput` to daemon request types
- Clarified input routing through executor
- Added interactive mode user input handling description

---

**FINAL STATUS:** Document converged after 5 passes. Ready for implementation.
