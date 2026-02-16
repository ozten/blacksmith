# Blacksmith V3.5: Deployment Model & Self-Improvement Architecture

> Brain dump from session 2026-02-16. Captures design decisions made while
> bringing up parallel agents and discovering that blacksmith can't actually
> run on any repo other than itself.

## The Core Problem

Blacksmith is developed on itself (dogfooding), but it's a tool meant to
orchestrate AI agents on **any** repository. Today, all runtime artifacts
are loose files committed to the blacksmith git repo:

- `PROMPT.md` — agent instructions
- `bd-finish.sh` — session close protocol
- `.claude/skills/` — prd-to-beads, break-down-issue, self-improvement
- `blacksmith.toml` — configuration
- `AGENTS.md` — project onboarding

When a user installs the `blacksmith` binary and runs it in their Rails app
or React project, **none of these files exist**. The coordinator tries to
read PROMPT.md, fails, and crashes.

## Design Principle

**`.blacksmith/` is blacksmith's home directory.** Everything the tool needs
to operate lives there. The repo owner never needs to commit anything
blacksmith-related to their git repo. If they choose to (for version control
or team sharing), that's their decision — blacksmith doesn't require it.

```
someone's-project/
  ├── .blacksmith/                ← tool-managed, gitignored
  │   ├── PROMPT.md               ← agent instructions (self-editing)
  │   ├── blacksmith.toml         ← config (or project root, see below)
  │   ├── skills/                 ← Claude skills for decomposition
  │   │   ├── prd-to-beads/SKILL.md
  │   │   ├── break-down-issue/SKILL.md
  │   │   └── self-improvement/SKILL.md
  │   ├── blacksmith.db           ← metrics, improvements, events
  │   ├── sessions/               ← JSONL output per session
  │   ├── worktrees/              ← git worktrees for parallel agents
  │   └── lock                    ← singleton lock (PID file + flock)
  ├── blacksmith.toml             ← user config (optional override)
  ├── src/                        ← their code
  └── ...
```

### Config resolution order

1. CLI flags (highest priority)
2. `./blacksmith.toml` in project root (user-committed, team-shared)
3. `.blacksmith/blacksmith.toml` (tool-managed defaults)
4. Compiled-in defaults (lowest priority)

This lets teams commit a `blacksmith.toml` to their repo for shared config,
while `.blacksmith/` handles everything else invisibly.

---

## Milestone 1: Embedded Defaults

### 1a. Embed assets in binary

Use `include_str!()` to bake default artifacts into the compiled binary:

```rust
// src/defaults.rs
pub const PROMPT_MD: &str = include_str!("../defaults/PROMPT.md");
pub const FINISH_SCRIPT: &str = include_str!("../defaults/bd-finish.sh");
pub const SKILL_PRD_TO_BEADS: &str = include_str!("../defaults/skills/prd-to-beads.md");
pub const SKILL_BREAK_DOWN: &str = include_str!("../defaults/skills/break-down-issue.md");
pub const SKILL_SELF_IMPROVE: &str = include_str!("../defaults/skills/self-improvement.md");
pub const DEFAULT_CONFIG: &str = include_str!("../defaults/blacksmith.toml");
```

Source files live in `defaults/` directory in the blacksmith repo. These are
the canonical versions. The copies in the project root (PROMPT.md, bd-finish.sh)
are the dogfooding copies used when developing blacksmith itself.

### 1b. First-run extraction

When blacksmith starts and `.blacksmith/` doesn't exist (or is missing key files):

```
blacksmith run
  → .blacksmith/ doesn't exist
  → Create .blacksmith/ directory structure
  → Extract PROMPT.md, skills, config defaults
  → Add .blacksmith/ to .gitignore if not already present
  → Continue with normal startup
```

**Never overwrite existing files.** If `.blacksmith/PROMPT.md` already exists
(from a previous run or manual creation), leave it alone. The user/agent may
have customized it.

### 1c. `blacksmith init` command

Explicit initialization for users who want to set up before first run:

