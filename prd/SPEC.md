# simple-agent-harness

A Rust CLI tool that runs an AI coding agent in a supervised loop: dispatch a prompt, monitor the session, enforce health invariants, collect metrics, and repeat.

## Problem

Running an autonomous coding agent (e.g. `claude -p`) in a loop requires orchestration that bash scripts handle poorly:

- **No timeout**: A hung session blocks the entire loop indefinitely. The only fix is manual `kill`.
- **No retry**: An empty/crashed session (0 bytes output) wastes an iteration slot. No automatic recovery.
- **No structured metrics**: Session metrics (turns, tool calls, commit status) are parsed ad-hoc with grep/jq. No queryable history.
- **No graceful shutdown**: Ctrl-C during a session leaves orphan processes and uncommitted work.
- **Fragile config**: Timeout thresholds, retry counts, backoff parameters, and prompt paths are scattered across shell variables with no validation.
- **No observability**: No way to see what the loop is doing from another terminal without tailing raw JSONL.

The current `ralph-wiggums-loop.sh` (348 iterations and counting) has grown organically to address these problems with shell workarounds. It works, but it's brittle. This tool replaces it.

## Goals

1. **Drop-in replacement** for `ralph-wiggums-loop.sh` with the same external interface (prompt file, output files, iteration counter, STOP file)
2. **Correct process management**: proper signal handling, child cleanup, no orphans
3. **Structured observability**: live status, queryable session history, machine-readable output
4. **Extensible**: plugin points for pre/post-session hooks, custom health checks, alternative agent backends

## Non-Goals

- GUI or TUI dashboard (terminal output is sufficient)
- Multi-agent parallelism (one session at a time)
- Agent prompt engineering (the harness is prompt-agnostic)
- Replacing the `self-improvement` metrics tool (harness produces structured events; analysis stays external)

---

## Architecture

```
simple-agent-harness
  ├── src/
  │   ├── main.rs           # CLI entry, arg parsing, config loading
  │   ├── config.rs         # Configuration struct + validation + file loading
  │   ├── runner.rs         # Core loop: dispatch → monitor → collect → repeat
  │   ├── session.rs        # Single session lifecycle: spawn, stream, watchdog
  │   ├── watchdog.rs       # Output-growth monitor, stale detection, kill logic
  │   ├── retry.rs          # Retry policy for empty/crashed sessions
  │   ├── metrics.rs        # JSONL parsing, session metrics extraction
  │   ├── signals.rs        # Signal handling (SIGINT, SIGTERM, STOP file)
  │   └── hooks.rs          # Pre/post-session hook execution
  ├── Cargo.toml
  └── harness.toml          # Default configuration (user overridable)
```

---

## Configuration

A single `harness.toml` file in the working directory (overridable via `--config`). All fields have sensible defaults so an empty file is valid.

```toml
# harness.toml

[session]
max_iterations = 25              # Total productive iterations before exiting
prompt_file = "PROMPT.md"        # Path to the prompt template
output_dir = "."                 # Where to write session JSONL files
output_prefix = "claude-iteration"  # Output filename prefix
counter_file = ".iteration_counter" # Persistent global iteration counter

[agent]
command = "claude"               # Agent binary
args = ["-p", "{prompt}", "--dangerously-skip-permissions", "--verbose", "--output-format", "stream-json"]
# {prompt} is replaced with the prompt content at runtime
# Additional static args can be appended

[watchdog]
check_interval_secs = 60        # How often to check output growth
stale_timeout_mins = 20         # Kill session after this many minutes of no output growth
min_output_bytes = 100          # Output below this threshold = "empty session"

[retry]
max_empty_retries = 2           # Retries per iteration slot for empty sessions
retry_delay_secs = 5            # Delay before retrying an empty session

[backoff]
initial_delay_secs = 2          # Delay between successful iterations
max_delay_secs = 600            # Cap on exponential backoff
max_consecutive_rate_limits = 5 # Exit after this many consecutive rate limits

[shutdown]
stop_file = "STOP"              # Touch this file to stop after current iteration

[hooks]
pre_session = []                # Shell commands to run before each session
post_session = []               # Shell commands to run after each session
# Hook commands receive environment variables:
#   HARNESS_ITERATION, HARNESS_GLOBAL_ITERATION, HARNESS_OUTPUT_FILE
#   HARNESS_EXIT_CODE, HARNESS_OUTPUT_BYTES (post-session only)
```

CLI flags override config file values. Config file overrides compiled defaults.

**Precedence**: CLI flags > `harness.toml` > compiled defaults.

---

## CLI Interface

```
simple-agent-harness [OPTIONS] [MAX_ITERATIONS]

Arguments:
  [MAX_ITERATIONS]    Override max iterations (default: from config)

Options:
  -c, --config <PATH>        Config file path (default: ./harness.toml)
  -p, --prompt <PATH>        Prompt file path (overrides config)
  -o, --output-dir <PATH>    Output directory (overrides config)
      --timeout <MINS>       Stale timeout in minutes (overrides config)
      --retries <N>          Max empty retries (overrides config)
      --dry-run              Validate config and print resolved settings, don't run
  -v, --verbose              Extra logging (watchdog checks, retry decisions)
  -q, --quiet                Suppress per-iteration banners, only errors and summary
      --status               Print current loop state (reads counter file + status file) and exit

Signals:
  SIGINT (Ctrl-C)    Wait for current session to finish, then exit cleanly
  SIGINT x2          Kill current session immediately, exit
  SIGTERM             Same as single SIGINT (graceful)
```

