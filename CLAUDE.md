# Claude Code — Agent-Specific Instructions

These rules apply only when the agent is Claude Code. They supplement the
agent-agnostic workflow in PROMPT.md (which blacksmith injects into all agents).

## Efficiency Rules

### Batch independent tool calls in the SAME turn.
Every time you are about to call a tool, ask: "Is there another independent call
I can make at the same time?" If yes, emit BOTH tool calls in the SAME message.

**Mandatory parallel patterns:**
- Session start: `bd ready` + `Read PROGRESS.txt` → ONE turn, TWO tool calls
- Reading source + test: `Read foo.rs` + `Read foo_test.rs` → ONE turn
- Multiple greps: `Grep("pattern1")` + `Grep("pattern2")` → ONE turn
- Session end: `Bash(cargo clippy --fix)` + `Bash(cargo test --release)` → ONE turn
- Reading multiple related files: `Read config.rs` + `Read main.rs` → ONE turn

Target at least 5 turns with 2+ parallel calls per session.

### NEVER emit a text-only turn.
Every assistant message MUST include at least one tool call.
Include narration AND the tool call in the same message.
A text-only turn doubles your turn count for zero benefit.

### Do NOT launch explore/research subagents.
NO `Task` tool with `subagent_type: Explore`. The architecture is documented in
PROMPT.md. Only read files you are about to modify.

## Operator Notes (for interactive Claude Code sessions)

### Monitoring a run
```bash
blacksmith --status                  # quick snapshot: workers, progress, ETA
blacksmith workers status            # per-worker assignments with liveness check
blacksmith metrics status            # per-session cost/turns/duration table
blacksmith metrics beads             # per-bead timing, highlights outliers
blacksmith integration log           # merge history with commits
blacksmith estimate                  # serial/parallel ETA, critical path depth
blacksmith brief                     # last session performance stats
```

### Key files
- Config: `.blacksmith/config.toml`
- DB: `.blacksmith/blacksmith.db`
- Worktrees: `.blacksmith/worktrees/worker-{N}-{bead_id}/`
- Sessions: `.blacksmith/sessions/{N}.jsonl`
- PRDs: `prd/SPEC.md` (V1), `prd/SPEC-v2-metrics.md` (V2), `prd/SPEC-v3-agents.md` (V3)
