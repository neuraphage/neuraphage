# Neuraphage: Infrastructure Design

> Part of the [Neuraphage Design Documentation](./neuraphage-design.md)
>
> Related: [Agentic Loop](./neuraphage-agentic-loop.md) | [Personas](./neuraphage-personas.md)

---

## Table of Contents

1. [Security Model](#security-model)
2. [Git Worktree Primer](#git-worktree-primer)
3. [TUI Visual Design](#tui-visual-design)

---

## Security Model

### Four Layers

```
┌─────────────────────────────────────────────────────────────────┐
│ LAYER 1: POLICY (default deny)                                  │
│                                                                 │
│ Built-in rules:                                                 │
│   ✗ rm -rf (anywhere)                                          │
│   ✗ Write to /etc, /usr, /System                               │
│   ✗ curl | bash, wget | sh                                     │
│   ✗ chmod 777                                                  │
│   ✗ Arbitrary network connections                              │
│                                                                 │
│ Configured in: neuraphage.yaml (security.policy section)        │
├─────────────────────────────────────────────────────────────────┤
│ LAYER 2: USER PERMISSIONS (allowlist overrides)                 │
│                                                                 │
│ User-defined allows:                                            │
│   ✓ Allow writes to ~/projects/**                              │
│   ✓ Allow bash: docker compose *                               │
│   ✓ Allow bash: cargo *                                        │
│   ✓ Allow network: api.anthropic.com, github.com               │
│                                                                 │
│ Configured in: neuraphage.yaml (security.permissions section)   │
├─────────────────────────────────────────────────────────────────┤
│ LAYER 3: RUNTIME CHECKS                                         │
│                                                                 │
│ Dynamic analysis:                                               │
│   ⚠ Anomaly detection (unusual patterns)                       │
│   ⚠ Rate limiting (too many file writes?)                      │
│   ⚠ Resource consumption monitoring                            │
│   ⚠ Sandbox execution for untrusted operations                 │
│                                                                 │
│ Flags for review, may auto-block                                │
├─────────────────────────────────────────────────────────────────┤
│ LAYER 4: HUMAN CONFIRMATION                                     │
│                                                                 │
│ When uncertain:                                                 │
│   ? Task wants to run: git push --force                        │
│   ? Allow? [y/N/always/never]                                  │
│                                                                 │
│ User decision recorded for future (updates Layer 2)             │
└─────────────────────────────────────────────────────────────────┘
```

### Configuration Example

```yaml
# neuraphage.yaml

security:
  policy:
    # Patterns that are always blocked
    blocked-commands:
      - "rm -rf /"
      - "rm -rf ~"
      - ":(){ :|:& };:"  # fork bomb
      - "chmod 777"
      - "curl * | bash"
      - "wget * | sh"

    blocked-paths:
      - /etc
      - /usr
      - /System
      - /var

    blocked-hosts:
      - "*"  # default deny outbound

  permissions:
    # User overrides
    allowed-paths:
      - ~/projects
      - ~/repos
      - /tmp/neuraphage-*

    allowed-commands:
      - "docker compose *"
      - "cargo *"
      - "npm *"
      - "git *"

    allowed-hosts:
      - api.anthropic.com
      - api.openai.com
      - github.com
      - "*.githubusercontent.com"

  runtime:
    max-file-writes-per-minute: 100
    max-bash-commands-per-minute: 50
    sandbox-unknown-commands: true
```

---

## Git Worktree Primer

Git worktrees allow multiple working directories from a single repository. Perfect for Neuraphage's parallel task execution.

### Basic Commands

```bash
# List existing worktrees
git worktree list

# Create a worktree for a task (new branch)
git worktree add ../task-abc123 -b feature/task-abc123

# Create a worktree on existing branch
git worktree add ../task-def456 existing-branch

# Remove a worktree when task completes
git worktree remove ../task-abc123

# Prune stale worktree references
git worktree prune
```

### How It Works

```
~/repos/myproject/                    # Main worktree
├── .git/                             # Shared git directory
│   ├── objects/                      # Shared objects (commits, blobs)
│   ├── refs/                         # Shared refs
│   └── worktrees/                    # Worktree metadata
│       ├── task-abc123/
│       └── task-def456/
├── src/
└── Cargo.toml

~/repos/.worktrees/task-abc123/       # Task A's worktree
├── .git -> ~/repos/myproject/.git    # Symlink to shared .git
├── src/                              # Task A's working copy
└── Cargo.toml

~/repos/.worktrees/task-def456/       # Task B's worktree
├── .git -> ~/repos/myproject/.git    # Same shared .git
├── src/                              # Task B's working copy (can differ!)
└── Cargo.toml
```

### Benefits for Neuraphage

| Benefit | Explanation |
|---------|-------------|
| **Parallel edits** | Task A edits `src/foo.rs`, Task B edits `src/bar.rs`, no conflicts |
| **Branch isolation** | Each task on its own branch, merge when ready |
| **Shared history** | All worktrees see same commits, can cherry-pick between tasks |
| **Disk efficient** | Objects shared, only working files duplicated |
| **Clean cleanup** | `git worktree remove` when task completes |

### Neuraphage Integration

```rust
use tokio::process::Command;

impl GitCoordinator {
    pub async fn setup_task_worktree(
        &self,
        repo_path: &Path,
        task_id: TaskId,
    ) -> Result<PathBuf> {
        let worktree_base = repo_path.parent().unwrap().join(".worktrees");
        let worktree_path = worktree_base.join(task_id.to_string());
        let branch_name = format!("neuraphage/{}", task_id);

        // Create worktree with new branch (using tokio::process for async)
        Command::new("git")
            .args([
                "worktree", "add",
                worktree_path.to_str().unwrap(),
                "-b", &branch_name,
            ])
            .current_dir(repo_path)
            .output()
            .await?;

        Ok(worktree_path)
    }

    pub async fn cleanup_task_worktree(
        &self,
        repo_path: &Path,
        task_id: TaskId,
    ) -> Result<()> {
        let worktree_path = repo_path
            .parent().unwrap()
            .join(".worktrees")
            .join(task_id.to_string());

        Command::new("git")
            .args(["worktree", "remove", worktree_path.to_str().unwrap()])
            .current_dir(repo_path)
            .output()
            .await?;

        Ok(())
    }
}
```

---

## TUI Visual Design

**Target:** btop-level visual fidelity using ratatui.

btop (C++) achieves gorgeous terminal graphics. Neuraphage can match this using ratatui's capabilities.

### Visual Techniques

| btop Feature | Technique | ratatui Implementation |
|--------------|-----------|------------------------|
| **Braille graphs** | Unicode Braille block (⠀⠁⠂⠃⡀⡁...) — 2×4 "pixels" per cell | `Canvas` widget with `Marker::Braille` |
| **Smooth progress bars** | Fractional block characters (▏▎▍▌▋▊▉█) | `Gauge` or custom rendering |
| **Color gradients** | 24-bit true color ANSI sequences | `Color::Rgb(r, g, b)` |
| **Rounded corners** | Unicode box drawing (╭╮╰╯) | `Block::bordered().border_type(BorderType::Rounded)` |
| **Sparklines** | Mini bar charts per-character | `Sparkline` widget built-in |
| **Dense layouts** | Careful constraint-based layout | `Layout` with percentage/ratio constraints |

### Braille Rendering (High-Resolution Graphs)

Braille characters give 8× the resolution of normal characters (2×4 dots per cell):

```rust
use ratatui::widgets::canvas::{Canvas, Points};
use ratatui::symbols::Marker;

fn render_cpu_graph(data: &[f64]) -> Canvas {
    Canvas::default()
        .marker(Marker::Braille)  // ← Key: sub-character precision
        .x_bounds([0.0, data.len() as f64])
        .y_bounds([0.0, 100.0])
        .paint(|ctx| {
            // Convert time series to points
            let points: Vec<(f64, f64)> = data
                .iter()
                .enumerate()
                .map(|(i, &v)| (i as f64, v))
                .collect();

            ctx.draw(&Points {
                coords: &points,
                color: Color::Cyan,
            });
        })
}
```

### Color Gradients

For btop-style gradient progress bars:

```rust
use ratatui::style::Color;

/// Generate a color along a gradient (red → yellow → green)
fn health_gradient(percent: f64) -> Color {
    if percent < 0.5 {
        // Red to Yellow (0.0 - 0.5)
        let t = percent * 2.0;
        Color::Rgb(255, (255.0 * t) as u8, 0)
    } else {
        // Yellow to Green (0.5 - 1.0)
        let t = (percent - 0.5) * 2.0;
        Color::Rgb((255.0 * (1.0 - t)) as u8, 255, 0)
    }
}

/// Render a gradient progress bar
fn render_gradient_bar(frame: &mut Frame, area: Rect, value: f64, max: f64) {
    let percent = value / max;
    let filled_width = (area.width as f64 * percent) as u16;

    for x in 0..filled_width {
        let segment_percent = x as f64 / area.width as f64;
        let color = health_gradient(segment_percent);
        let cell = Cell::default().set_char('█').set_fg(color);
        frame.buffer_mut().set(area.x + x, area.y, cell);
    }
}
```

### Fractional Block Characters

For smooth sub-character progress indication:

```rust
/// Fractional block characters for smooth progress bars
const BLOCKS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

fn fractional_bar(width: u16, percent: f64) -> String {
    let total_eighths = (width as f64 * 8.0 * percent) as usize;
    let full_blocks = total_eighths / 8;
    let remainder = total_eighths % 8;

    let mut bar = "█".repeat(full_blocks);
    if remainder > 0 {
        bar.push(BLOCKS[remainder]);
    }
    bar
}
```

### Proposed Layout

```
╭─ Tasks ──────────────────────────────╮╭─ Task: implement auth ─────────────────────────╮
│ ● abc123 implement auth    Running   ││                                                 │
│ ○ def456 fix login bug     Waiting   ││ I'll start by examining the existing user      │
│ ✓ ghi789 update docs       Done      ││ model to understand the authentication flow... │
│ ◌ jkl012 add tests         Queued    ││                                                 │
│                                      ││ Reading src/models/user.rs                     │
│                                      ││ ████████████████████░░░░░░░░░░░░░░ 52%         │
│                                      ││                                                 │
├─ Stats ──────────────────────────────┤│ Found User struct with email and password_hash │
│ Running: 1  Waiting: 1  Queued: 1    ││ fields. I'll add JWT token generation...       │
│ Cost: $1.24  Tokens: 45.2k           ││                                                 │
│                                      ││                                                 │
│ API: ⣀⣠⣤⣶⣿⣿⣿⣶⣤⣀ 42 req/min        ││                                                 │
╰──────────────────────────────────────╯╰─────────────────────────────────────────────────╯
╭─ Notifications ──────────────────────────────────────────────────────────────────────────╮
│ 14:32 Task def456 needs input: "Which auth provider? [OAuth/JWT/Session]"               │
│ 14:28 Task ghi789 completed: Updated README with API documentation                       │
╰──────────────────────────────────────────────────────────────────────────────────────────╯
 [j/k] Navigate  [Enter] Attach  [n] New task  [d] Detach  [q] Quit  [?] Help
```

### Reference Implementation

**bottom** (`btm`) is a Rust system monitor using ratatui that achieves btop-level visuals:
- Repository: https://github.com/ClementTsang/bottom
- Demonstrates: Braille graphs, color gradients, dense layouts
- Proof that ratatui can match btop's fidelity

### Dependencies for Visual Features

```toml
[dependencies]
ratatui = { version = "0.28", features = ["all-widgets"] }
crossterm = { version = "0.28", features = ["event-stream"] }

# For smooth animations (optional)
tokio = { version = "1", features = ["time"] }
```

---

*This document is part of the [Neuraphage Design Documentation](./neuraphage-design.md).*
