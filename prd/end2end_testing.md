# Blacksmith E2E Testing Strategy

## The Problem You're Solving

Agents write unit tests but leave "manual verification steps" because they don't have a way to exercise the full blacksmith loop. The existing 1,119 unit tests cover individual modules well, but nothing tests the actual sequence: **config → beads query → schedule → spawn agent → capture output → retry/proceed → close bead → integrate**. That gap is exactly where features fall through the cracks.

## Architecture Summary (What We're Testing)

Two external boundaries exist in the Rust code:

1. **Agent subprocess** — `session::run_session()` spawns whatever `[agent] command` is configured via `tokio::process::Command`. Already swappable: config accepts `echo`, `cat`, any CLI.
2. **Beads query** — `coordinator::query_ready_beads()` shells out to `bd list --status=open --json`. This is the only `bd` callsite in the scheduling path.

Everything else — config parsing, retry logic, watchdog, scheduler, worktree management, metrics ingestion, integration queue — is deterministic Rust code making decisions based on the outputs of those two boundaries.

## Recommended Approach: Option 1 (Fake Agent + Fake `bd` Shim)

Your instinct on option 1 is right, and the codebase makes it surprisingly easy. Here's why:

- `harness.toml` already supports `[agent] command = "echo"` — the session tests in `session.rs` use `echo`, `printf`, `cat`, and `sh -c` as fake agents
- The coordinator reads beads via a single `bd list --status=open --json` subprocess call (line 809 of coordinator.rs)
- You can put a fake `bd` script on `PATH` that returns canned JSON

### The Two Shims You Need

**1. Fake `bd` script** — a shell script (or tiny Rust binary) that:
- On `bd list --status=open --json` → reads from a fixture file (e.g., `.beads-fixture/ready.json`)
- On `bd close <id>` → appends to a log file so tests can assert it was called
- On `bd sync` → no-op (exit 0)
- On `bd show <id> --json` → returns details from fixture

```bash
#!/usr/bin/env bash
# tests/fixtures/fake-bd.sh
FIXTURE_DIR="${BD_FIXTURE_DIR:-.beads-fixture}"
LOG="${FIXTURE_DIR}/bd-calls.log"
echo "$@" >> "$LOG"

case "$1" in
  list)
    cat "${FIXTURE_DIR}/ready.json" 2>/dev/null || echo "[]"
    ;;
  close|start|sync)
    exit 0
    ;;
  show)
    echo '{"id":"'$2'","status":"open"}'
    ;;
  *)
    exit 0
    ;;
esac
```

**2. Fake agent script** — instead of Claude Code, a script that does predictable, verifiable work:

```bash
#!/usr/bin/env bash
# tests/fixtures/fake-agent.sh
# Reads the prompt from args, creates deterministic output
PROMPT="$1"
WORKDIR="${AGENT_WORKDIR:-.}"

# Create a file so blacksmith sees "work was done"
touch "${WORKDIR}/agent-was-here.txt"
echo "implemented: ${PROMPT}" > "${WORKDIR}/agent-was-here.txt"

# Produce output that blacksmith's adapter will parse
# (raw adapter just captures stdout)
echo '{"type":"result","result":"success"}'
```

For more sophisticated scenarios, the fake agent can:
- `touch` specific files based on bead ID parsed from the prompt
- Exit non-zero to test retry logic
- Sleep to test watchdog timeouts
- Write different amounts of output to test min_output_bytes thresholds

### Test Harness Structure

```
tests/
├── e2e/
│   ├── common.rs              # Shared setup: create temp repo, write config, install shims
│   ├── test_serial_loop.rs    # Single-worker loop scenarios
│   ├── test_multi_agent.rs    # Parallel workers + worktree isolation
│   ├── test_error_recovery.rs # Retry, rate limit, watchdog scenarios
│   └── test_init.rs           # blacksmith init in various project types
├── fixtures/
│   ├── fake-bd.sh             # Configurable bd shim
│   ├── fake-agent.sh          # Configurable agent shim
│   └── beads/                 # Canned bd JSON responses
│       ├── single-task.json
│       ├── three-tasks-no-conflicts.json
│       ├── tasks-with-dependency-chain.json
│       ├── tasks-with-cycle.json
│       └── empty.json
```

### A Concrete Test: Serial Loop Happy Path