```bash
blacksmith init
# Creates .blacksmith/ with all defaults
# Adds .blacksmith/ to .gitignore
# Prints summary of what was created

blacksmith init --force
# Overwrites existing files with fresh defaults
# Use when upgrading blacksmith version with new PROMPT.md features

blacksmith init --export
# Copies PROMPT.md and skills to project root (outside .blacksmith/)
# For users who want to commit them to their repo
```

---

## Milestone 2: `blacksmith finish` Subcommand

### Problem

`bd-finish.sh` is a shell script that agents call from their worktree.
In the deployed model, this script doesn't exist in the worktree because
it's not in the target repo's git.

### Solution

Replace `bd-finish.sh` with a `blacksmith finish` subcommand compiled
into the binary. The binary is installed globally (or in PATH), so it's
available in every worktree without copying files.

```bash
# Old (requires bd-finish.sh in worktree):
./bd-finish.sh simple-agent-harness-abc "Implement feature X" src/foo.rs

# New (binary subcommand, works anywhere):
blacksmith finish simple-agent-harness-abc "Implement feature X" src/foo.rs
```

### Behavior (same as bd-finish.sh)

1. `cargo check` — abort if compilation fails
2. `cargo test` — abort if tests fail
3. Append PROGRESS.txt to PROGRESS_LOG.txt
4. Stage files (specified or `git add -u`)
5. `git commit`
6. `bd close`
7. `bd sync`
8. Auto-commit .beads/ changes
9. `git push`

### Migration

- PROMPT.md updated to reference `blacksmith finish` instead of `./bd-finish.sh`
- `bd-finish.sh` retained in blacksmith repo for dogfooding / backwards compat
- Embedded PROMPT.md uses `blacksmith finish` by default

### Configurable quality gates

The finish subcommand reads from config which gates to run:

```toml
[finish]
gates = ["cargo check", "cargo test"]
# Users can add project-specific gates:
# gates = ["cargo check", "cargo test", "npm run lint"]
```

This makes blacksmith language-agnostic. A TypeScript project would configure
`tsc --noEmit` and `jest` instead of cargo commands.

---

## Milestone 3: Worktree Provisioning

### Problem

When the coordinator creates a worktree for a parallel agent, the worktree
is a git checkout of main. It won't have:

- `.claude/skills/` (not in git)
- Any non-git blacksmith artifacts

The agent process (`claude`) discovers skills from `.claude/skills/` in its
working directory (the worktree). Without provisioning, skills are invisible.

### Solution: Provision on create, clean on remove

**worktree::create (after git worktree add):**

```rust
fn provision_worktree(worktree_path: &Path, data_dir: &DataDir) {
    // Copy skills from .blacksmith/skills/ to worktree/.claude/skills/
    let src_skills = data_dir.root().join("skills");
    let dst_skills = worktree_path.join(".claude/skills");
    if src_skills.exists() {
        copy_dir_recursive(&src_skills, &dst_skills);
    }

    // Note: PROMPT.md is NOT copied — coordinator passes it via -p
    // Note: blacksmith.toml is NOT copied — coordinator reads from main
    // Note: bd-finish.sh is NOT needed — agents use `blacksmith finish`
}
```

**worktree::remove (before git worktree remove):**

```rust
fn deprovision_worktree(worktree_path: &Path) {
    // Remove non-git files we added
    let claude_dir = worktree_path.join(".claude");
    if claude_dir.exists() {
        fs::remove_dir_all(&claude_dir).ok();
    }
}
```

### What lives where

| Artifact | Location | How agent accesses it |
|----------|----------|----------------------|
| PROMPT.md | `.blacksmith/PROMPT.md` | Coordinator reads it, passes via `-p` |
| Skills | `.blacksmith/skills/` → copied to `worktree/.claude/skills/` | Claude discovers from `.claude/skills/` |
| Finish protocol | `blacksmith finish` binary subcommand | Agent calls it directly |
| Config | `.blacksmith/blacksmith.toml` or `./blacksmith.toml` | Coordinator reads at startup |
| Improvements DB | `.blacksmith/blacksmith.db` | Agent calls `blacksmith improve add` |
| Session output | `.blacksmith/sessions/{N}.jsonl` | Coordinator creates, agent writes via stdout |

