# Neuraphage: Personas Design

> Part of the [Neuraphage Design Documentation](./neuraphage-design.md)
>
> Related: [Agentic Loop](./neuraphage-agentic-loop.md) | [Infrastructure](./neuraphage-infrastructure.md)

---

## Table of Contents

1. [Why Personas?](#why-personas)
2. [Default Personas](#default-personas)
3. [Persona Selection](#persona-selection)
4. [User Customization](#user-customization)
5. [Persona Hooks](#persona-hooks)
6. [Watcher Integration](#watcher-integration)
7. [Syncer: Cross-Task Learning Relay](#syncer-cross-task-learning-relay)
8. [Multi-Persona Tasks](#multi-persona-tasks)
9. [Comparison with Gas Town](#comparison-with-gas-town)

---

## Why Personas?

Gas Town uses named personas (Whiskey, Tango, Foxtrot, etc.) with distinct personalities and expertise levels. Neuraphage adopts this pattern with sane defaults and full user override capability.

| Benefit | Explanation |
|---------|-------------|
| **Predictable behavior** | Each persona has consistent traits across tasks |
| **Right tool for the job** | Route complex tasks to senior personas, simple tasks to juniors |
| **Parallel perspectives** | Multiple personas can review the same code with different lenses |
| **User preference** | Some users prefer terse experts, others prefer verbose teachers |
| **Cost optimization** | Junior personas can use cheaper models for appropriate tasks |

---

## Default Personas

```yaml
# ~/.config/neuraphage/personas.yaml

personas:
  # Senior engineer - thorough, opinionated, minimal hand-holding
  senior:
    name: Senior
    description: Experienced engineer, concise and direct
    model-tier: expert
    system-prompt: |
      You are a senior software engineer with 15+ years of experience.

      Traits:
      - Direct and concise - no fluff, get to the point
      - Opinionated but open to discussion when challenged
      - Assumes competence - doesn't over-explain basics
      - Focuses on architecture, edge cases, and maintainability
      - Will push back on bad ideas

      Communication style:
      - Short sentences, minimal filler
      - Uses technical terminology without explanation
      - Points out potential issues proactively
      - Suggests alternatives when declining an approach
    auto-assign:
      - complex-reasoning
      - architecture
      - code-review
      - refactoring

  # Mid-level engineer - balanced, explains reasoning
  mid:
    name: Mid
    description: Solid engineer, explains reasoning clearly
    model-tier: worker
    system-prompt: |
      You are a mid-level software engineer with 5-7 years of experience.

      Traits:
      - Balanced between speed and thoroughness
      - Explains reasoning but doesn't belabor points
      - Asks clarifying questions when requirements are ambiguous
      - Good at breaking down problems into steps
      - Collaborative, open to feedback

      Communication style:
      - Clear and structured
      - Brief explanations of "why" alongside "what"
      - Uses examples when helpful
      - Acknowledges trade-offs
    auto-assign:
      - standard-tasks
      - code-generation
      - bug-fixes
      - feature-implementation

  # Junior engineer - verbose, learning-oriented, asks questions
  junior:
    name: Junior
    description: Eager learner, thorough explanations, asks questions
    model-tier: worker
    system-prompt: |
      You are a junior software engineer with 1-2 years of experience.

      Traits:
      - Eager and thorough
      - Explains thinking step-by-step
      - Asks clarifying questions frequently
      - Double-checks assumptions
      - Cautious about making changes without confirmation

      Communication style:
      - Verbose, shows work
      - Explains terminology when using it
      - Asks "did I understand correctly?" style questions
      - Celebrates small wins
    auto-assign:
      - documentation
      - simple-tasks
      - learning-exploration

  # Reviewer - critical eye, security/quality focused
  reviewer:
    name: Reviewer
    description: Critical eye for code review and security
    model-tier: expert
    system-prompt: |
      You are a staff engineer specializing in code review and security.

      Traits:
      - Skeptical by default - looks for problems
      - Security-minded - thinks about attack vectors
      - Performance-aware - spots inefficiencies
      - Standards-focused - enforces consistency
      - Constructive - criticism comes with suggestions

      Communication style:
      - Structured feedback (issues, suggestions, questions)
      - References specific lines/patterns
      - Rates severity (critical, important, nitpick)
      - Explains the "why" behind concerns
    auto-assign:
      - code-review
      - security-audit
      - pr-review

  # Architect - big picture, system design
  architect:
    name: Architect
    description: Systems thinker, designs for scale and maintainability
    model-tier: expert
    system-prompt: |
      You are a software architect with expertise in system design.

      Traits:
      - Thinks in systems, not just code
      - Considers scale, maintainability, and evolution
      - Balances ideal solutions with pragmatic constraints
      - Documents decisions and rationale
      - Identifies dependencies and risks

      Communication style:
      - Uses diagrams and visual representations
      - Presents options with trade-offs
      - References patterns and prior art
      - Thinks out loud about constraints
    auto-assign:
      - architecture
      - system-design
      - technical-planning

  # Router - fast triage, minimal processing
  router:
    name: Router
    description: Fast classifier for task routing
    model-tier: router
    system-prompt: |
      You are a task router. Your job is to quickly classify tasks.

      Respond ONLY with a JSON object:
      {
        "category": "<category>",
        "complexity": "low|medium|high",
        "suggested-persona": "<persona-name>",
        "tags": ["<tag1>", "<tag2>"]
      }

      Do not explain. Do not elaborate. JSON only.
    internal: true  # Not user-assignable

  # Watcher - monitors tasks, detects stuck states, suggests interventions
  watcher:
    name: Watcher
    description: Monitors running tasks for stuck states and helps unstick them
    model-tier: router
    system-prompt: |
      You are a task watcher. You monitor running tasks for signs of trouble.

      You receive periodic snapshots of task state:
      - Current iteration count
      - Recent tool calls and results
      - Token usage
      - Time since last meaningful progress
      - Recent conversation snippets

      Analyze and respond with JSON:
      {
        "status": "healthy|struggling|stuck|runaway",
        "confidence": 0.0-1.0,
        "diagnosis": "<brief explanation>",
        "recommendation": "continue|nudge|escalate|pause|abort",
        "nudge-message": "<optional message to inject into task context>"
      }

      Signs of trouble:
      - Repeating the same tool calls with same results
      - Apologizing repeatedly without making progress
      - Asking the same clarifying question multiple times
      - High token burn with no file changes
      - Tool errors being ignored rather than addressed

      Nudge messages should be brief, specific interventions like:
      - "Try a different approach - the current one isn't working"
      - "The file you're looking for might be in src/utils/ instead"
      - "Stop and ask the user for clarification"
    internal: true
    run-interval: 30s  # Check each running task every 30 seconds
```

---

## Persona Selection

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         PERSONA SELECTION FLOW                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. USER EXPLICIT ASSIGNMENT                                                │
│     │                                                                       │
│     ├─ neuraphage task new --persona=senior "refactor auth module"          │
│     │                                                                       │
│     └─ If user specifies persona → use it, skip auto-routing                │
│                                                                             │
│  2. AUTO-ROUTING (if no explicit persona)                                   │
│     │                                                                       │
│     ├─ Send task description to Router persona                              │
│     │                                                                       │
│     ├─ Router returns:                                                      │
│     │   {                                                                   │
│     │     "category": "refactoring",                                        │
│     │     "complexity": "high",                                             │
│     │     "suggested-persona": "senior",                                    │
│     │     "tags": ["auth", "security", "breaking-change"]                   │
│     │   }                                                                   │
│     │                                                                       │
│     ├─ Match suggested-persona against persona.auto-assign lists            │
│     │                                                                       │
│     └─ Select best match                                                    │
│                                                                             │
│  3. FALLBACK                                                                │
│     │                                                                       │
│     └─ If no match → use task-defaults.default-persona (default: "mid")    │
│                                                                             │
│  4. RUNTIME ESCALATION                                                      │
│     │                                                                       │
│     ├─ If task struggles (drift detection, repeated failures):              │
│     │   └─ Escalate: junior → mid → senior → architect                     │
│     │                                                                       │
│     └─ Hook: PersonaEscalated for user visibility                           │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## User Customization

Users can override everything:

```yaml
# ~/.config/neuraphage/personas.yaml (user overrides)

personas:
  # Override default senior to be more verbose
  senior:
    system-prompt: |
      You are a senior software engineer.
      [user's custom prompt...]

  # Add a completely custom persona
  chaos-gremlin:
    name: Chaos Gremlin
    description: Chaotic agent for stress testing and edge cases
    model-tier: worker
    system-prompt: |
      You are a chaos engineering specialist.

      Traits:
      - Actively looks for ways things can break
      - Suggests edge cases and failure modes
      - Proposes chaos experiments
      - Questions assumptions about reliability

      Your job is to make systems more resilient by finding weaknesses.
    auto-assign:
      - chaos-engineering
      - resilience-testing

  # Disable a default persona
  junior:
    enabled: false

task-defaults:
  default-persona: mid
  allow-escalation: true
  escalation-chain: [junior, mid, senior, architect]
```

---

## Persona Hooks

```
PERSONA HOOKS
├─ PersonaSelected       ─► Persona chosen for task (can override)
├─ PersonaEscalated      ─► Task escalated to higher persona
├─ PersonaPromptAssembly ─► Before persona prompt merged into system prompt
├─ PersonaSwitch         ─► Mid-task persona change (rare, user-initiated)
│
WATCHER HOOKS
├─ WatcherCheck          ─► Watcher evaluated a task (observe diagnosis)
├─ WatcherNudge          ─► Watcher injecting nudge message (can modify/block)
├─ WatcherEscalate       ─► Watcher triggering persona escalation
├─ WatcherPause          ─► Watcher pausing a runaway task
└─ WatcherAbort          ─► Watcher aborting a hopeless task
```

---

## Watcher Integration

The Watcher runs as a background loop, separate from task execution:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           WATCHER LOOP                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Every 30 seconds (configurable):                                           │
│                                                                             │
│  1. COLLECT SNAPSHOTS                                                       │
│     │                                                                       │
│     └─ For each task with status = Running:                                 │
│         ├─ iteration_count                                                  │
│         ├─ tokens_used_this_session                                         │
│         ├─ last_5_tool_calls (name, result_summary, duration)               │
│         ├─ last_meaningful_change (file write, user response)               │
│         ├─ time_since_last_progress                                         │
│         └─ recent_conversation_snippet (last 500 tokens)                    │
│                                                                             │
│  2. EVALUATE EACH TASK                                                      │
│     │                                                                       │
│     ├─ Send snapshot to Watcher persona (cheap/fast model)                  │
│     │                                                                       │
│     ├─► [HOOK: WatcherCheck] with diagnosis                                 │
│     │                                                                       │
│     └─ Parse response:                                                      │
│         {                                                                   │
│           "status": "stuck",                                                │
│           "confidence": 0.85,                                               │
│           "diagnosis": "Repeating glob search for non-existent file",       │
│           "recommendation": "nudge",                                        │
│           "nudge-message": "The file doesn't exist. Ask user where it is."  │
│         }                                                                   │
│                                                                             │
│  3. ACT ON RECOMMENDATION                                                   │
│     │                                                                       │
│     ├─ "continue" → no action                                               │
│     │                                                                       │
│     ├─ "nudge" →                                                            │
│     │   ├─► [HOOK: WatcherNudge] (can modify message)                       │
│     │   └─ Inject system message into task's next context assembly:         │
│     │       "<system-nudge>The file doesn't exist. Ask user...</system-nudge>" │
│     │                                                                       │
│     ├─ "escalate" →                                                         │
│     │   ├─► [HOOK: WatcherEscalate]                                         │
│     │   └─ Trigger persona escalation (junior → mid → senior)               │
│     │                                                                       │
│     ├─ "pause" →                                                            │
│     │   ├─► [HOOK: WatcherPause]                                            │
│     │   ├─ Set task status = Paused                                         │
│     │   └─ Notify user: "Task paused by Watcher: {diagnosis}"               │
│     │                                                                       │
│     └─ "abort" →                                                            │
│         ├─► [HOOK: WatcherAbort]                                            │
│         ├─ Set task status = Failed { error: "Aborted by Watcher" }         │
│         └─ Notify user: "Task aborted: {diagnosis}"                         │
│                                                                             │
│  4. LOG & LEARN                                                             │
│     │                                                                       │
│     └─ Record Watcher interventions for analysis:                           │
│         ├─ Which tasks needed help?                                         │
│         ├─ Did nudges work or did escalation follow?                        │
│         └─ Patterns in stuck states (inform persona prompt improvements)    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

The Watcher complements the drift detection in the loop itself:
- **Drift Detection** (in-loop): Hard limits, automatic triggers
- **Watcher** (external): Nuanced analysis, smart interventions

---

## Syncer: Cross-Task Learning Relay

The Watcher monitors individual task health. The **Syncer** monitors the *collective* — when one task learns something, should other running tasks know about it *now*?

This matters most when:
- Multiple tasks work on a large feature (Gas Town swarm pattern)
- Tasks operate in the same codebase area
- One task discovers something that would save another task time/tokens

**Key insight:** Use the existing user message mechanism. Tasks already yield at breakpoints. A sync message is just another queued message.

### Syncer Persona

```yaml
  # Syncer - cross-task learning relay
  syncer:
    name: Syncer
    description: Routes learnings between concurrent tasks in real-time
    model-tier: router
    system-prompt: |
      You are a cross-task coordinator. You monitor concurrent tasks and
      route relevant discoveries between them in real-time.

      You receive:
      - Summary of each running task's purpose and current focus
      - Recent learnings/discoveries from each task
      - Task relationships (same feature, same codebase area, parent/child)

      For each new learning, evaluate:
      1. Which other running tasks might benefit from knowing this NOW?
      2. How urgent is it? (blocking vs nice-to-know)
      3. How to phrase it concisely for the recipient?

      Respond with JSON:
      {
        "learning-source": "<task-id>",
        "learning-summary": "<what was discovered>",
        "recipients": [
          {
            "task-id": "<recipient-task-id>",
            "relevance": "high|medium|low",
            "urgency": "blocking|helpful|fyi",
            "message": "<concise message for this task>"
          }
        ],
        "skip-reason": "<if no recipients, why>"
      }

      Examples of valuable cross-task learnings:
      - "The API endpoint changed from /v1/users to /v2/users"
      - "config.rs panics if the array is empty - add a check"
      - "The UserService is in src/services/, not src/models/"
      - "Don't use the deprecated authenticate() - use verify_token()"

      Be selective. Not every learning needs broadcasting. Only relay
      things that would genuinely help another task right now.
    internal: true
    run-interval: 15s  # More frequent than Watcher - learnings are time-sensitive
```

### Syncer Integration Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           SYNCER LOOP                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Every 15 seconds (configurable):                                           │
│                                                                             │
│  1. COLLECT TASK SUMMARIES                                                  │
│     │                                                                       │
│     └─ For each task with status = Running:                                 │
│         ├─ task_id, description, tags                                       │
│         ├─ current_focus (last tool call, current file)                     │
│         ├─ recent_learnings (from in-flight learning extraction)            │
│         ├─ working_directory / git_branch                                   │
│         └─ relationship_to_other_tasks (same parent, same tags, etc.)       │
│                                                                             │
│  2. IDENTIFY NEW LEARNINGS                                                  │
│     │                                                                       │
│     ├─ Diff against last Syncer run                                         │
│     │                                                                       │
│     └─ New learnings since last check:                                      │
│         - Task A: "Found that deleted users return 404, not 400"            │
│         - Task C: "The config parser is in src/utils/config.rs"             │
│                                                                             │
│  3. EVALUATE EACH LEARNING                                                  │
│     │                                                                       │
│     ├─ Send to Syncer persona with all task contexts                        │
│     │                                                                       │
│     ├─► [HOOK: SyncerEvaluate] with learning + potential recipients         │
│     │                                                                       │
│     └─ Parse response:                                                      │
│         {                                                                   │
│           "learning-source": "task-a",                                      │
│           "learning-summary": "Deleted users return 404, not 400",          │
│           "recipients": [                                                   │
│             {                                                               │
│               "task-id": "task-b",                                          │
│               "relevance": "high",                                          │
│               "urgency": "blocking",                                        │
│               "message": "FYI: The API returns 404 (not 400) for deleted    │
│                          users. Adjust your error handling."                │
│             }                                                               │
│           ]                                                                 │
│         }                                                                   │
│                                                                             │
│  4. QUEUE MESSAGES TO RECIPIENTS                                            │
│     │                                                                       │
│     ├─► [HOOK: SyncerRelay] for each message (can modify/block)             │
│     │                                                                       │
│     └─ For each recipient:                                                  │
│         │                                                                   │
│         ├─ Create sync message:                                             │
│         │   {                                                               │
│         │     role: "user",                                                 │
│         │     content: "<sync from=task-a urgency=blocking>                 │
│         │               FYI: The API returns 404 (not 400) for deleted      │
│         │               users. Adjust your error handling.                  │
│         │               </sync>",                                           │
│         │     metadata: { type: "sync", source_task: "task-a" }             │
│         │   }                                                               │
│         │                                                                   │
│         └─ Queue to task's input channel                                    │
│             └─ Task receives at next yield point (like user messages)       │
│                                                                             │
│  5. TRACK DELIVERY                                                          │
│     │                                                                       │
│     └─ Log: which learnings relayed, to whom, delivery status               │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Recipient Task Handling

When a task resumes from a yield point and has queued messages:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  Task B resumes from yield                                                   │
│      │                                                                       │
│      ▼                                                                       │
│  Check input queue:                                                          │
│      │                                                                       │
│      ├─ User message? → Process as conversation input                        │
│      │                                                                       │
│      ├─ Sync message? →                                                      │
│      │   ├─ Inject into context with special formatting:                     │
│      │   │                                                                   │
│      │   │   <sync-message from="task-a" urgency="blocking">                 │
│      │   │   FYI: The API returns 404 (not 400) for deleted users.           │
│      │   │   Adjust your error handling.                                     │
│      │   │   </sync-message>                                                 │
│      │   │                                                                   │
│      │   └─ LLM sees this and can adjust approach                            │
│      │                                                                       │
│      └─ Multiple messages? → Batch them, ordered by urgency                  │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Syncer Hooks

```
SYNCER HOOKS
├─ SyncerEvaluate   ─► Learning being evaluated for relay (observe/modify)
├─ SyncerRelay      ─► Message about to be queued (can modify/block)
├─ SyncerDelivered  ─► Message delivered to recipient task
└─ SyncerIgnored    ─► Learning evaluated but not relayed (for analysis)
```

### Comparison: Watcher vs Syncer

| Aspect | Watcher | Syncer |
|--------|---------|--------|
| **Focus** | Individual task health | Cross-task intelligence |
| **Question** | "Is this task stuck?" | "Should other tasks know this?" |
| **Trigger** | Time-based (every 30s) | Learning-based + time (every 15s) |
| **Action** | Nudge, escalate, pause | Relay message to other tasks |
| **Recipient** | Same task | Other running tasks |
| **Urgency** | Reactive (fix problems) | Proactive (share discoveries) |

### When Syncer Shines

```yaml
# Scenario: Three tasks working on auth feature

tasks:
  - id: task-auth-api
    description: "Implement JWT token validation endpoint"
    tags: [auth, api, jwt]

  - id: task-auth-ui
    description: "Build login form with token refresh"
    tags: [auth, ui, jwt]

  - id: task-auth-tests
    description: "Write integration tests for auth flow"
    tags: [auth, tests, jwt]

# Task task-auth-api discovers:
# "The JWT library requires tokens to have 'iat' claim, throws if missing"

# Syncer evaluates:
# - task-auth-ui: HIGH relevance (generating tokens in UI refresh)
# - task-auth-tests: HIGH relevance (test fixtures need valid tokens)

# Both receive sync message before they hit the same issue
```

### Syncer Configuration

```yaml
# ~/.config/neuraphage/neuraphage.yaml

syncer:
  enabled: true
  run-interval: 15s

  # Only sync between related tasks
  relationship-filter:
    - same-parent          # Subtasks of same parent
    - shared-tags          # Tasks with overlapping tags
    - same-directory       # Tasks in same git worktree/directory
    - explicit-group       # User-defined task groups

  # Urgency thresholds
  relay-threshold:
    blocking: always       # Always relay blocking discoveries
    helpful: related-only  # Only to related tasks
    fyi: same-parent-only  # Only to sibling subtasks

  # Prevent message storms
  rate-limit:
    max-messages-per-task-per-minute: 5
    cooldown-after-relay: 10s  # Don't re-evaluate same learning
```

---

## Multi-Persona Tasks

Some tasks benefit from multiple personas:

```yaml
# Task configuration
task:
  description: "Implement new payment processing module"
  personas:
    primary: architect      # Does the main work
    reviewers: [reviewer, senior]  # Review before completion

  workflow:
    - phase: design
      persona: architect
    - phase: implement
      persona: mid
    - phase: review
      personas: [reviewer, senior]
      mode: parallel        # Both review simultaneously
    - phase: revise
      persona: mid
```

---

## Comparison with Gas Town

| Aspect | Gas Town | Neuraphage |
|--------|----------|------------|
| **Naming** | Phonetic alphabet (Whiskey, Tango) | Role-based (senior, reviewer) |
| **Personality** | Strong personalities, humor | Professional, configurable |
| **Count** | 20-30 agents | 5-6 defaults, unlimited custom |
| **Coordination** | Emergent chaos | Structured routing |
| **Customization** | Limited | Full override |

Gas Town's charm is in the chaos and personality. Neuraphage takes the useful parts (specialized behaviors, expertise levels) while maintaining the disciplined approach.

---

*This document is part of the [Neuraphage Design Documentation](./neuraphage-design.md).*
