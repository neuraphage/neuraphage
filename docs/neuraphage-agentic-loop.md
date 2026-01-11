# Neuraphage: Agentic Loop Design

> Part of the [Neuraphage Design Documentation](./neuraphage-design.md)
>
> Related: [Personas](./neuraphage-personas.md) | [Infrastructure](./neuraphage-infrastructure.md)

---

## Table of Contents

1. [Layer Integration: Orchestration ↔ Loop](#layer-integration-orchestration--loop)
2. [The Agentic Loop (Production-Hardened)](#the-agentic-loop-production-hardened)
3. [Task States](#task-states-complete)
4. [Recovery Matrix](#recovery-matrix)
5. [Drift Detection](#drift-detection)
6. [Persistence & Recovery](#persistence--recovery)

---

## Layer Integration: Orchestration ↔ Loop

Neuraphage has two conceptual layers that must work together:

- **Orchestration Layer** — What happens *between* and *around* tasks (Task Manager, Scheduler, Lock Manager, Event Bus, Knowledge Store)
- **Loop Layer** — What happens *inside* each task (LLM calls, tool execution, checkpoints)

### How They Connect

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         ORCHESTRATION LAYER                                  │
│                                                                              │
│   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐   ┌─────────────┐    │
│   │ Task        │   │ Scheduler   │   │ Lock        │   │ Event       │    │
│   │ Manager     │   │             │   │ Manager     │   │ Bus         │    │
│   │             │   │ - priority  │   │             │   │             │    │
│   │ - create    │   │ - rate lim  │   │ - files     │   │ - publish   │    │
│   │ - list      │   │ - max conc  │   │ - git       │   │ - subscribe │    │
│   │ - kill      │   │ - model     │   │ - APIs      │   │ - persist   │    │
│   │ - resume    │   │   tiering   │   │ - deadlock  │   │             │    │
│   └──────┬──────┘   └──────┬──────┘   └──────┬──────┘   └──────┬──────┘    │
│          │                 │                 │                 │            │
│   ┌──────┴─────────────────┴─────────────────┴─────────────────┴──────┐    │
│   │                     SHARED SERVICE BUS                             │    │
│   │                                                                    │    │
│   │  Provides to loops:                                                │    │
│   │    • llm_client: Arc<LlmClient>                                    │    │
│   │    • lock_manager: Arc<LockManager>                                │    │
│   │    • event_bus: Arc<EventBus>                                      │    │
│   │    • knowledge_store: Arc<KnowledgeStore>                          │    │
│   │    • persistence: Arc<Persistence>                                 │    │
│   │    • security_policy: Arc<SecurityPolicy>                          │    │
│   └────────────────────────────┬───────────────────────────────────────┘    │
│                                │                                            │
│                                │ Arc<SharedServices>                        │
│                                │                                            │
├────────────────────────────────┼────────────────────────────────────────────┤
│                                │                                            │
│                                ▼                                            │
│   ┌────────────────────────────────────────────────────────────────────┐   │
│   │                         LOOP LAYER                                  │   │
│   │                                                                     │   │
│   │   ┌─────────────┐     ┌─────────────┐     ┌─────────────┐         │   │
│   │   │   Task 1    │     │   Task 2    │     │   Task N    │         │   │
│   │   │   Loop      │     │   Loop      │     │   Loop      │         │   │
│   │   │             │     │             │     │             │         │   │
│   │   │  Uses:      │     │  Uses:      │     │  Uses:      │         │   │
│   │   │  • llm      │     │  • llm      │     │  • llm      │         │   │
│   │   │  • locks    │     │  • locks    │     │  • locks    │         │   │
│   │   │  • events   │     │  • events   │     │  • events   │         │   │
│   │   │  • knowledge│     │  • knowledge│     │  • knowledge│         │   │
│   │   └─────────────┘     └─────────────┘     └─────────────┘         │   │
│   │                                                                     │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Touch Points: Loop → Orchestration

| Loop Action | Orchestration Service | What Happens |
|-------------|----------------------|--------------|
| **Context Assembly** | `knowledge_store.query_relevant(context)` | Inject learnings from other tasks |
| **LLM Request** | `llm_client.complete(request)` | Queued, rate-limited, routed to correct model tier |
| **Tool: needs lock** | `lock_manager.acquire(resources, task_id)` | Sorted acquisition, may block task |
| **Tool: blocked** | `event_bus.publish(TaskBlocked)` | Scheduler notified, TUI updated |
| **Needs user input** | `event_bus.publish(UserInputNeeded)` | TUI shows notification, terminal title updates |
| **Checkpoint** | `persistence.checkpoint(task, state)` | SQLite + git commit |
| **Learning found** | `knowledge_store.queue_learning(text)` | Queued for extraction, available to other tasks |
| **Task complete** | `event_bus.publish(TaskCompleted)` | Parent notified (if subtask), TUI updated |

### Touch Points: Orchestration → Loop

| Orchestration Action | Loop Effect | What Happens |
|---------------------|-------------|--------------|
| **User provides input** | `task.resume(input)` | Loop wakes, continues from yield point |
| **User attaches** | `task.set_attached(stream_tx)` | Loop forwards streaming output to TUI |
| **User detaches** | `task.set_detached()` | Loop buffers output, continues running |
| **Lock available** | `task.unblock()` | Blocked task resumes, acquires lock |
| **Kill signal** | `task.cancel()` | Loop exits cleanly, runs cleanup |
| **Pause signal** | `task.pause()` | Loop yields at next safe point |

### Data Flow Example: Task Needs User Permission

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Task Loop (Task A)                                                        │
│                                                                          │
│   Tool execution: bash "rm -rf ./build"                                  │
│       │                                                                  │
│       ▼                                                                  │
│   security_policy.check() → Not in allowlist                             │
│       │                                                                  │
│       ▼                                                                  │
│   task.status = WaitingForUser { prompt: "Allow rm -rf ./build?" }       │
│       │                                                                  │
│       ▼                                                                  │
│   event_bus.publish(UserInputNeeded { task_id: A, prompt: "..." })       │
│       │                                                                  │
│       ▼                                                                  │
│   yield ─────────────────────────────────────────────────────────────────┼──┐
│                                                                          │  │
└──────────────────────────────────────────────────────────────────────────┘  │
                                                                              │
┌──────────────────────────────────────────────────────────────────────────┐  │
│ Event Bus                                                                 │  │
│                                                                          │  │
│   UserInputNeeded { task_id: A, ... }                                    │◄─┘
│       │                                                                  │
│       ├──► TUI subscriber: show notification, highlight Task A           │
│       │                                                                  │
│       └──► Terminal title subscriber: update to "⚠ Task A needs input"  │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│ TUI                                                                       │
│                                                                          │
│   User sees: "Task A wants to run: rm -rf ./build — Allow? [y/N/always]" │
│       │                                                                  │
│       ▼                                                                  │
│   User presses 'y'                                                       │
│       │                                                                  │
│       ▼                                                                  │
│   task_manager.provide_input(task_id: A, input: "yes")                   │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│ Task Manager                                                              │
│                                                                          │
│   provide_input(A, "yes")                                                │
│       │                                                                  │
│       ▼                                                                  │
│   task_a.resume(UserDecision::Allow)                                     │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────────────┐
│ Task Loop (Task A) — resumed                                              │
│                                                                          │
│   ◄── wakes from yield                                                   │
│       │                                                                  │
│       ▼                                                                  │
│   user_decision == Allow → proceed with tool execution                   │
│       │                                                                  │
│       ▼                                                                  │
│   lock_manager.acquire([File("./build")])                                │
│       │                                                                  │
│       ▼                                                                  │
│   execute: rm -rf ./build                                                │
│       │                                                                  │
│       ▼                                                                  │
│   continue loop...                                                       │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

### Data Flow Example: Cross-Task Knowledge Sharing

```
┌────────────────────────────────────────────────────────────────┐
│ Task A completes                                                │
│                                                                │
│   Conversation includes: "The bug was in config.rs line 42.    │
│   The parser expected an array but got an object."             │
│       │                                                        │
│       ▼                                                        │
│   Learning extraction (keyword: "bug", "expected", "got")      │
│       │                                                        │
│       ▼                                                        │
│   knowledge_store.store_learning(Learning {                    │
│     content: "config.rs parser expects arrays not objects",    │
│     source_task: A,                                            │
│     tags: ["config", "parser", "bug"],                         │
│   })                                                           │
│                                                                │
└────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────┐
│ Knowledge Store (shared)                                        │
│                                                                │
│   Learnings:                                                   │
│     - "config.rs parser expects arrays not objects" [A]        │
│     - "API rate limit is 100 req/min" [B]                      │
│     - ...                                                      │
│                                                                │
└────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────┐
│ Task C starts (later, different task)                          │
│                                                                │
│   User prompt: "Fix the config loading bug"                    │
│       │                                                        │
│       ▼                                                        │
│   Context assembly phase:                                      │
│       │                                                        │
│       ▼                                                        │
│   knowledge_store.query_relevant("config loading bug")         │
│       │                                                        │
│       ▼                                                        │
│   Returns: "config.rs parser expects arrays not objects"       │
│       │                                                        │
│       ▼                                                        │
│   Injected into system prompt:                                 │
│   "Relevant learnings from previous tasks:                     │
│    - config.rs parser expects arrays not objects (from task A)"│
│       │                                                        │
│       ▼                                                        │
│   LLM sees the learning, immediately knows where to look       │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

### Rust Sketch: Shared Services

```rust
/// Shared services passed to each task loop
pub struct SharedServices {
    pub llm_client: Arc<LlmClient>,
    pub lock_manager: Arc<LockManager>,
    pub event_bus: Arc<EventBus>,
    pub knowledge_store: Arc<KnowledgeStore>,
    pub persistence: Arc<Persistence>,
    pub security_policy: Arc<SecurityPolicy>,
    pub git_coordinator: Arc<GitCoordinator>,
}

/// The agent loop receives shared services at spawn time
pub struct AgentLoop {
    task_id: TaskId,
    services: Arc<SharedServices>,
}

impl AgentLoop {
    pub async fn run(&self, task: &mut Task) -> Result<()> {
        // Context assembly uses knowledge store
        let relevant_learnings = self.services.knowledge_store
            .query_relevant(&task.context, 5)
            .await?;

        loop {
            // LLM call goes through shared client (rate limited, tiered)
            let response = self.services.llm_client
                .complete(request)
                .await?;

            // Tool execution acquires locks
            for tool_call in tool_calls {
                let resources = tool_call.required_resources();
                let _guard = self.services.lock_manager
                    .acquire(&resources, self.task_id)
                    .await?;

                // Security check
                self.services.security_policy
                    .check(&tool_call)?;

                // Execute tool...
            }

            // State changes persisted
            self.services.persistence
                .checkpoint(task)
                .await?;

            // Events published
            if needs_user_input {
                self.services.event_bus
                    .publish(Event::UserInputNeeded {
                        task_id: self.task_id
                    })
                    .await;

                // Yield until input arrives
                task.wait_for_input().await?;
            }
        }
    }
}
```

---

## The Agentic Loop (Production-Hardened)

This section specifies the agentic loop with production-level detail: every edge case, error path, retry strategy, and hook point.

### Design Principles

1. **Crash-recoverable** — Every state change checkpointed before proceeding
2. **Observable** — Hooks at every decision point for user extension
3. **Defensive** — Assume everything can fail, handle gracefully
4. **Idempotent** — Recovery can safely re-execute from any checkpoint

### Hook Points Overview

Hooks fire at specific points in the loop. Users can register handlers (shell/Rust/Python) that:
- **Observe** — Log, metrics, debugging
- **Modify** — Transform data passing through
- **Block** — Prevent action, return error
- **Inject** — Add content (context, tools, etc.)

```
TASK LIFECYCLE HOOKS
├─ TaskCreated          ─► Task added to system
├─ TaskScheduled        ─► Scheduler picked up task
├─ TaskStarted          ─► Loop begins execution
├─ TaskPaused           ─► User paused task
├─ TaskResumed          ─► User resumed task
├─ TaskCompleted        ─► Task finished successfully
├─ TaskFailed           ─► Task errored out
└─ TaskCancelled        ─► User killed task

LOOP ITERATION HOOKS
├─ PreContextAssembly   ─► Before building LLM context
├─ PostContextAssembly  ─► After context built
├─ PreLLMCall           ─► About to send to API
├─ PostLLMCall          ─► Response received
├─ PreToolUse           ─► Before tool execution
├─ PostToolUse          ─► After tool execution
├─ PreUserPrompt        ─► About to ask user
├─ PostUserInput        ─► User provided input
└─ LoopIteration        ─► Each iteration (for drift detection)

CROSS-TASK HOOKS
├─ LearningExtracted    ─► Learning identified
├─ KnowledgeQuery       ─► Querying knowledge store
├─ KnowledgeInjected    ─► Knowledge added to context
├─ ResourceLockWait     ─► Task waiting for lock
├─ ResourceLockAcquired ─► Lock obtained
├─ ResourceLockReleased ─► Lock released
└─ SubtaskSpawned       ─► Child task created

SECURITY HOOKS
├─ SecurityPolicyCheck  ─► Policy evaluation
├─ SecurityViolation    ─► Policy blocked action
├─ PermissionRequest    ─► Asking user for permission
├─ PermissionGranted    ─► User allowed action
├─ PermissionDenied     ─► User denied action
└─ AnomalyDetected      ─► Runtime check flagged something
```

---

### Phase 1: Task Initialization

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TASK INITIALIZATION                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1.1 LOAD OR CREATE TASK                                                    │
│      │                                                                      │
│      ├─► [HOOK: TaskCreated] if new task                                    │
│      │                                                                      │
│      ├─ If resuming from checkpoint:                                        │
│      │   ├─ Load task state from SQLite                                     │
│      │   ├─ Load conversation from git                                      │
│      │   ├─ Determine recovery point:                                       │
│      │   │   ├─ "request_sent" → will retry LLM call                        │
│      │   │   ├─ "tool_started" → check idempotency, maybe re-run            │
│      │   │   └─ "tool_completed" → resume normally                          │
│      │   └─ Log recovery action                                             │
│      │                                                                      │
│      ├─ If new task:                                                        │
│      │   ├─ Generate task_id (UUID v7 for time-ordering)                    │
│      │   ├─ Initialize empty conversation                                   │
│      │   └─ Set status = Queued                                             │
│      │                                                                      │
│      └─► CHECKPOINT: INSERT task into SQLite                                │
│                                                                             │
│  1.2 WAIT FOR SCHEDULER                                                     │
│      │                                                                      │
│      ├─ Scheduler checks:                                                   │
│      │   ├─ Current running count < max_concurrent                          │
│      │   ├─ Rate limit headroom available                                   │
│      │   └─ Priority queue ordering                                         │
│      │                                                                      │
│      ├─► [HOOK: TaskScheduled]                                              │
│      │                                                                      │
│      └─ Set status = Running { phase: Preparing }                           │
│                                                                             │
│  1.3 SETUP WORKSPACE                                                        │
│      │                                                                      │
│      ├─► [HOOK: TaskStarted]                                                │
│      │                                                                      │
│      ├─ If task needs git isolation:                                        │
│      │   ├─ Create git worktree: git worktree add .worktrees/{task_id}      │
│      │   ├─ Set task.working_dir = worktree path                            │
│      │   └─ On error: log warning, use main repo (degraded mode)            │
│      │                                                                      │
│      ├─ Initialize tool registry for this task                              │
│      │   ├─ Load built-in tools                                             │
│      │   ├─ Load skill-defined tools                                        │
│      │   └─ Generate tool schemas for LLM                                   │
│      │                                                                      │
│      └─► CHECKPOINT: UPDATE task with workspace info                        │
│                                                                             │
│  ERROR HANDLING:                                                            │
│  ├─ SQLite write fails → retry 3x with backoff, then fail task              │
│  ├─ Git worktree fails → continue in degraded mode (log warning)            │
│  └─ Tool registry fails → fail task (unrecoverable)                         │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Phase 2: Context Assembly

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         CONTEXT ASSEMBLY                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  2.1 PRE-ASSEMBLY HOOK                                                      │
│      │                                                                      │
│      └─► [HOOK: PreContextAssembly]                                         │
│          ├─ Can inject: additional context, instructions                    │
│          ├─ Can modify: task parameters                                     │
│          └─ Can block: return error to fail task                            │
│                                                                             │
│  2.2 BUILD SYSTEM PROMPT                                                    │
│      │                                                                      │
│      ├─ Base system prompt (Neuraphage instructions)                        │
│      ├─ Tool schemas (JSON schema for each registered tool)                 │
│      ├─ Environment context:                                                │
│      │   ├─ Working directory: {task.working_dir}                           │
│      │   ├─ Git state: branch, clean/dirty, recent commits                  │
│      │   ├─ Date/time: {now}                                                │
│      │   ├─ Model: {selected_model}                                         │
│      │   └─ Task metadata: id, name, parent, tags                           │
│      │                                                                      │
│      └─ Skill-injected context (from matched SKILL.md files)                │
│                                                                             │
│  2.3 KNOWLEDGE INJECTION                                                    │
│      │                                                                      │
│      ├─► [HOOK: KnowledgeQuery] with task context                           │
│      │                                                                      │
│      ├─ Query knowledge store:                                              │
│      │   ├─ Input: task description, tags, recent conversation              │
│      │   ├─ Method: keyword match (fast) or embedding similarity (better)   │
│      │   └─ Limit: top 5 relevant learnings                                 │
│      │                                                                      │
│      ├─► [HOOK: KnowledgeInjected] with selected learnings                  │
│      │   └─ Can filter, reorder, or add learnings                           │
│      │                                                                      │
│      └─ Append to system prompt:                                            │
│          "Relevant learnings from previous tasks:                           │
│           - {learning_1} (from task {source_task_id})                       │
│           - {learning_2} ..."                                               │
│                                                                             │
│  2.4 LOAD CONVERSATION HISTORY                                              │
│      │                                                                      │
│      ├─ Load messages from git-backed storage                               │
│      ├─ If resuming: include all messages up to recovery point              │
│      └─ If new: just the initial user message                               │
│                                                                             │
│  2.5 TOKEN COUNTING & MANAGEMENT                                            │
│      │                                                                      │
│      ├─ Count tokens: system + messages + tool_schemas                      │
│      │   └─ Use tiktoken or anthropic's counter                             │
│      │                                                                      │
│      ├─ If over context limit (e.g., 180k of 200k):                         │
│      │   ├─ Strategy 1: Summarize old messages                              │
│      │   │   ├─ Keep recent N messages verbatim                             │
│      │   │   ├─ Summarize older messages with cheap model                   │
│      │   │   └─ Replace old messages with summary                           │
│      │   │                                                                  │
│      │   ├─ Strategy 2: Truncate tool results                               │
│      │   │   ├─ Keep first/last N lines of large outputs                    │
│      │   │   └─ Add "[truncated, {X} lines omitted]"                        │
│      │   │                                                                  │
│      │   └─ Strategy 3: Drop least relevant messages                        │
│      │       └─ Based on recency and semantic relevance                     │
│      │                                                                      │
│      └─ Reserve tokens for response (e.g., 16k for max_tokens)              │
│                                                                             │
│  2.6 POST-ASSEMBLY HOOK                                                     │
│      │                                                                      │
│      └─► [HOOK: PostContextAssembly]                                        │
│          ├─ Receives: full assembled context                                │
│          ├─ Can modify: system prompt, messages                             │
│          ├─ Can log: token counts, context hash                             │
│          └─ Can block: return error if context invalid                      │
│                                                                             │
│  ERROR HANDLING:                                                            │
│  ├─ Knowledge store unavailable → continue without injection (log warning)  │
│  ├─ Token count exceeds limit after summarization → fail with clear error   │
│  └─ Git read fails → retry 3x, then fail task                               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Phase 3: LLM Request/Response

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         LLM REQUEST/RESPONSE                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  3.1 BUILD REQUEST                                                          │
│      │                                                                      │
│      ├─ Select model tier:                                                  │
│      │   ├─ Default: task.model_tier (usually "worker")                     │
│      │   ├─ Auto-escalate: if retry_count > threshold → upgrade tier        │
│      │   └─ Hook can override                                               │
│      │                                                                      │
│      ├─ Build request object:                                               │
│      │   {                                                                  │
│      │     model: "claude-sonnet-4-20250514",                               │
│      │     system: "<assembled system prompt>",                             │
│      │     messages: [<conversation history>],                              │
│      │     tools: [<tool schemas>],                                         │
│      │     max_tokens: 16384,                                               │
│      │     stream: true,                                                    │
│      │     // Optional:                                                     │
│      │     temperature: 0.7,                                                │
│      │     thinking: { type: "enabled", budget_tokens: 10000 }              │
│      │   }                                                                  │
│      │                                                                      │
│      └─► [HOOK: PreLLMCall]                                                 │
│          ├─ Can modify: request parameters, model selection                 │
│          ├─ Can log: request hash, estimated cost                           │
│          └─ Can block: return cached response, skip API call                │
│                                                                             │
│  3.2 CHECKPOINT & SEND                                                      │
│      │                                                                      │
│      ├─► CHECKPOINT: record "request_sent" marker                           │
│      │   └─ On recovery: we know to retry this request                      │
│      │                                                                      │
│      ├─ Submit to LLM client priority queue                                 │
│      │   ├─ Queue respects rate limits across all tasks                     │
│      │   ├─ High priority tasks get preference                              │
│      │   └─ Request waits if rate limited                                   │
│      │                                                                      │
│      └─ Set status = Running { phase: LlmRequest }                          │
│                                                                             │
│  3.3 STREAM RESPONSE                                                        │
│      │                                                                      │
│      ├─ Handle SSE events:                                                  │
│      │   │                                                                  │
│      │   ├─ message_start                                                   │
│      │   │   └─ Initialize response accumulator                             │
│      │   │                                                                  │
│      │   ├─ content_block_start                                             │
│      │   │   ├─ type: "text" → start text block                             │
│      │   │   ├─ type: "tool_use" → start tool_use block                     │
│      │   │   └─ type: "thinking" → start thinking block (if enabled)        │
│      │   │                                                                  │
│      │   ├─ content_block_delta                                             │
│      │   │   ├─ Append delta to current block                               │
│      │   │   ├─ If text: forward to TUI (if attached)                       │
│      │   │   └─ If tool_use: accumulate JSON                                │
│      │   │                                                                  │
│      │   ├─ content_block_stop                                              │
│      │   │   ├─ Finalize current block                                      │
│      │   │   └─ If tool_use: parse JSON, validate schema                    │
│      │   │                                                                  │
│      │   ├─ message_delta                                                   │
│      │   │   ├─ stop_reason: end_turn | tool_use | max_tokens               │
│      │   │   └─ usage: input_tokens, output_tokens                          │
│      │   │                                                                  │
│      │   └─ message_stop                                                    │
│      │       └─ Response complete                                           │
│      │                                                                      │
│      ├─ Track metrics:                                                      │
│      │   ├─ Input tokens (for cost)                                         │
│      │   ├─ Output tokens (for cost)                                        │
│      │   ├─ Time to first token (latency)                                   │
│      │   └─ Total response time                                             │
│      │                                                                      │
│      └─ Update cost tracker: task.cost += calculate_cost(usage, model)      │
│                                                                             │
│  3.4 POST-RESPONSE PROCESSING                                               │
│      │                                                                      │
│      ├─► [HOOK: PostLLMCall]                                                │
│      │   ├─ Receives: full response, usage stats                            │
│      │   ├─ Can log: response hash, token counts, cost                      │
│      │   ├─ Can extract: patterns, learnings                                │
│      │   └─ Can modify: response content (use carefully)                    │
│      │                                                                      │
│      ├─ Parse response.content blocks:                                      │
│      │   ├─ thinking blocks → log but don't include in conversation         │
│      │   ├─ text blocks → collect into assistant message                    │
│      │   └─ tool_use blocks → collect for execution                         │
│      │                                                                      │
│      ├─ Validate tool_use blocks:                                           │
│      │   ├─ Tool name exists in registry?                                   │
│      │   ├─ Parameters match schema?                                        │
│      │   └─ On validation error: tool_result = error message                │
│      │                                                                      │
│      └─► CHECKPOINT: git commit conversation with response                  │
│                                                                             │
│  3.5 ROUTE BY STOP REASON                                                   │
│      │                                                                      │
│      └─ match stop_reason:                                                  │
│                                                                             │
│          "end_turn" | "stop_sequence" ──────────────────────────────────┐   │
│          │                                                              │   │
│          │  No tool calls, model finished speaking                      │   │
│          │  ├─ Append assistant message to conversation                 │   │
│          │  ├─ Check for learning indicators                            │   │
│          │  │   └─ If found: queue for extraction                       │   │
│          │  ├─► [HOOK: LoopIteration] with iteration stats              │   │
│          │  ├─ Set status = WaitingForUser                              │   │
│          │  ├─► event_bus.publish(UserInputNeeded)                      │   │
│          │  └─ YIELD ──────────────────────────────────► WAIT FOR INPUT │   │
│          │                                                              │   │
│          "tool_use" ────────────────────────────────────────────────────┤   │
│          │                                                              │   │
│          │  Model wants to execute tools                                │   │
│          │  ├─ Append assistant message (with tool_use) to conversation │   │
│          │  └─ GOTO ──────────────────────────────► TOOL EXECUTION      │   │
│          │                                                              │   │
│          "max_tokens" ──────────────────────────────────────────────────┤   │
│          │                                                              │   │
│          │  Response was cut off                                        │   │
│          │  ├─ Append partial response to conversation                  │   │
│          │  ├─ If has tool_use blocks: execute them first               │   │
│          │  └─ Auto-continue: LOOP BACK ──────────► CONTEXT ASSEMBLY    │   │
│          │                                                              │   │
│          null | error ──────────────────────────────────────────────────┘   │
│          │                                                                  │
│          │  Something went wrong                                            │
│          └─ GOTO ──────────────────────────────────► ERROR HANDLING         │
│                                                                             │
│  ERROR HANDLING:                                                            │
│  ├─ Network timeout → retry with exponential backoff (max 3)                │
│  ├─ 429 Rate Limited → respect Retry-After header, requeue                  │
│  ├─ 400 Bad Request → log, fail task (likely bad context)                   │
│  ├─ 500 Server Error → retry with backoff (max 3)                           │
│  ├─ 529 Overloaded → wait 30s, retry (max 3)                                │
│  ├─ Stream interrupted → if >50% received, try to use partial               │
│  ├─ JSON parse error in tool_use → return error as tool_result              │
│  └─ Unknown tool name → return "unknown tool" as tool_result                │
│                                                                             │
│  RETRY STRATEGY:                                                            │
│  ├─ Attempt 1: immediate                                                    │
│  ├─ Attempt 2: wait 1s                                                      │
│  ├─ Attempt 3: wait 5s                                                      │
│  ├─ Attempt 4: wait 30s (for 529 only)                                      │
│  └─ After max retries: fail task with error details                         │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Phase 4: Tool Execution

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TOOL EXECUTION                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  For each tool_use block in response:                                       │
│                                                                             │
│  4.1 SECURITY CHECK (Layer 1: Policy)                                       │
│      │                                                                      │
│      ├─► [HOOK: SecurityPolicyCheck]                                        │
│      │   ├─ Input: tool_name, parameters, task context                      │
│      │   └─ Can: add checks, override decision                              │
│      │                                                                      │
│      ├─ Evaluate against security policy:                                   │
│      │   ├─ Is tool in blocked list?                                        │
│      │   ├─ Do parameters match blocked patterns?                           │
│      │   │   ├─ Path in protected directories?                              │
│      │   │   ├─ Command matches dangerous regex?                            │
│      │   │   └─ Host in blocked network list?                               │
│      │   └─ Does operation exceed rate limits?                              │
│      │                                                                      │
│      ├─ If policy DENIES:                                                   │
│      │   ├─► [HOOK: SecurityViolation]                                      │
│      │   │   └─ Can: log, alert, escalate                                   │
│      │   └─ tool_result = { error: "Blocked by security policy: {reason}" } │
│      │       └─ SKIP to next tool_use block                                 │
│      │                                                                      │
│      └─ If policy ALLOWS: continue                                          │
│                                                                             │
│  4.2 PERMISSION CHECK (Layer 2: User Allowlist)                             │
│      │                                                                      │
│      ├─ Check if operation is pre-approved:                                 │
│      │   ├─ Tool in always-allowed list? (e.g., Read, Glob)                 │
│      │   ├─ Path matches allowed patterns?                                  │
│      │   ├─ Command matches allowed patterns?                               │
│      │   └─ User previously said "always allow"?                            │
│      │                                                                      │
│      ├─ If NOT pre-approved:                                                │
│      │   │                                                                  │
│      │   ├─► [HOOK: PermissionRequest]                                      │
│      │   │   └─ Can: auto-approve, auto-deny, modify prompt                 │
│      │   │                                                                  │
│      │   ├─► [HOOK: PreUserPrompt]                                          │
│      │   │                                                                  │
│      │   ├─ Set status = WaitingForUser {                                   │
│      │   │     prompt: "Allow {tool}: {summary}?",                          │
│      │   │     options: ["Yes", "No", "Always allow", "Never allow"]        │
│      │   │   }                                                              │
│      │   │                                                                  │
│      │   ├─► event_bus.publish(UserInputNeeded)                             │
│      │   │                                                                  │
│      │   ├─ YIELD ─────────────────────────────────────► WAIT FOR INPUT     │
│      │   │                                                                  │
│      │   ├─► [HOOK: PostUserInput] with decision                            │
│      │   │                                                                  │
│      │   └─ Handle decision:                                                │
│      │       ├─ "Yes" → continue                                            │
│      │       ├─ "No" →                                                      │
│      │       │   ├─► [HOOK: PermissionDenied]                               │
│      │       │   └─ tool_result = { error: "User denied permission" }       │
│      │       │       └─ SKIP to next tool_use block                         │
│      │       ├─ "Always allow" →                                            │
│      │       │   ├─► [HOOK: PermissionGranted]                              │
│      │       │   ├─ Add pattern to user allowlist                           │
│      │       │   └─ continue                                                │
│      │       └─ "Never allow" →                                             │
│      │           ├─ Add pattern to user blocklist                           │
│      │           └─ tool_result = error, SKIP                               │
│      │                                                                      │
│      └─► [HOOK: PermissionGranted] if approved                              │
│                                                                             │
│  4.3 RESOURCE LOCKING                                                       │
│      │                                                                      │
│      ├─ Identify required resources:                                        │
│      │   ├─ Read tool → Shared lock on file                                 │
│      │   ├─ Write/Edit tool → Exclusive lock on file                        │
│      │   ├─ Bash tool → Depends on command analysis                         │
│      │   ├─ Git tool → Exclusive lock on repo                               │
│      │   └─ WebFetch → Rate limit lock on host                              │
│      │                                                                      │
│      ├─ Sort resources (deadlock prevention):                               │
│      │   └─ Alphabetical by resource ID                                     │
│      │                                                                      │
│      ├─ For each resource:                                                  │
│      │   │                                                                  │
│      │   ├─ Try acquire lock:                                               │
│      │   │   ├─ If available → acquire, continue                            │
│      │   │   └─ If held by another task →                                   │
│      │   │       │                                                          │
│      │   │       ├─► [HOOK: ResourceLockWait]                               │
│      │   │       │                                                          │
│      │   │       ├─ Set status = Blocked {                                  │
│      │   │       │     resource: {resource_id},                             │
│      │   │       │     waiting_since: now()                                 │
│      │   │       │   }                                                      │
│      │   │       │                                                          │
│      │   │       ├─ Wait with timeout (default 60s):                        │
│      │   │       │   ├─ Lock acquired → continue                            │
│      │   │       │   └─ Timeout → tool_result = error, SKIP                 │
│      │   │       │                                                          │
│      │   │       └─► [HOOK: ResourceLockAcquired]                           │
│      │   │                                                                  │
│      │   └─► [HOOK: ResourceLockAcquired] for each lock                     │
│      │                                                                      │
│      └─ All locks acquired → continue                                       │
│                                                                             │
│  4.4 EXECUTE TOOL                                                           │
│      │                                                                      │
│      ├─► CHECKPOINT: record "tool_started" + tool_name + params             │
│      │   └─ On recovery: check if tool actually ran (idempotency)           │
│      │                                                                      │
│      ├─► [HOOK: PreToolUse]                                                 │
│      │   ├─ Input: tool_name, params, task context                          │
│      │   ├─ Can: modify params, log, inject context                         │
│      │   └─ Can: block execution, return synthetic result                   │
│      │                                                                      │
│      ├─ Set status = Running { phase: ToolExecution { tool: name } }        │
│      │                                                                      │
│      ├─ Execute with timeout (per-tool, default 120s):                      │
│      │   │                                                                  │
│      │   ├─ Read tool:                                                      │
│      │   │   ├─ Read file contents                                          │
│      │   │   ├─ Add line numbers                                            │
│      │   │   ├─ Truncate if > 30k chars                                     │
│      │   │   └─ Return content or error                                     │
│      │   │                                                                  │
│      │   ├─ Write tool:                                                     │
│      │   │   ├─ Write content to file                                       │
│      │   │   ├─ Create parent directories if needed                         │
│      │   │   └─ Return success or error                                     │
│      │   │                                                                  │
│      │   ├─ Edit tool:                                                      │
│      │   │   ├─ Find old_string in file (must be unique)                    │
│      │   │   ├─ Replace with new_string                                     │
│      │   │   └─ Return success, "not found", or "ambiguous"                 │
│      │   │                                                                  │
│      │   ├─ Bash tool:                                                      │
│      │   │   ├─ Spawn subprocess with timeout                               │
│      │   │   ├─ Capture stdout + stderr                                     │
│      │   │   ├─ Truncate output if > 30k chars                              │
│      │   │   └─ Return output + exit code                                   │
│      │   │                                                                  │
│      │   ├─ Glob tool:                                                      │
│      │   │   ├─ Match files against pattern                                 │
│      │   │   ├─ Sort by modification time                                   │
│      │   │   └─ Return file list                                            │
│      │   │                                                                  │
│      │   ├─ Grep tool:                                                      │
│      │   │   ├─ Search files for pattern (ripgrep)                          │
│      │   │   ├─ Apply options (-A, -B, -C, etc.)                            │
│      │   │   └─ Return matches                                              │
│      │   │                                                                  │
│      │   ├─ WebFetch tool:                                                  │
│      │   │   ├─ Fetch URL                                                   │
│      │   │   ├─ Convert HTML to markdown                                    │
│      │   │   ├─ Summarize with small model                                  │
│      │   │   └─ Return summary                                              │
│      │   │                                                                  │
│      │   ├─ Task tool (subtask):                                            │
│      │   │   ├─► [HOOK: SubtaskSpawned]                                     │
│      │   │   ├─ Check subtask depth < max_depth                             │
│      │   │   ├─ Create child task with parent = this task                   │
│      │   │   ├─ Wait for child completion (or timeout)                      │
│      │   │   └─ Return child's final response                               │
│      │   │                                                                  │
│      │   └─ Custom/Skill tools:                                             │
│      │       ├─ Dispatch to registered handler                              │
│      │       └─ Return handler result                                       │
│      │                                                                      │
│      ├─ On timeout:                                                         │
│      │   ├─ Kill subprocess (if applicable)                                 │
│      │   └─ tool_result = { error: "Tool execution timed out after Xs" }    │
│      │                                                                      │
│      ├─ On error:                                                           │
│      │   └─ tool_result = { error: "{error_message}" }                      │
│      │                                                                      │
│      ├─ On success:                                                         │
│      │   └─ tool_result = { content: "{output}" }                           │
│      │                                                                      │
│      ├─► [HOOK: PostToolUse]                                                │
│      │   ├─ Input: tool_name, params, result, duration                      │
│      │   ├─ Can: transform result, log, extract patterns                    │
│      │   └─ Can: flag for learning extraction                               │
│      │                                                                      │
│      ├─► CHECKPOINT: record "tool_completed" + result                       │
│      │                                                                      │
│      └─ Release locks:                                                      │
│          └─► [HOOK: ResourceLockReleased] for each lock                     │
│                                                                             │
│  4.5 RUNTIME CHECKS (Layer 3: Anomaly Detection)                            │
│      │                                                                      │
│      ├─ Check for anomalies:                                                │
│      │   ├─ Too many file writes this minute?                               │
│      │   ├─ Unusual file paths being accessed?                              │
│      │   ├─ Network connections to unexpected hosts?                        │
│      │   └─ Resource usage spikes?                                          │
│      │                                                                      │
│      └─ If anomaly detected:                                                │
│          ├─► [HOOK: AnomalyDetected]                                        │
│          │   └─ Can: pause task, notify user, continue with warning         │
│          └─ Default: log warning, continue                                  │
│                                                                             │
│  4.6 ACCUMULATE RESULTS                                                     │
│      │                                                                      │
│      ├─ Build tool_result message:                                          │
│      │   {                                                                  │
│      │     role: "user",                                                    │
│      │     content: [                                                       │
│      │       { type: "tool_result", tool_use_id: "...", content: "..." },   │
│      │       { type: "tool_result", tool_use_id: "...", content: "..." },   │
│      │       ...                                                            │
│      │     ]                                                                │
│      │   }                                                                  │
│      │                                                                      │
│      ├─ Append to conversation                                              │
│      │                                                                      │
│      ├─► CHECKPOINT: git commit with tool results                           │
│      │                                                                      │
│      ├─► [HOOK: LoopIteration] with iteration stats                         │
│      │   ├─ iteration_count, tokens_used, tools_executed                    │
│      │   └─ Can: detect drift (too many iterations without progress)        │
│      │                                                                      │
│      └─ LOOP BACK ────────────────────────────────► CONTEXT ASSEMBLY        │
│                                                                             │
│  ERROR HANDLING:                                                            │
│  ├─ Tool not found → tool_result: "Unknown tool: {name}"                    │
│  ├─ Invalid params → tool_result: "Invalid parameters: {details}"           │
│  ├─ Execution error → tool_result: "Error: {message}"                       │
│  ├─ Timeout → tool_result: "Timeout after {N}s"                             │
│  ├─ Lock timeout → tool_result: "Resource locked by another task"           │
│  └─ All errors → logged, included in conversation, loop continues           │
│                                                                             │
│  IDEMPOTENCY (for recovery):                                                │
│  ├─ Read → always safe to re-run                                            │
│  ├─ Write → check file hash, skip if matches                                │
│  ├─ Edit → check if old_string still present                                │
│  ├─ Bash → generally NOT idempotent (log warning on recovery)               │
│  ├─ Git → check if commit/branch already exists                             │
│  └─ WebFetch → safe to re-run (cache may help)                              │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Phase 5: User Input Handling

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         USER INPUT HANDLING                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  When task yields with status = WaitingForUser:                             │
│                                                                             │
│  5.1 WAIT FOR INPUT                                                         │
│      │                                                                      │
│      ├─ Task is suspended (not consuming resources)                         │
│      ├─ Other tasks continue running                                        │
│      │                                                                      │
│      └─ Input arrives via:                                                  │
│          ├─ TUI: user types in attached session                             │
│          ├─ CLI: neuraphage task send <id> "message"                        │
│          └─ API: programmatic input                                         │
│                                                                             │
│  5.2 PROCESS INPUT                                                          │
│      │                                                                      │
│      ├─► [HOOK: PostUserInput]                                              │
│      │   ├─ Input: user message, task context                               │
│      │   ├─ Can: transform input, validate                                  │
│      │   └─ Can: reject invalid input                                       │
│      │                                                                      │
│      ├─ If permission response (from tool confirmation):                    │
│      │   └─ Route to permission handler → resume tool execution             │
│      │                                                                      │
│      ├─ If conversation input:                                              │
│      │   ├─ Append user message to conversation                             │
│      │   ├─► CHECKPOINT: git commit with user message                       │
│      │   └─ Resume loop → CONTEXT ASSEMBLY                                  │
│      │                                                                      │
│      └─ If special command:                                                 │
│          ├─ "/pause" → set status = Paused, YIELD                           │
│          ├─ "/cancel" → set status = Cancelled, GOTO TERMINATION            │
│          ├─ "/retry" → discard last response, re-run LLM call               │
│          └─ "/context" → show current context stats                         │
│                                                                             │
│  5.3 HANDLE PAUSE/RESUME                                                    │
│      │                                                                      │
│      ├─ On pause:                                                           │
│      │   ├─► [HOOK: TaskPaused]                                             │
│      │   ├─ Set status = Paused                                             │
│      │   ├─► CHECKPOINT: persist state                                      │
│      │   └─ YIELD indefinitely                                              │
│      │                                                                      │
│      └─ On resume:                                                          │
│          ├─► [HOOK: TaskResumed]                                            │
│          ├─ Restore status = Running                                        │
│          └─ Continue from saved point                                       │
│                                                                             │
│  TIMEOUT HANDLING:                                                          │
│  ├─ Permission prompts: timeout after 5 minutes → deny                      │
│  ├─ Conversation input: no timeout (task stays waiting)                     │
│  └─ Configurable per-task timeouts available                                │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### Phase 6: Task Termination

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TASK TERMINATION                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Triggered by:                                                              │
│  ├─ Natural completion (model says "done")                                  │
│  ├─ User cancellation                                                       │
│  ├─ Unrecoverable error                                                     │
│  └─ Max iterations reached                                                  │
│                                                                             │
│  6.1 FINALIZE STATUS                                                        │
│      │                                                                      │
│      ├─ Set terminal status:                                                │
│      │   ├─ Completed { summary, completed_at }                             │
│      │   ├─ Failed { error, recoverable, failed_at }                        │
│      │   └─ Cancelled { cancelled_at }                                      │
│      │                                                                      │
│      └─► CHECKPOINT: final state to SQLite + git                            │
│                                                                             │
│  6.2 GENERATE SUMMARY                                                       │
│      │                                                                      │
│      ├─ Extract summary from conversation:                                  │
│      │   ├─ Use last assistant message, or                                  │
│      │   └─ Generate with cheap model if needed                             │
│      │                                                                      │
│      └─ Store in task.summary                                               │
│                                                                             │
│  6.3 LEARNING EXTRACTION                                                    │
│      │                                                                      │
│      ├─ Process queued learning candidates:                                 │
│      │   ├─ If keyword-only mode: store directly                            │
│      │   └─ If hybrid mode: refine with LLM                                 │
│      │                                                                      │
│      ├─► [HOOK: LearningExtracted] for each learning                        │
│      │   └─ Can: enrich, tag, route to specific store                       │
│      │                                                                      │
│      └─ Store in knowledge/learnings/                                       │
│                                                                             │
│  6.4 CLEANUP                                                                │
│      │                                                                      │
│      ├─ Release any held locks:                                             │
│      │   └─► [HOOK: ResourceLockReleased] for each                          │
│      │                                                                      │
│      ├─ If subtask: notify parent task                                      │
│      │   └─ Parent receives child's final response                          │
│      │                                                                      │
│      ├─ Git worktree cleanup (if applicable):                               │
│      │   ├─ If task completed successfully:                                 │
│      │   │   ├─ Offer to merge branch to main                               │
│      │   │   └─ Remove worktree: git worktree remove                        │
│      │   └─ If task failed/cancelled:                                       │
│      │       ├─ Keep worktree for debugging (configurable)                  │
│      │       └─ Or remove: git worktree remove --force                      │
│      │                                                                      │
│      └─► [HOOK: TaskCompleted | TaskFailed | TaskCancelled]                 │
│                                                                             │
│  6.5 NOTIFY                                                                 │
│      │                                                                      │
│      ├─► event_bus.publish(TaskCompleted | TaskFailed | TaskCancelled)      │
│      │                                                                      │
│      ├─ Update TUI (if running)                                             │
│      │                                                                      │
│      ├─ Update terminal title                                               │
│      │                                                                      │
│      └─ Optional voice notification                                         │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Task States (Complete)

```rust
pub enum TaskStatus {
    /// Waiting in queue to be scheduled
    Queued,

    /// Actively executing
    Running {
        phase: RunningPhase,
        started_at: DateTime<Utc>,
        iteration: u32,
    },

    /// Waiting for user input (non-blocking to other tasks)
    WaitingForUser {
        prompt: String,
        context: UserInputContext,
        waiting_since: DateTime<Utc>,
    },

    /// Blocked on resource lock
    Blocked {
        resource: ResourceId,
        waiting_since: DateTime<Utc>,
        holder_task: Option<TaskId>,
    },

    /// Paused by user
    Paused {
        paused_at: DateTime<Utc>,
        resume_point: ResumePoint,
    },

    /// Successfully completed
    Completed {
        summary: String,
        completed_at: DateTime<Utc>,
        iterations: u32,
        total_cost: f64,
    },

    /// Failed with error
    Failed {
        error: String,
        recoverable: bool,
        failed_at: DateTime<Utc>,
        retry_count: u32,
    },

    /// Cancelled by user
    Cancelled {
        cancelled_at: DateTime<Utc>,
        reason: Option<String>,
    },
}

pub enum RunningPhase {
    /// Initial setup
    Initializing,
    /// Building context
    AssemblingContext,
    /// Waiting for LLM response
    LlmRequest { request_id: String },
    /// Executing tool
    ToolExecution { tool: String, tool_use_id: String },
    /// Processing response
    Processing,
}

pub enum UserInputContext {
    /// Awaiting next conversation message
    Conversation,
    /// Awaiting permission decision
    Permission {
        tool: String,
        params_summary: String,
        options: Vec<String>,
    },
    /// Awaiting question response
    Question {
        question: String,
        options: Option<Vec<String>>,
    },
}

pub enum ResumePoint {
    /// Resume at context assembly
    ContextAssembly,
    /// Resume at LLM call (retry)
    LlmCall,
    /// Resume at tool execution
    ToolExecution { remaining_tools: Vec<ToolCall> },
    /// Resume waiting for user
    WaitingForUser,
}
```

---

## Recovery Matrix

| Checkpoint State | Recovery Action |
|------------------|-----------------|
| `task_created` | Re-enter scheduler queue |
| `request_sent` | Retry LLM call (request is idempotent) |
| `response_received` | Parse and continue from routing |
| `tool_started:{tool}:{id}` | Check idempotency, re-run or skip |
| `tool_completed:{tool}:{id}` | Continue to next tool or loop |
| `waiting_for_user` | Republish UserInputNeeded event |
| `paused` | Stay paused until user resumes |
| `completed` | No action |
| `failed` | No action (or offer retry) |

---

## Drift Detection

To prevent runaway loops (task burning tokens without progress):

```rust
pub struct DriftDetector {
    max_iterations: u32,           // Default: 50
    max_tokens_per_iteration: u32, // Default: 50000
    max_tool_failures: u32,        // Default: 10
    max_time_without_progress: Duration, // Default: 10 minutes

    // Progress indicators
    last_meaningful_output: DateTime<Utc>,
    consecutive_empty_responses: u32,
    consecutive_tool_failures: u32,
}

impl DriftDetector {
    pub fn check(&mut self, iteration: &IterationStats) -> DriftStatus {
        // Too many iterations
        if iteration.count > self.max_iterations {
            return DriftStatus::Stuck("Max iterations exceeded");
        }

        // Burning tokens
        if iteration.tokens > self.max_tokens_per_iteration {
            return DriftStatus::Warning("High token usage this iteration");
        }

        // Tool failures accumulating
        if self.consecutive_tool_failures > self.max_tool_failures {
            return DriftStatus::Stuck("Too many consecutive tool failures");
        }

        // No progress
        if iteration.timestamp - self.last_meaningful_output > self.max_time_without_progress {
            return DriftStatus::Stuck("No meaningful progress");
        }

        DriftStatus::Ok
    }
}
```

---

## Persistence & Recovery

### Core Principle

**Crash-recoverable by default. All state changes persisted before acknowledgment.**

If it's not on disk, it didn't happen.

### Engram for Task State

Task graph state (items, edges, status) is persisted via [engram](../../engram/):
- **JSONL source of truth** — append-only, git-friendly
- **SQLite cache** — fast queries, rebuilt on startup
- **ready()/blocked()** — identify actionable tasks

Conversations are stored separately in git-backed repositories per task.

### Checkpoint Strategy

| Event | Checkpoint Action | Recovery Behavior |
|-------|-------------------|-------------------|
| Task created | INSERT into SQLite | Recreate task in Queued state |
| LLM request sent | UPDATE task with "request_sent" marker | Retry the LLM request |
| LLM response received | Git commit conversation | Resume from last response |
| Tool started | UPDATE task with tool + params | Check if tool ran (idempotency) |
| Tool completed | Git commit + UPDATE task | Resume with tool result |
| User input received | Git commit + UPDATE task | Resume with input |
| Task completed | UPDATE status, git commit | No action needed |

### Recovery Flow

```
Daemon starts
    │
    ▼
Load all tasks from SQLite
    │
    ▼
For each task:
    │
    ├─ Status: Queued → Re-queue for scheduling
    │
    ├─ Status: Running → Check last checkpoint
    │   ├─ "request_sent" → Retry LLM call
    │   ├─ "tool_started" → Check idempotency, maybe re-run
    │   └─ "tool_completed" → Resume loop
    │
    ├─ Status: WaitingForUser → Republish UserInputNeeded event
    │
    ├─ Status: Blocked → Re-attempt lock acquisition
    │
    └─ Status: Completed/Failed/Cancelled → No action
```

### Lock Recovery

Locks use leases with daemon PID:

```rust
struct Lock {
    resource: ResourceId,
    holder_task: TaskId,
    holder_daemon_pid: u32,
    acquired_at: DateTime<Utc>,
    lease_expires: DateTime<Utc>,
}
```

On recovery:
1. Check if PID in lock is still alive
2. If dead → lock is stale, can be taken
3. If alive but different daemon → wait or steal based on lease expiry

---

*This document is part of the [Neuraphage Design Documentation](./neuraphage-design.md).*
