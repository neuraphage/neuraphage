# Neuraphage: Multi-Task AI Orchestrator Design

> A Rust-based daemon for concurrent AI agent task execution with crash-recoverable architecture.

**Author:** Scott Aidler
**Date:** 2026-01-10
**Status:** Design Document
**Review Passes:** 5/5 (Rule of Five applied)

---

## Documentation

This design is split across multiple documents:

| Document | Description |
|----------|-------------|
| **[Overview](./neuraphage-design.md)** | Architecture, design decisions, open questions (this document) |
| **[Agentic Loop](./neuraphage-agentic-loop.md)** | Core loop mechanics, phases, persistence, recovery |
| **[Personas](./neuraphage-personas.md)** | Agent personalities, Watcher, Syncer, coordination |
| **[Infrastructure](./neuraphage-infrastructure.md)** | Security model, Git worktrees, TUI design |
| **[Engram Integration](../../engram/docs/implementation-plan.md)** | Task graph persistence layer (engram library) |

---

## Table of Contents

1. [Executive Summary](#executive-summary)
2. [Design Decisions](#design-decisions)
3. [Architecture Overview](#architecture-overview)
4. [Engram Integration](#engram-integration)
5. [Implementation Plan](#implementation-plan)
6. [Open Questions](#open-questions)
7. [References](#references)

---

## Executive Summary

Neuraphage is a ground-up Rust implementation of a multi-task AI orchestrator. Unlike Claude Code, Cursor, or KAI/PAI (which are single-task-per-session), Neuraphage manages N concurrent tasks within a single daemon process.

**Core thesis:** The agentic loop is simple; multi-task coordination is where the real complexity and value lies.

**Positioning:**

```
                Single-Task          Multi-Task
              ┌─────────────────┬─────────────────┐
              │                 │                 │
   Chaotic    │    Cursor/      │   Gas Town      │
              │    Aider        │                 │
              │                 │                 │
              ├─────────────────┼─────────────────┤
              │                 │                 │
   Disciplined│   KAI/PAI       │   Neuraphage    │
              │   Claude Code   │                 │
              │                 │                 │
              └─────────────────┴─────────────────┘
```

Neuraphage targets the **disciplined multi-task** quadrant—no existing tool occupies this space.

---

## Design Decisions

Decisions made 2026-01-08:

| # | Decision | Choice | Notes |
|---|----------|--------|-------|
| 1 | Layer on Claude Code or replace? | **Replace entirely** | Full Rust implementation, own the whole stack |
| 2 | Daemon vs on-demand? | **Daemon + tokio** | Long-running process, tasks as tokio spawns |
| 3 | Process model? | **Tokio tasks (lightweight)** | ~KB overhead, shared rate limiter/knowledge. Risk: crash isolation |
| 4 | Persistence backend? | **Engram + git** | Engram (JSONL+SQLite) for task graph, git for conversations |
| 5 | Git coordination? | **Git worktrees** | Each task gets isolated worktree, clean parallel execution |
| 6 | Sub-task support? | **Yes, bounded (2-3 levels)** | Sane defaults, user can override depth limit |
| 7 | User attention routing? | **Queue + notify + TUI** | Tasks queue for input, ratatui TUI for management |
| 8 | Notification strategy? | **TUI + terminal title + optional voice/socket** | Primary: TUI running, terminal title. Optional: TTS, status socket |
| 9 | Learning extraction? | **Hybrid (keyword + LLM), configurable** | Default: keyword triggers, LLM refines. Override to keyword-only or LLM-only |
| 10 | Rate limiting? | **Multi-key pool + model tiering + priority queue** | Multiple API keys, router/worker/expert model tiers, auto-escalation |
| 11 | Crash isolation? | **Crash-recoverable by default** | All state persisted before ack. Supervisor restarts daemon. Future: warm standby |
| 12 | Security model? | **Layered (policy + permissions + runtime + human)** | Sane defaults, user overrides. Details in [Infrastructure](./neuraphage-infrastructure.md) |

---

## Architecture Overview

### High-Level Design

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           NEURAPHAGE DAEMON                                  │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         TASK MANAGER                                    │ │
│  │                                                                         │ │
│  │   ┌─────────┐   ┌─────────┐   ┌─────────┐   ┌─────────┐              │ │
│  │   │ Task 1  │   │ Task 2  │   │ Task 3  │   │ Task N  │              │ │
│  │   │         │   │         │   │         │   │         │              │ │
│  │   │ [Loop]  │   │ [Loop]  │   │ [Loop]  │   │ [Loop]  │              │ │
│  │   │ [State] │   │ [State] │   │ [State] │   │ [State] │              │ │
│  │   │ [Conv]  │   │ [Conv]  │   │ [Conv]  │   │ [Conv]  │              │ │
│  │   └────┬────┘   └────┬────┘   └────┬────┘   └────┬────┘              │ │
│  │        │             │             │             │                    │ │
│  └────────┼─────────────┼─────────────┼─────────────┼────────────────────┘ │
│           │             │             │             │                      │
│  ┌────────▼─────────────▼─────────────▼─────────────▼────────────────────┐ │
│  │                        SHARED SERVICES                                 │ │
│  │                                                                        │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                │ │
│  │  │  LLM Client  │  │    Tool      │  │  Knowledge   │                │ │
│  │  │              │  │   Executor   │  │    Store     │                │ │
│  │  │ - Multi-key  │  │              │  │              │                │ │
│  │  │ - Tiering    │  │ - File ops   │  │ - Learnings  │                │ │
│  │  │ - Rate limit │  │ - Bash       │  │ - Decisions  │                │ │
│  │  │ - Priority Q │  │ - Git        │  │ - Patterns   │                │ │
│  │  └──────────────┘  │ - Locks      │  └──────────────┘                │ │
│  │                    └──────────────┘                                   │ │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                │ │
│  │  │  Event Bus   │  │  Persistence │  │   Security   │                │ │
│  │  │  (SQLite Q)  │  │  (SQLite+Git)│  │   Policy     │                │ │
│  │  └──────────────┘  └──────────────┘  └──────────────┘                │ │
│  │                                                                        │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
└──────────────────────────────────┬───────────────────────────────────────────┘
                                   │
                                   │ Unix Socket
                                   │
┌──────────────────────────────────▼───────────────────────────────────────────┐
│                              NEURAPHAGE CLI / TUI                             │
│                                                                               │
│   neuraphage daemon start|stop|status                                        │
│   neuraphage task new "description"                                          │
│   neuraphage task list [--status running|waiting|blocked]                    │
│   neuraphage task attach <id>                                                │
│   neuraphage task detach                                                     │
│   neuraphage task send <id> "message"                                        │
│   neuraphage task kill <id>                                                  │
│   neuraphage tui                           # Full TUI dashboard              │
│                                                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

### Data Layout

```
~/.config/neuraphage/
├── neuraphage.db              # SQLite: task metadata, event queue, learnings index
├── neuraphage.yaml            # User configuration
├── api-keys.yaml              # Multi-key configuration (gitignored)
├── tasks/
│   └── {task_id}/
│       ├── conversation/      # Git repo for conversation history
│       │   ├── .git/
│       │   └── messages.json
│       ├── worktree/          # Git worktree for this task (if applicable)
│       └── outputs/           # Large tool outputs
├── knowledge/
│   ├── learnings/             # Extracted learnings (markdown)
│   └── decisions/             # Recorded decisions (markdown)
└── logs/
    └── daemon.log
```

### Model Tiering Configuration

```yaml
# ~/.config/neuraphage/neuraphage.yaml

models:
  router:
    provider: anthropic
    model: claude-3-haiku-20240307
    api-key-pool: [cheap]
    use-for:
      - skill-routing
      - classification
      - learning-extraction

  worker:
    provider: anthropic
    model: claude-sonnet-4-20250514
    api-key-pool: [main, secondary]
    use-for:
      - standard-tasks
      - code-generation

  expert:
    provider: anthropic
    model: claude-opus-4-20250514
    api-key-pool: [main]
    use-for:
      - complex-reasoning
      - escalation

api-key-pools:
  main:
    keys:
      - ${ANTHROPIC_API_KEY_1}
      - ${ANTHROPIC_API_KEY_2}
  cheap:
    keys:
      - ${ANTHROPIC_API_KEY_HAIKU}
  secondary:
    keys:
      - ${ANTHROPIC_API_KEY_3}

task-defaults:
  model-tier: worker
  auto-escalate: true
  escalate-after-retries: 3
  max-subtask-depth: 3
```

---

## Engram Integration

Neuraphage uses [engram](../../engram/) as its task graph persistence layer. Engram provides:

- **Task/Item graph** with blocking dependencies
- **ready()** query — tasks with no open blockers
- **blocked()** query — tasks blocked by open tasks
- **JSONL persistence** — git-friendly append-only log
- **SQLite cache** — fast queries rebuilt from JSONL

### Architecture with Engram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           NEURAPHAGE DAEMON                                  │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                    TASK MANAGER (wraps engram)                         │ │
│  │                                                                         │ │
│  │   engram::Store ──┬──► Task graph (items, edges, ready/blocked)        │ │
│  │                   │                                                     │ │
│  │                   └──► Maps: engram::Item ↔ neuraphage::Task metadata  │ │
│  │                                                                         │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                    CONVERSATION STORE (separate from engram)            │ │
│  │                                                                         │ │
│  │   ~/.config/neuraphage/tasks/{task_id}/conversation/                   │ │
│  │   └─ Git-backed conversation history (messages.json)                   │ │
│  │                                                                         │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                                                              │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                    KNOWLEDGE STORE (extends engram)                     │ │
│  │                                                                         │ │
│  │   engram items with label:"learning" for extracted learnings           │ │
│  │   Cross-task knowledge injection via query_relevant()                  │ │
│  │                                                                         │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Data Model Mapping

| Engram | Neuraphage | Notes |
|--------|------------|-------|
| `Item.id` | `Task.id` | Use engram's `eg-{hash}` ID format |
| `Item.title` | `Task.description` | Brief task description |
| `Item.description` | `Task.context` | Extended context, user prompt |
| `Item.status` | `Task.status` | Extended (see below) |
| `Item.priority` | `Task.priority` | 0=critical, 4=low |
| `Item.labels` | `Task.tags` | Categorization + metadata encoding |
| `Edge(Blocks)` | Task dependencies | Direct mapping |

### Status Extension

Engram has 4 statuses; neuraphage needs more granularity:

```
Engram Status    Neuraphage Status (encoded via labels)
─────────────    ─────────────────────────────────────
Open          →  Queued, WaitingForUser (label: "waiting:{reason}")
InProgress    →  Running (label: "phase:{phase}")
Blocked       →  Blocked (blocked by engram edge)
Closed        →  Completed, Failed, Cancelled (label: "outcome:{type}")
```

### Storage Layout (Updated)

```
~/.config/neuraphage/
├── .engram/                  # Engram task graph storage
│   ├── items.jsonl           # Task metadata (append-only)
│   ├── edges.jsonl           # Dependencies (append-only)
│   └── cache.db              # SQLite query cache
├── neuraphage.yaml           # User configuration
├── api-keys.yaml             # Multi-key configuration (gitignored)
├── tasks/
│   └── {task_id}/
│       ├── conversation/     # Git repo for conversation history
│       │   ├── .git/
│       │   └── messages.json
│       ├── worktree/         # Git worktree for this task (if applicable)
│       └── outputs/          # Large tool outputs
├── knowledge/
│   └── learnings.jsonl       # Extracted learnings (or in engram with labels)
└── logs/
    └── daemon.log
```

---

## Implementation Plan

### Phase 1: Foundation

1. **engram integration** — Add engram as workspace dependency
2. **Task manager** — Wrap engram::Store with neuraphage-specific logic
3. **Daemon skeleton** — Basic tokio daemon with Unix socket
4. **CLI skeleton** — neuraphage CLI with basic commands

### Phase 2: Agentic Loop

1. **Loop implementation** — Core agentic loop per [Agentic Loop](./neuraphage-agentic-loop.md)
2. **Tool executor** — Built-in tools (Read, Write, Edit, Bash, Glob, Grep)
3. **LLM client** — Anthropic API integration with rate limiting
4. **Conversation storage** — Git-backed conversation persistence

### Phase 3: Multi-Task Coordination

1. **Scheduler** — Priority queue, rate limit management
2. **Lock manager** — Resource locking with deadlock prevention
3. **Event bus** — SQLite-backed event queue
4. **Knowledge store** — Learning extraction and injection

### Phase 4: User Interface

1. **TUI** — ratatui dashboard per [Infrastructure](./neuraphage-infrastructure.md)
2. **Notifications** — Terminal title, optional TTS
3. **CLI polish** — All CLI commands functional

### Phase 5: Personas and Polish

1. **Personas** — Per [Personas](./neuraphage-personas.md)
2. **Watcher** — Stuck task detection
3. **Syncer** — Cross-task learning relay
4. **Testing** — Comprehensive test coverage

---

## Open Questions

### To Be Resolved

| Question | Context | Options |
|----------|---------|---------|
| **TUI layout specifics** | Proposed layout in [Infrastructure](./neuraphage-infrastructure.md), but details TBD | Pane ratios, resizable, tab support |
| **Voice TTS provider** | Optional notification | Local (piper), cloud (ElevenLabs), system (say/espeak) |
| **Status socket protocol** | For shell/polybar integration | JSON lines, simple text, msgpack |
| **Drift detection thresholds** | When is a task "stuck"? | Tokens without progress, time elapsed, loop iterations |
| **Learning storage format** | Markdown confirmed, but schema? | Frontmatter fields, linking between learnings |
| **Sub-agent communication** | How do parent/child tasks communicate? | Event bus, shared memory, return values only |
| **Cost tracking granularity** | Per-task, per-model, per-day? | All of the above, queryable |
| **Plugin system** | Extend tools, add new capabilities | WASM, dynamic Rust, subprocess |

### Future Enhancements

| Enhancement | Priority | Complexity |
|-------------|----------|------------|
| Warm standby daemon | Medium | High |
| Web UI (alternative to TUI) | Low | Medium |
| Team/multi-user support | Low | High |
| Remote task execution | Low | High |
| VS Code extension | Medium | Medium |
| MCP server integration | Medium | Low |

---

## References

### Inspiration

- **Gas Town** — Steve Yegge's multi-agent orchestrator. Embraces chaos, 20-30 parallel agents with named personas.
  - Article: [Revenge of the Junior Developers](https://steve-yegge.medium.com/revenge-of-the-junior-developers-f4cf67ce8053) (Medium, 2025)
- **KAI/PAI** — Daniel Miessler's disciplined single-agent system. Scaffolding > model philosophy.
- **Claude Code** — Anthropic's agentic CLI. Tool implementations, permission model.

### Technical

- [Tokio](https://tokio.rs/) — Async runtime
- [SQLx](https://github.com/launchbadge/sqlx) — Async SQL
- [Ratatui](https://ratatui.rs/) — Terminal UI
- [Git Worktrees](https://git-scm.com/docs/git-worktree) — Parallel working directories

### Related Documents

- [Multi-Task AI Orchestrator RFC](~/multi-task-ai-orchestrator.md) — Original design document
- [Gas Town vs KAI/PAI Analysis](~/.config/pais/research/tech/gas-town-vs-kai-pai/2026-01-08.md)
- [Orchestrator Analysis](~/.config/pais/research/tech/multi-task-orchestrator-analysis/2026-01-08.md)

---

---

## Review Log

### Pass 1: Completeness (2026-01-10)
- Added Engram Integration section with architecture diagram
- Added Implementation Plan section with 5 phases
- Added engram reference to documentation table
- Updated decision #4 to reference engram explicitly

### Pass 2: Correctness (2026-01-10)
- Fixed data model mismatch between neuraphage Task and engram Item
- Added status mapping table (engram's 4 states → neuraphage's 8 states)
- Clarified storage layout showing engram's .engram/ directory

### Pass 3: Edge Cases (2026-01-10)
- Identified potential race conditions (engram vacuum during operation)
- Noted need for socket permissions in daemon design
- Flagged lock timeout handling in Open Questions

### Pass 4: Architecture (2026-01-10)
- Decided to embed engram::Store directly (not separate daemon)
- Added Knowledge Store concept using engram labels
- Clarified conversation storage is separate from engram

### Pass 5: Clarity (2026-01-10)
- Added data model mapping table
- Added status extension encoding scheme
- Document converged — no significant changes needed

**Final Status:** Document complete after 5 passes.

---

*This document represents decisions made 2026-01-08, updated 2026-01-10. Update as implementation progresses.*