```rust
// tests/e2e/test_serial_loop.rs
use std::process::Command;
use std::path::PathBuf;
use tempfile::TempDir;

/// Set up a temp git repo with blacksmith config pointing at our fake shims.
fn setup_project(beads_fixture: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();

    // git init
    Command::new("git").args(["init"]).current_dir(&root).output().unwrap();
    Command::new("git").args(["config", "user.email", "test@ci"]).current_dir(&root).output().unwrap();
    Command::new("git").args(["config", "user.name", "CI"]).current_dir(&root).output().unwrap();

    // Initial commit (needed for worktrees)
    std::fs::write(root.join("README.md"), "# test").unwrap();
    Command::new("git").args(["add", "."]).current_dir(&root).output().unwrap();
    Command::new("git").args(["commit", "-m", "init"]).current_dir(&root).output().unwrap();

    // .blacksmith/config.toml — point at fake agent, 1 iteration
    let bs_dir = root.join(".blacksmith");
    std::fs::create_dir_all(&bs_dir).unwrap();
    let fake_agent = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-agent.sh");
    let config = format!(r#"
[session]
max_iterations = 1
prompt_file = "PROMPT.md"

[agent]
command = "{}"
args = ["{{prompt}}"]
adapter = "raw"

[watchdog]
stale_timeout_mins = 1
min_output_bytes = 1

[retry]
max_empty_retries = 0
"#, fake_agent.display());
    std::fs::write(bs_dir.join("config.toml"), config).unwrap();

    // PROMPT.md
    std::fs::write(root.join("PROMPT.md"), "Touch a file called hello.txt").unwrap();

    // Beads fixture
    let fixture_dir = root.join(".beads-fixture");
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let fixture_src = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/beads")
        .join(beads_fixture);
    std::fs::copy(fixture_src, fixture_dir.join("ready.json")).unwrap();

    (dir, root)
}

#[test]
fn serial_loop_completes_one_iteration() {
    let (dir, root) = setup_project("single-task.json");
    let fake_bd = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake-bd.sh");

    // Add fake bd to PATH
    let original_path = std::env::var("PATH").unwrap_or_default();
    let shim_dir = dir.path().join("shims");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::os::unix::fs::symlink(&fake_bd, shim_dir.join("bd")).unwrap();

    let blacksmith = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/debug/blacksmith");

    let output = Command::new(&blacksmith)
        .arg("1")  // max_iterations = 1
        .current_dir(&root)
        .env("PATH", format!("{}:{}", shim_dir.display(), original_path))
        .env("BD_FIXTURE_DIR", root.join(".beads-fixture"))
        .output()
        .unwrap();

    // Assert: process exited cleanly
    assert!(output.status.success(), "blacksmith failed: {}",
        String::from_utf8_lossy(&output.stderr));

    // Assert: agent was invoked (output file exists)
    let session_files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .collect();
    assert!(!session_files.is_empty(), "no session output files created");

    // Assert: agent output was captured (non-empty)
    let first = &session_files[0];
    let size = first.metadata().unwrap().len();
    assert!(size > 0, "session output file is empty");

    // Assert: bd was called (check the log)
    let bd_log = std::fs::read_to_string(
        root.join(".beads-fixture/bd-calls.log")
    ).unwrap_or_default();
    assert!(bd_log.contains("list"), "bd list was never called");
}
```

### Key Test Scenarios

Here are the scenarios that catch the bugs agents typically introduce:

| Scenario | Fake bd returns | Fake agent does | Assert |
|---|---|---|---|
| **Happy path** | 1 ready bead | Exits 0 with output | Session file created, non-empty |
| **Empty queue** | `[]` | N/A | Blacksmith exits cleanly, no crash |
| **Agent crash** | 1 bead | Exits 1 | Retry triggered, bead NOT closed |
| **Empty output** | 1 bead | Exits 0, writes 0 bytes | Retry triggered (min_output_bytes) |
| **Rate limit** | 1 bead | Writes error JSON with rate limit keywords | Backoff delay observed, bead NOT closed |
| **Watchdog kill** | 1 bead | Sleeps for 120s | Process killed after stale_timeout |
| **STOP file** | 3 beads | Exits 0 | Create STOP file mid-loop, verify early exit |
| **No config file** | 1 bead | Exits 0 | Embedded defaults apply, runs fine |
| **Multi-worker isolation** | 3 non-overlapping beads | Touches files | 3 worktrees created, no conflicts |
| **Dependency chain** | A→B→C chain | Exits 0 | Only C runs first, then B, then A |
| **Cycle detection** | A↔B cycle | N/A | Cycled beads excluded, warning logged |
| **`blacksmith init`** | N/A | N/A | .blacksmith/ created, PROMPT.md generated, config valid |
| **Config variations** | 1 bead | Exits 0 | Test with config.toml present vs absent vs malformed |

### Fixture Files

```json
// tests/fixtures/beads/single-task.json
[
  {
    "id": "bd-test-001",
    "title": "Touch hello.txt",
    "status": "open",
    "priority": 1,
    "design": "affected: hello.txt",
    "dependencies": []
  }
]
```

