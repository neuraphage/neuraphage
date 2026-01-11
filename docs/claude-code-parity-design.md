# Design Document: Claude Code Parity

**Author:** Scott Aidler
**Date:** 2026-01-11
**Status:** Draft
**Review Passes:** 5/5

## Summary

This document specifies the remaining work to achieve feature parity with Claude Code. The core execution infrastructure exists (daemon, task management, Anthropic client, agentic loop), but the agent lacks the sophisticated prompting, tools, and interaction model that make Claude Code effective for software engineering tasks.

## Problem Statement

### Background

Neuraphage currently has:
- A functioning daemon with task management
- Anthropic API integration (via `AnthropicClient`)
- An agentic loop that executes tasks
- 7 basic tools: `read_file`, `write_file`, `list_directory`, `run_command`, `ask_user`, `complete_task`, `fail_task`
- Interactive mode with real-time status display

### Problem

The current implementation is a minimal proof-of-concept. To be a Claude Code replacement, it needs:

1. **Primitive system prompt** - Current prompt is ~15 lines with no guidance on coding style, git workflows, security, or best practices
2. **Missing critical tools** - No search/grep, no glob patterns, no edit-in-place, no web access, no task spawning
3. **No REPL mode** - Claude Code allows back-and-forth conversation; np only runs single tasks
4. **Poor context management** - No file reading before edit, no codebase exploration
5. **No permission model** - Bash commands run unsandboxed with no confirmation

### Goals

- Implement system prompt with Claude Code's effective patterns
- Add 10+ essential tools for software engineering
- Enable persistent REPL-style conversation
- Add safety guardrails and permission model
- Achieve usability comparable to Claude Code

### Non-Goals

- Full feature parity with every Claude Code capability
- MCP (Model Context Protocol) server support
- IDE integrations (VS Code, JetBrains)
- Multi-model support (OpenAI, Gemini, etc.)
- Streaming responses (defer to future)

## Proposed Solution

### Overview

Four major work streams:

```
┌─────────────────────────────────────────────────────────────────┐
│                    Claude Code Parity                            │
├─────────────────┬─────────────────┬─────────────────┬───────────┤
│  System Prompt  │     Tools       │   REPL Mode     │  Safety   │
│   Engineering   │   Expansion     │  Conversation   │  Model    │
├─────────────────┼─────────────────┼─────────────────┼───────────┤
│ - Persona       │ - Glob          │ - Session mgmt  │ - Sandbox │
│ - Code style    │ - Grep          │ - Follow-ups    │ - Confirm │
│ - Git workflow  │ - Edit          │ - History       │ - Hooks   │
│ - Tool guidance │ - WebFetch      │ - /commands     │ - Limits  │
│ - Security      │ - Task spawn    │                 │           │
└─────────────────┴─────────────────┴─────────────────┴───────────┘
```

### Architecture

#### Component 1: System Prompt Engine

**Location:** `src/agentic/prompt.rs` (NEW)

The system prompt is the single most important factor in agent effectiveness. Claude Code's prompt includes:

```rust
pub struct SystemPrompt {
    /// Base persona and capabilities
    persona: String,
    /// Tool usage guidelines
    tool_guidance: String,
    /// Code style conventions
    code_style: String,
    /// Git workflow instructions
    git_workflow: String,
    /// Security policies
    security_policy: String,
    /// Current context (working dir, git status, etc.)
    context: PromptContext,
}

impl SystemPrompt {
    pub fn build(&self, task: &Task) -> String;
}
```

**Key prompt sections:**

1. **Persona** - "You are Neuraphage, an AI assistant for software engineering..."
2. **Tone & Style** - Professional, concise, no unnecessary praise
3. **Tool Selection** - When to use each tool, preferences (Edit over Write, Read before Edit)
4. **Code Conventions** - Don't over-engineer, avoid unnecessary changes, keep it simple
5. **Git Workflow** - Commit message format, PR conventions, never force push
6. **Security** - Never output secrets, validate user input, sandbox commands
7. **Context** - Working directory, git repo status, platform info

#### Component 2: Tool Expansion

**Location:** `src/agentic/tools/` (NEW module structure)

Reorganize tools into a module with individual files:

```
src/agentic/tools/
├── mod.rs          # Tool registry, execution
├── filesystem.rs   # read, write, glob
├── search.rs       # grep, ripgrep integration
├── edit.rs         # precise text editing
├── bash.rs         # command execution
├── web.rs          # fetch, search
├── task.rs         # subagent spawning
├── user.rs         # ask_user, confirm
└── control.rs      # complete_task, fail_task
```