---

## Milestone 4: Self-Improvement Architecture

### Two-speed feedback loop

```
FAST (immediate)     .blacksmith/blacksmith.db → improvements table
                     Agent: blacksmith improve add "batch tool calls" --category workflow
                     → Brief injection includes it in next session's prompt
                     → No git, no worktree, no integration
                     → Takes effect in seconds

SLOW (reviewed)      .blacksmith/PROMPT.md → coordinator re-reads each iteration
                     Agent (or human) edits PROMPT.md
                     → Structural changes to agent behavior
                     → Takes effect on next agent spawn
                     → Still no git required — .blacksmith/ is the source of truth
```

### The promotion cycle

```
Session N:    Agent discovers pattern → records improvement
              blacksmith improve add "Always run cargo check before closing"
              → Status: open, Ref: R1

Session N+1:  Brief injects: "## OPEN IMPROVEMENTS\n- R1: Always run cargo check..."
              → Agent sees it, follows it

Session N+5:  Pattern proven across 5 sessions
              Agent (or human): blacksmith improve promote R1
              → Status: promoted

Session N+6:  Promotion trigger fires
              → PROMPT.md is edited to incorporate R1 permanently
              → R1 status: closed
```

### Who edits PROMPT.md?

**In the dogfooding case** (blacksmith developing itself):
- PROMPT.md is in git, agents edit via worktree + integration
- Self-improvement competes with code changes for integration slots
- Slow but reviewed

**In the deployed case** (blacksmith running on other repos):
- PROMPT.md is in `.blacksmith/`, not in git
- Self-improvement edits it directly (no worktree needed)
- The coordinator re-reads each iteration
- Fast, unreviewed — but improvements are proven via the promotion cycle first
- If an edit degrades performance, the metrics system detects it

### Parallel editing concern

With 3 agents running, what if two agents both call `blacksmith improve add`
at the same time?

**Improvements DB:** SQLite with WAL mode handles concurrent writers. No issue.

**PROMPT.md direct edits:** Only the promotion step edits PROMPT.md, and it runs
in the coordinator (single-threaded between iterations). Agents never edit
PROMPT.md directly — they record improvements, and the coordinator promotes.

### Auto-promotion (future)

```toml
[improvements]
auto_promote_after = 5   # sessions where improvement was active
auto_promote_metric = "cost"  # only if this metric improved
```

When an improvement has been in the brief for N sessions and the target metric
improved, auto-promote it to PROMPT.md. This closes the loop without human
intervention.

---

## Milestone 5: Process Quality Gates

### Bead description template

Every bead created by `/prd-to-beads` or `/break-down-issue` MUST include:

```markdown
<What to build — 2-4 sentences>

## Done when
- [ ] <specific, testable condition>
- [ ] <specific, testable condition>
- [ ] Quality gates pass (cargo check, cargo test, or project equivalent)

## Verify
- Run: <exact command>
- Expect: <exact output or behavior>

## Affected files
- <file> (new|modified)
```

**Done when** — independently verifiable conditions, not "works correctly."
**Verify** — concrete command, not "check that it works."
**Affected files** — feeds the parallel scheduler's conflict detection.

### Quality gates in `blacksmith finish`

```
blacksmith finish <bead-id> "<message>" [files...]
  ├── 0a. Run configured check command (default: cargo check)
  ├── 0b. Run configured test command (default: cargo test)
  ├── 1.  Append PROGRESS.txt to PROGRESS_LOG.txt
  ├── 2.  Stage files
  ├── 3.  git commit
  ├── 4.  bd close
  ├── 5.  bd sync
  ├── 6.  Auto-commit .beads/
  └── 7.  git push
```

If 0a or 0b fail, the bead is NOT closed. The agent must fix the issue first.

### Agent verification protocol (PROMPT.md)

Step 4 of the execution protocol:

1. **4a. Bead-specific verification** — read the bead's "## Verify" section
   and execute those exact steps
2. **4b. Code quality gates** — run test suite + lint
3. **4c. Integration check** — grep for changed function/struct names,
   confirm callers still work

