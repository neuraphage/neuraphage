# Architectural Evaluation: Engram + Neuraphage vs Beads + Gas Town

**Author:** Scott A. Idler
**Date:** 2026-01-11
**Status:** Commentary

## Executive Summary

**We did not abandon graphing capabilities.** What we did was separate concerns properly. Engram provides graph primitives (Items, Edges, Events) while neuraphage provides semantic interpretation and orchestration.

## Graph Capabilities in Engram

Engram has a full DAG (Directed Acyclic Graph) implementation:

```rust
// engram/src/types.rs
pub enum EdgeKind {
    Blocks,        // to_id blocks from_id (dependency semantics)
    ParentChild,   // hierarchical containment
    Related,       // informational links
}
```

Plus:
- **Cycle detection** - DFS traversal prevents circular dependencies
- **Ready/Blocked queries** - SQLite-backed queries find actionable items
- **Graph traversal** via `get_blocking_edges_from()`

This is the same underlying data model as Beads - nodes (Items) and edges (relationships). The difference is **semantic layering**.

## What Beads Has That Engram Doesn't (By Design)

Beads is tightly coupled to Gas Town's **MEOW stack**:
- **Epics** - High-level goals
- **Molecules** - Atomic units of work
- **Protomolecules** - Work in progress
- **Formulas** - Execution recipes

These are **semantic abstractions baked into the storage layer**. Engram intentionally doesn't have these because:

1. **Decoupling** - Engram is usable outside neuraphage
2. **Flexibility** - Different orchestrators may need different semantics
3. **Simplicity** - Items + Edges + Events is a minimal correct model

## Where the Semantics Live Now

In neuraphage, the semantic layer lives in the orchestrator, not the storage:

| Gas Town Concept | Neuraphage Equivalent |
|------------------|----------------------|
| Epic | Task with `EdgeKind::ParentChild` children |
| Molecule | Single Task execution |
| Protomolecule | Task with status `InProgress` |
| Formula | Prompt + context in TaskInfo |
| GUPP (propulsion) | EventBus pub/sub |

## Architecture Comparison

### Gas Town Architecture

```
┌─────────────────────────────────────────┐
│ tmux (UI/session management)            │
├─────────────────────────────────────────┤
│ Mayor (coordination)                    │
│ Polecats (worker processes)             │
│ Witness (observability)                 │
│ Dogs (main tracking)                    │
│ Deacon (learning)                       │
├─────────────────────────────────────────┤
│ Beads (git+json storage)                │
└─────────────────────────────────────────┘
```

### Neuraphage Architecture

```
┌─────────────────────────────────────────┐
│ CLI (np) / daemon socket                │
├─────────────────────────────────────────┤
│ Daemon (tokio runtime)                  │
│   ├── SupervisedExecutor (orchestration)│
│   ├── EventBus (pub/sub coordination)   │
│   ├── Watcher (stuck detection)         │
│   ├── Syncer (learning extraction)      │
│   └── MainWatcher (rebase notifications)│
├─────────────────────────────────────────┤
│ Engram Store (git+sqlite storage)       │
└─────────────────────────────────────────┘
```

## Key Architectural Advantages

1. **Single runtime** - tokio provides async concurrency without process explosion
2. **Type-safe coordination** - EventBus with Rust enums vs bash scripts
3. **Crash recovery** - checkpoints + idempotent tools
4. **Budget enforcement** - CostTracker integrated at daemon level
5. **Decoupled storage** - engram is a standalone crate with its own daemon

## What Gas Town Does Better (Currently)

1. **tmux UI** - Visual session management. We have `np attach` but it's simpler.
2. **Convoys** - Git branch coordination across repos. We have MainWatcher for single-repo.
3. **Chaotic parallelism** - Gas Town runs many agents simultaneously. We're more conservative.

## What Neuraphage Does Better

1. **Disciplined coordination** - Supervision prevents runaway tasks
2. **Event persistence** - Full audit trail in engram
3. **Budget controls** - Hard stops when spending exceeds limits
4. **Clean separation** - engram is reusable, neuraphage is the orchestrator
5. **Single binary** - No tmux/process management complexity

## Component Mapping

| Gas Town | Neuraphage | Notes |
|----------|------------|-------|
| Mayor | SupervisedExecutor | Task orchestration and lifecycle |
| Polecats | tokio tasks | Parallel execution units |
| Witness | Watcher | Observability and stuck detection |
| Deacon | Syncer | Learning extraction across tasks |
| Dogs | MainWatcher | Main branch tracking and rebase |
| GUPP | EventBus | Pub/sub coordination mechanism |
| Beads | Engram | Git-backed persistent storage |
| Formulas | TaskInfo prompts | Execution instructions |

## The Bottom Line

We didn't abandon graphing. We **separated storage (engram) from semantics (neuraphage)**.

Beads bakes Gas Town concepts into the data model. That works for Steve's use case but makes Beads useless outside Gas Town.

Engram provides primitives (Items, Edges, Events) that neuraphage interprets. If MEOW-style semantics were needed, they could be added as:
- Labels (`epic`, `molecule`)
- Edge kinds (already have `Blocks`, `ParentChild`, `Related`)
- Events (`learning_extracted`, `sync_relayed`)

The daemon + tokio + EventBus design provides the coordination layer Gas Town builds with bash/tmux, but with type safety and a single runtime.

**This is Gas Town's functionality without Gas Town's complexity.** The tradeoff is we're in the "disciplined multi-task" quadrant instead of "chaotic multi-task". That's a feature, not a bug.

## References

- `docs/yegge/welcome-to-gas-town.md` - Steve Yegge's Gas Town architecture
- `docs/neuraphage-design.md` - Neuraphage design document
- `engram/docs/implementation-plan.md` - Engram implementation details
- `docs/watcher-syncer-integration-design.md` - Supervision system design