```json
// tests/fixtures/beads/three-tasks-no-conflicts.json
[
  {"id": "bd-t1", "title": "Task A", "status": "open", "priority": 1,
   "design": "affected: src/a.rs", "dependencies": []},
  {"id": "bd-t2", "title": "Task B", "status": "open", "priority": 1,
   "design": "affected: src/b.rs", "dependencies": []},
  {"id": "bd-t3", "title": "Task C", "status": "open", "priority": 1,
   "design": "affected: src/c.rs", "dependencies": []}
]
```

```json
// tests/fixtures/beads/tasks-with-cycle.json
[
  {"id": "bd-x", "title": "X", "status": "open", "priority": 1,
   "design": "", "dependencies": [{"depends_on_id": "bd-y"}]},
  {"id": "bd-y", "title": "Y", "status": "open", "priority": 1,
   "design": "", "dependencies": [{"depends_on_id": "bd-x"}]}
]
```

## Why Not Option 2 (LLM-Driven Exploratory Testing)

Option 2 has a role but shouldn't replace deterministic E2E tests:

- **Non-reproducible** — different results each run, can't use in CI
- **Expensive** — even Haiku costs real money per invocation
- **Slow** — minutes per run vs seconds for fake-agent tests
- **Flaky** — LLM might misunderstand the verification task

Where it IS useful: as an occasional **soak test** or **chaos test**. Run it nightly with a cheap model to catch edge cases the deterministic tests don't anticipate. But the deterministic suite is your foundation.

## Option 3: Things You Might Not Have Considered

### A. Stateful fake `bd` with progression

Instead of static JSON fixtures, make the fake `bd` script track state across calls:

```bash
# On "bd close <id>": remove that id from ready.json, write to closed.json
# On next "bd list": return the updated ready.json
# This lets you test multi-iteration loops where beads are consumed
```

This is critical for testing the coordinator's polling loop — verifying it re-queries beads after each completion and handles the shrinking queue correctly.

### B. Property-based testing of the scheduler

The scheduler's glob overlap detection and dependency filtering are pure functions. Use `proptest` to generate random bead configurations and verify invariants:

```rust
// Invariant: no two simultaneously scheduled beads have overlapping affected sets
// Invariant: a bead is never scheduled before all its dependencies are closed
// Invariant: cycled beads are never scheduled
```

This catches edge cases in the scheduling logic that hand-written fixtures miss.

### C. A `--dry-run` mode for blacksmith itself

Add a flag that runs the full loop logic but replaces `Command::new(agent)` with a no-op that writes fake output. This would let the entire Rust test suite exercise the full code path without any external dependencies. The key change would be an `AgentRunner` trait:

```rust
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run_session(
        &self,
        agent_config: &AgentConfig,
        output_path: &Path,
        prompt: &str,
    ) -> Result<SessionResult, SessionError>;
}

// Production: spawns real subprocess (current code)
pub struct SubprocessRunner;

// Test: writes canned output, returns configurable result
pub struct FakeRunner {
    pub exit_code: i32,
    pub output: String,
    pub delay: Duration,
}
```

This is a modest refactor (inject the runner into `runner::run()` and `coordinator::run()`) but would make the entire orchestration layer testable without subprocess mocking. You'd test the real Rust code paths, not shell script shims.

### D. Snapshot testing for `blacksmith init`

Use `insta` (Rust snapshot testing) to capture the exact output of `blacksmith init` in different project types. When the init logic changes, you'll see the diff immediately:

```rust
#[test]
fn init_rust_project_snapshot() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
    // ... run blacksmith init ...
    let prompt = std::fs::read_to_string(dir.path().join("PROMPT.md")).unwrap();
    insta::assert_snapshot!(prompt);
}
```

## Implementation Priority

I'd start with this order:

1. **Fake `bd` shim + fake agent script** — get one happy-path E2E test passing. This proves the harness works.
2. **`blacksmith init` tests** — low-hanging fruit, no agent needed, just filesystem assertions.
3. **Error scenario tests** (crash, empty output, rate limit) — these catch the bugs agents miss most often.
4. **`AgentRunner` trait refactor** — once you have shell-based E2E working, this upgrade makes the tests faster and more reliable.
5. **Multi-agent/worktree scenarios** — more complex setup but high-value.
6. **Property-based scheduler tests** — icing on the cake.

The first three items could be done in a day. The trait refactor is maybe half a day. After that, you have a test suite that can catch the "agent forgot to implement the end-to-end flow" problem because every new feature can be verified with: create a fixture, write a fake agent behavior, assert on the outcome.