---

## Milestone 6: Language Agnosticism

### Problem

Today, blacksmith assumes Rust everywhere:
- `cargo check`, `cargo test`, `cargo clippy` hardcoded in PROMPT.md
- `bd-finish.sh` runs `cargo check` and `cargo test`
- Metrics extraction assumes Claude adapter JSONL format

### Solution

Make quality gates, test commands, and build commands configurable:

```toml
[finish]
check = "cargo check"      # or "tsc --noEmit" or "go vet ./..."
test = "cargo test"         # or "jest" or "go test ./..."
lint = "cargo clippy --fix" # or "eslint --fix" or "golangci-lint run"
format = "cargo fmt"        # or "prettier --write" or "gofmt -w"

[agent]
command = "claude"
adapter = "claude"          # or "codex" or "opencode"
```

PROMPT.md uses template variables:

```markdown
## Verify
```bash
{check_command}
{test_command}
{lint_command}
{format_command}
```
```

The coordinator substitutes these when assembling the prompt.

---

## Migration Path (from dogfooding to deployable)

### Phase 1: Embed + Init (unblocks other repos)
1. Create `defaults/` directory with canonical PROMPT.md, skills, config
2. Add `include_str!()` embedding in `src/defaults.rs`
3. Add first-run extraction to DataDir initialization
4. Add `blacksmith init` command
5. Test on a fresh repo (not blacksmith itself)

### Phase 2: Finish Subcommand (removes file dependency)
1. Add `blacksmith finish` subcommand (port bd-finish.sh logic)
2. Update embedded PROMPT.md to use `blacksmith finish`
3. Keep bd-finish.sh for backwards compat during transition

### Phase 3: Worktree Provisioning (enables parallel on other repos)
1. Add provision step to worktree::create (copy skills)
2. Add deprovision step to worktree::remove (cleanup)
3. Test parallel agents on a non-blacksmith repo

### Phase 4: Self-Improvement Loop (closes the optimization loop)
1. Add promotion step to coordinator (between iterations)
2. Add PROMPT.md template variable substitution
3. Add auto-promotion config and logic

### Phase 5: Language Agnosticism (broadens audience)
1. Make quality gates configurable
2. Add template variable substitution to prompt assembly
3. Test on a TypeScript project, a Go project, a Python project

---

## Decisions Made This Session

| Decision | Rationale |
|----------|-----------|
| `.blacksmith/` owns all runtime artifacts | Repo owner shouldn't need to commit anything |
| PROMPT.md not in git (deployed case) | Tool-managed, self-editing, no PR needed |
| `blacksmith finish` replaces bd-finish.sh | Binary is globally available, no file copying |
| Skills copied to worktree on create | Claude discovers from `.claude/skills/` in cwd |
| Improvements DB is the fast feedback path | SQLite, no git, immediate via brief injection |
| PROMPT.md edits are the slow feedback path | Coordinator re-reads each iteration |
| Only promotion step edits PROMPT.md | Prevents parallel agents from conflicting |
| Quality gates configurable in config | Language agnosticism for non-Rust projects |
| Bead descriptions require Done/Verify/Affected | Prevents premature closure, feeds scheduler |

## Bugs Found This Session

| Bug | Root cause | Fix |
|-----|-----------|-----|
| Singleton lock doesn't compile | Session 50 closed bead without verifying cargo check | Added cargo test gate to bd-finish.sh |
| Stale worktrees on restart | cleanup_orphans() written but never called (#[allow(dead_code)]) | Wired into coordinator startup |
| Agents suspended (SIGTSTP) | Missing .stdin(Stdio::null()) on background process spawn | Added to pool.rs |
| Session files never compressed/retained | Worker files named worker-{id}-{bead}.jsonl, parsers expect {N}.jsonl | Agent fixed to use numeric naming |
| 0-byte session files on failed spawn | Output file created before worktree, not cleaned up on failure | Noted in vrq bead |
| Improvements never recorded (0 in 73 sessions) | PROMPT.md didn't instruct agents to use `blacksmith improve add` | Added instructions to PROMPT.md |