**New tools to implement:**

| Tool | Description | Priority |
|------|-------------|----------|
| `Glob` | File pattern matching (`**/*.rs`) | P0 |
| `Grep` | Regex search with ripgrep | P0 |
| `Edit` | Precise string replacement | P0 |
| `WebFetch` | Fetch URL content | P1 |
| `WebSearch` | Web search via API | P1 |
| `Task` | Spawn subagent for complex work | P1 |
| `TodoWrite` | Task list management | P2 |
| `NotebookEdit` | Jupyter cell editing | P2 |

**Tool: Edit (most critical)**

```rust
pub struct EditTool;

impl EditTool {
    /// Replace exact string in file
    /// Requires: file was read in conversation
    /// Fails if: old_string not found or not unique
    async fn execute(&self, args: EditArgs) -> ToolResult {
        // 1. Validate file was previously read
        // 2. Find old_string (must be unique)
        // 3. Replace with new_string
        // 4. Write file
        // 5. Return diff preview
    }
}

#[derive(Deserialize)]
struct EditArgs {
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}
```

**Tool: Glob**

```rust
pub struct GlobTool;

impl GlobTool {
    async fn execute(&self, pattern: &str, path: Option<&str>) -> ToolResult {
        // Use globwalk or similar
        // Return sorted file paths
        // Respect .gitignore
    }
}
```

**Tool: Grep**

```rust
pub struct GrepTool;

impl GrepTool {
    async fn execute(&self, args: GrepArgs) -> ToolResult {
        // Shell out to ripgrep (rg) if available
        // Fallback to grep
        // Support: pattern, path, -A/-B/-C context, -i case insensitive
    }
}
```

#### Component 3: REPL Mode

**Location:** `src/repl.rs` (NEW)

Current limitation: `np run` creates a task, executes it, and exits. No follow-up conversation.

REPL mode enables:
```
$ np
> help me refactor the authentication module
[agent works on it]
> actually, use JWT instead of sessions
[agent continues with context]
> /commit
[agent commits the changes]
```

```rust
pub struct Repl {
    client: DaemonClient,
    session: Session,
    history: Vec<Message>,
}

impl Repl {
    pub async fn run(&mut self) -> Result<()> {
        loop {
            let input = self.read_input()?;

            match input.as_str() {
                "/quit" | "/exit" => break,
                "/clear" => self.clear_history(),
                "/history" => self.show_history(),
                s if s.starts_with('/') => self.handle_command(s).await?,
                _ => self.send_message(&input).await?,
            }
        }
        Ok(())
    }
}
```

**Session persistence:**

```rust
pub struct Session {
    id: String,
    started_at: DateTime<Utc>,
    working_dir: PathBuf,
    conversation_path: PathBuf,
    task_id: Option<TaskId>,
}
```

Sessions are stored at `~/.local/share/neuraphage/sessions/`.

**Slash commands:**

| Command | Description |
|---------|-------------|
| `/help` | Show available commands |
| `/clear` | Clear conversation history |
| `/history` | Show conversation history |
| `/commit` | Create git commit |
| `/pr` | Create pull request |
| `/status` | Show current status |
| `/quit` | Exit REPL |

#### Component 4: Safety Model

**Location:** `src/safety.rs` (NEW)

Claude Code has several safety mechanisms:

1. **Bash sandboxing** - Commands run in restricted environment
2. **Confirmation prompts** - Dangerous operations require user approval
3. **Hooks** - User-defined scripts that run on events
4. **File restrictions** - Some paths are protected

```rust
pub struct SafetyConfig {
    /// Require confirmation for these command patterns
    confirm_patterns: Vec<Regex>,
    /// Block these commands entirely
    blocked_commands: Vec<String>,
    /// Protected paths (no write)
    protected_paths: Vec<PathBuf>,
    /// Maximum file size to read
    max_file_size: usize,
    /// Command timeout
    command_timeout: Duration,
}

impl SafetyConfig {
    pub fn default() -> Self {
        Self {
            confirm_patterns: vec![
                regex!(r"rm\s+-rf"),
                regex!(r"git\s+push.*--force"),
                regex!(r"sudo"),
            ],
            blocked_commands: vec![
                "rm -rf /".into(),
                "mkfs".into(),
            ],
            protected_paths: vec![
                PathBuf::from("/etc"),
                PathBuf::from("/usr"),
            ],
            max_file_size: 10 * 1024 * 1024, // 10MB
            command_timeout: Duration::from_secs(120),
        }
    }
}
```