### Status Command

```
$ simple-agent-harness --status
Loop state: running (PID 12345)
Current iteration: 14/25 (global: 362)
Session output: 48.2 KB (growing)
Uptime: 3h 42m
Last completed: iteration 361 — committed (bd-finish detected)
```

Reads from a `harness.status` file that the running loop writes atomically on each state change.

---

## Core Behavior

### Session Lifecycle

```
┌─────────────────────────────────────────┐
│              Iteration N                │
├─────────────────────────────────────────┤
│ 1. Check STOP file                      │
│ 2. Run pre_session hooks                │
│ 3. Read + prepare prompt                │
│ 4. Spawn agent subprocess               │
│    ├── stdout/stderr → output file      │
│    └── watchdog monitors output growth  │
│ 5. Session ends (natural or killed)     │
│ 6. Check output size                    │
│    ├── < min_output_bytes → retry       │
│    └── >= min_output_bytes → continue   │
│ 7. Run post_session hooks               │
│ 8. Detect rate limiting                 │
│ 9. Update counters + status file        │
│ 10. Delay before next iteration         │
└─────────────────────────────────────────┘
```

### Watchdog

The watchdog runs in a separate tokio task (or thread) alongside the spawned agent process.

**Algorithm**:
1. Record initial output file size
2. Every `check_interval_secs`, read the file size
3. If size has grown since last check: reset stale timer
4. If size has NOT grown: increment stale timer
5. If stale timer >= `stale_timeout_mins`: kill the agent process group
6. Return exit code 124 (timeout convention)

**Process cleanup**:
- Send SIGTERM to the process group (not just the child PID)
- Wait 5 seconds for graceful shutdown
- Send SIGKILL if still alive
- Collect exit status to avoid zombies

### Retry Policy

When a session produces < `min_output_bytes` of output:

1. Log a warning with the output size
2. Increment `global_iteration` (new output filename)
3. Re-run the session with the same prompt
4. After `max_empty_retries` failures, skip to the next iteration
5. Empty retries do NOT increment the productive `iteration` counter
6. Empty retries do NOT trigger post-session hooks

### Rate Limit Detection

Scan the output file for rate limit indicators:
- JSON: `"error":"rate_limit"`
- Text: `usage limit`, `hit your limit`, `resets.*UTC` (case-insensitive)

On detection:
1. Increment consecutive rate limit counter
2. Apply exponential backoff: `initial_delay * 2^consecutive_count`, capped at `max_delay_secs`
3. Do not increment productive iteration counter
4. After `max_consecutive_rate_limits` consecutive rate limits, exit the loop

A successful (non-rate-limited) session resets the consecutive counter and backoff to initial values.

### Graceful Shutdown

Three shutdown triggers, all leading to the same clean exit path:

1. **STOP file**: Checked at the top of each iteration. If present, log the event, delete the file, and exit.
2. **SIGINT (first)**: Set a flag. After the current session finishes, exit the loop instead of starting a new iteration. Print "Caught SIGINT, finishing current session..."
3. **SIGINT (second / within 3s of first)**: Kill the current agent process group immediately. Exit.
4. **SIGTERM**: Same as single SIGINT.

On any shutdown:
- Write final status to `harness.status`
- Persist the global iteration counter
- Print summary line (same as normal loop completion)

### Status File

The harness writes `harness.status` (JSON) atomically (write-to-temp + rename) on every state transition:

```json
{
  "pid": 12345,
  "state": "session_running",
  "iteration": 14,
  "max_iterations": 25,
  "global_iteration": 362,
  "output_file": "claude-iteration-362.jsonl",
  "output_bytes": 49331,
  "session_start": "2026-02-14T23:15:00Z",
  "last_update": "2026-02-14T23:18:30Z",
  "last_completed_iteration": 361,
  "last_committed": true,
  "consecutive_rate_limits": 0
}
```

States: `starting`, `pre_hooks`, `session_running`, `watchdog_kill`, `retrying`, `post_hooks`, `rate_limited_backoff`, `idle`, `shutting_down`.

---

## Structured Output

### Console Output

The harness uses structured, parseable log lines:

```
[2026-02-14T23:15:00Z] [INFO]  iteration=14 global=362 status=starting
[2026-02-14T23:15:01Z] [INFO]  iteration=14 global=362 status=session_running pid=54321
[2026-02-14T23:18:30Z] [WARN]  iteration=14 global=362 watchdog=stale stale_secs=60 limit=1200
[2026-02-14T23:38:30Z] [ERROR] iteration=14 global=362 watchdog=killed stale_mins=20
[2026-02-14T23:38:31Z] [WARN]  iteration=14 global=362 retry=1/2 output_bytes=0
[2026-02-14T23:45:00Z] [INFO]  iteration=14 global=363 status=completed output_bytes=128456 committed=true
```