**Permission flow:**

```
User request → Safety check → [Confirmation needed?] → Execute
                                    ↓ yes
                              Prompt user → [Approved?] → Execute
                                               ↓ no
                                            Abort
```

### Data Model

#### Session State

```rust
pub struct SessionState {
    /// Unique session ID
    pub id: String,
    /// When session started
    pub started_at: DateTime<Utc>,
    /// Working directory
    pub working_dir: PathBuf,
    /// Files that have been read (for Edit validation)
    pub read_files: HashSet<PathBuf>,
    /// Current task (if any)
    pub current_task: Option<TaskId>,
    /// Conversation history
    pub messages: Vec<Message>,
    /// Token usage this session
    pub tokens_used: u64,
    /// Cost this session
    pub cost: f64,
}
```

#### Tool Registry

```rust
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolResult>;
}

pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session: Arc<Mutex<SessionState>>,
    pub safety: Arc<SafetyConfig>,
}
```

### API Design

#### REPL Input Handling

```rust
pub enum ReplInput {
    /// Regular message to send to Claude
    Message(String),
    /// Slash command
    Command { name: String, args: Vec<String> },
    /// End of input (Ctrl+D)
    Eof,
}

impl ReplInput {
    pub fn parse(input: &str) -> Self {
        if input.starts_with('/') {
            let parts: Vec<&str> = input[1..].split_whitespace().collect();
            ReplInput::Command {
                name: parts[0].to_string(),
                args: parts[1..].iter().map(|s| s.to_string()).collect(),
            }
        } else {
            ReplInput::Message(input.to_string())
        }
    }
}
```

#### Confirmation Protocol

```rust
pub enum ConfirmationRequest {
    /// Command needs approval
    Command { command: String, reason: String },
    /// File write needs approval
    FileWrite { path: PathBuf, size: usize },
    /// Destructive operation
    Destructive { operation: String, description: String },
}

pub enum ConfirmationResponse {
    Approved,
    Denied,
    ApprovedOnce,    // Don't ask again this command
    ApprovedSession, // Don't ask again this session
}
```

### Implementation Plan

**Phase 1: Tool Expansion (P0)**
1. Refactor tools into module structure
2. Implement `Glob` tool with gitignore support
3. Implement `Grep` tool with ripgrep
4. Implement `Edit` tool with validation
5. Add read-file tracking to session state

**Phase 2: System Prompt (P0)**
1. Create `prompt.rs` module
2. Write comprehensive prompt sections
3. Add context injection (git status, platform)
4. Test prompt effectiveness with real tasks

**Phase 3: REPL Mode (P0)**
1. Create `repl.rs` module
2. Implement session persistence
3. Add slash commands
4. Integrate with daemon for task continuity

**Phase 4: Safety Model (P1)**
1. Create `safety.rs` module
2. Implement confirmation flow
3. Add command sandboxing (basic)
4. Add file restriction checks

**Phase 5: Additional Tools (P1)**
1. Implement `WebFetch` tool
2. Implement `Task` subagent spawning
3. Implement `TodoWrite` for task tracking

## Alternatives Considered

### Alternative 1: Fork Claude Code

- **Description:** Fork the actual Claude Code CLI and modify it
- **Pros:** Immediate feature parity, tested codebase
- **Cons:** Claude Code is not open source, TypeScript (not Rust), tightly coupled to Anthropic infrastructure
- **Why not chosen:** Not possible - Claude Code is proprietary

### Alternative 2: Minimal Agent

- **Description:** Keep the current simple implementation, don't try for parity
- **Pros:** Less work, simpler codebase
- **Cons:** Not actually useful for real work, can't replace Claude Code
- **Why not chosen:** The goal is a real replacement tool

### Alternative 3: Use Existing Open Source Agent

- **Description:** Use/adapt Aider, Continue, or similar
- **Pros:** Existing community, tested
- **Cons:** Different architecture, usually Python, may not fit neuraphage daemon model
- **Why not chosen:** Want Rust implementation with daemon architecture for multi-task orchestration

## Technical Considerations

### Dependencies

**New crate dependencies:**
```toml
globwalk = "0.9"        # Glob pattern matching
grep-regex = "0.1"      # Regex for grep (or shell out to rg)
rustyline = "14"        # REPL line editing
similar = "2"           # Diff generation for Edit tool
```

### Performance