Agent output (stdout/stderr from the subprocess) goes to the JSONL file only, not to the harness's console output. Use `--verbose` to interleave agent output with harness logs.

### Event Log

Optionally (`[output] event_log = "harness-events.jsonl"`), the harness appends one JSON event per line for each significant state transition. This is the machine-readable equivalent of the console log, suitable for the `self-improvement` tool to ingest.

```json
{"ts":"2026-02-14T23:45:00Z","event":"session_complete","iteration":14,"global":363,"output_bytes":128456,"exit_code":0,"duration_secs":1800,"committed":true,"retries":0,"rate_limited":false}
```

---

## Hooks

Pre- and post-session hooks are shell commands executed synchronously.

```toml
[hooks]
pre_session = [
    "bd list --status=in_progress --json | jq -r '.[].id' | while read id; do bd update \"${id##*-}\" --status=open; done"
]
post_session = [
    "tools/self-improvement log $HARNESS_OUTPUT_FILE"
]
```

**Environment variables** available to hooks:

| Variable | Available | Description |
|---|---|---|
| `HARNESS_ITERATION` | pre + post | Productive iteration counter (0-based) |
| `HARNESS_GLOBAL_ITERATION` | pre + post | Global iteration counter |
| `HARNESS_OUTPUT_FILE` | post only | Path to the session's JSONL output file |
| `HARNESS_EXIT_CODE` | post only | Agent process exit code (124 = watchdog kill) |
| `HARNESS_OUTPUT_BYTES` | post only | Size of the output file in bytes |
| `HARNESS_SESSION_DURATION` | post only | Session wall time in seconds |
| `HARNESS_COMMITTED` | post only | "true" if bd-finish detected in output |
| `HARNESS_PROMPT_FILE` | pre + post | Path to the prompt file |

**Hook failure behavior**:
- Pre-session hook failure (non-zero exit): skip this iteration, log error, continue to next
- Post-session hook failure: log error, continue (session output is already saved)

---

## Prompt Injection

The harness reads the prompt file and can optionally prepend/append content before passing it to the agent. This replaces the inline bash logic for injecting performance briefs.

```toml
[prompt]
file = "PROMPT.md"
prepend_commands = ["tools/self-improvement brief --last 5"]
# Output of each command is prepended to the prompt, separated by "\n---\n"
# Empty command output is silently skipped
```

---

## Implementation Plan

### Milestone 1: Core Loop (MVP)

Minimum viable replacement for `ralph-wiggums-loop.sh`.

- `config.rs` — Load `harness.toml` with defaults, merge CLI flags
- `session.rs` — Spawn agent subprocess, pipe output to file
- `watchdog.rs` — Monitor output file growth, kill stale sessions
- `retry.rs` — Retry empty sessions up to N times
- `runner.rs` — Main loop: iterate, dispatch, collect
- `signals.rs` — STOP file check, SIGINT/SIGTERM handling
- `main.rs` — CLI parsing with clap, wire everything together

**Exit criteria**: Can run `simple-agent-harness 5` and produce 5 productive iterations with the same behavior as the shell script. STOP file and Ctrl-C both work. Stale sessions get killed. Empty sessions get retried.

### Milestone 2: Observability

- `harness.status` JSON file, atomic writes
- `--status` command to read and display it
- Structured console log format (timestamped, key=value)
- Event log (JSONL) for external tooling

**Exit criteria**: Can run `simple-agent-harness --status` from another terminal and see current loop state. `harness-events.jsonl` has one entry per session.

### Milestone 3: Hooks + Prompt Injection

- Pre/post-session hook execution with environment variables
- `[prompt] prepend_commands` for dynamic prompt construction
- Migrate zombie-reset and self-improvement calls from the shell script into hooks

**Exit criteria**: Full feature parity with `ralph-wiggums-loop.sh`. The shell script can be retired.

### Milestone 4: Polish

- `--dry-run` mode
- Config validation with helpful error messages
- `--quiet` mode
- Rate limit detection with configurable patterns
- Commit detection with configurable patterns

---

## Dependencies

```toml
[dependencies]
clap = { version = "4", features = ["derive"] }    # CLI parsing
serde = { version = "1", features = ["derive"] }    # Config deserialization
toml = "0.8"                                         # Config file format
tokio = { version = "1", features = ["full"] }       # Async runtime for watchdog + signals
nix = { version = "0.29", features = ["signal", "process"] }  # Unix process/signal management
chrono = { version = "0.4", features = ["serde"] }   # Timestamps
serde_json = "1"                                     # Event log, status file
tracing = "0.1"                                      # Structured logging
tracing-subscriber = "0.3"                           # Log output formatting
```

No database dependency. Metrics storage stays in the external `self-improvement` Python tool, which reads the JSONL event log.

---

## Migration Path

1. Build and test Milestone 1 alongside the running shell script
2. Run both in parallel on a few iterations, diff the output files to verify identical behavior
3. Swap in the harness, keep the shell script as a fallback
4. After 50+ successful iterations, delete `ralph-wiggums-loop.sh`