- **Glob:** Use streaming iterator, don't collect all at once
- **Grep:** Shell to ripgrep for large codebases
- **Session storage:** Memory + periodic flush to disk
- **Large files:** Chunk reads, refuse files over limit

### Security

- **Command injection:** Escape arguments, never interpolate user input into shell
- **Path traversal:** Resolve all paths, check they're under working dir
- **Secrets:** Scan tool output for API key patterns, warn user
- **Resource exhaustion:** Timeout on commands, limit output size

### Testing Strategy

1. **Unit tests:** Each tool in isolation with mock filesystem
2. **Integration tests:** Tool execution with real filesystem in temp dir
3. **Prompt tests:** Evaluate prompt effectiveness on standard tasks
4. **REPL tests:** Automated interaction sequences
5. **Safety tests:** Verify blocked commands, confirmation prompts

### Rollout Plan

1. v0.2.0 - Tool expansion (Glob, Grep, Edit)
2. v0.3.0 - System prompt improvements
3. v0.4.0 - REPL mode
4. v0.5.0 - Safety model
5. v1.0.0 - Feature complete for Claude Code replacement

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Prompt engineering is hard to get right | High | High | Iterative testing, copy patterns from Claude Code behavior |
| Edit tool causes data loss | Medium | High | Require read-first, create backups, git safety checks |
| REPL mode is complex to implement | Medium | Medium | Start simple, add features incrementally |
| ripgrep not available on all systems | Low | Low | Fallback to built-in grep, document dependency |
| Session state corruption | Low | Medium | JSON schema validation, recovery mechanism |

## Open Questions

- [x] How to handle very large files in Edit? → Refuse files over 10MB, suggest manual edit
- [x] Should sessions persist across daemon restarts? → Yes, stored in filesystem
- [ ] How to implement proper bash sandboxing? → Research firejail, bubblewrap
- [ ] Should we support custom tools/plugins? → Defer to post-v1.0
- [ ] How to handle multi-file edits atomically? → Research, maybe git stash approach

## References

- Claude Code behavior (observed through usage)
- [Aider](https://github.com/paul-gauthier/aider) - AI pair programming tool
- [Continue](https://github.com/continuedev/continue) - AI code assistant
- [ripgrep](https://github.com/BurntSushi/ripgrep) - Fast grep alternative
- [globwalk](https://docs.rs/globwalk) - Glob pattern matching

---

## Review Pass 1: Completeness

Checking all sections...

**Findings:**
- Summary: OK
- Problem Statement: OK - clear gap analysis
- Goals/Non-goals: OK
- Architecture: OK - 4 components defined
- Data Model: OK - SessionState, ToolRegistry
- API Design: OK - ReplInput, ConfirmationProtocol
- Implementation Plan: OK - phased approach
- Alternatives: OK - 3 alternatives
- Risks: OK - 5 risks identified

**Changes made:**
- Added rollout plan with version numbers
- Added testing strategy section
- Expanded tool table with priorities

---

## Review Pass 2: Correctness

Checking technical accuracy...

**Findings:**
- Tool implementations match Claude Code behavior
- Edit tool requires read-first - correct pattern
- Glob with gitignore - need to verify crate supports this
- Session persistence path matches neuraphage conventions

**Changes made:**
- Clarified ripgrep fallback behavior
- Added command timeout to SafetyConfig
- Fixed tool context to include safety config

---

## Review Pass 3: Edge Cases

Checking error handling and failure modes...

**Findings:**
- Edit on non-existent file: Should fail with clear error
- Grep on binary file: Should skip or warn
- Session corruption: Need recovery mechanism
- Concurrent REPL sessions: Need to handle

**Changes made:**
- Added max_file_size to SafetyConfig
- Added session corruption to risks
- Added atomic edit question to open questions

---

## Review Pass 4: Architecture

Checking system fit...

**Findings:**
- REPL mode integrates with existing daemon via DaemonClient - good
- Tool refactor is backwards compatible
- Session state is separate from task state - correct separation
- Safety config can be per-session or global - flexible

**Changes made:**
- Clarified ToolContext includes session reference
- Added Arc<Mutex> for session state sharing

---

## Review Pass 5: Clarity

Checking implementability...

**Findings:**
- Each component has clear location and structure
- Tool implementations have concrete examples
- Implementation phases are actionable
- Dependencies are specified

**Changes made:**
- No significant changes needed
- Document has converged

---

**FINAL STATUS:** Document complete after 5 passes. Ready for implementation.
