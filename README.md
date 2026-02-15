# Blacksmith

A supervised agent harness that runs an AI coding agent in a loop — dispatching prompts, monitoring sessions, enforcing health invariants, collecting metrics, and repeating.

## Install

```bash
cargo build --release
```

The binary is at `target/release/simple-agent-harness`.

## Quick Start

1. Create a `PROMPT.md` with the instructions for your agent.
2. Run:

```bash
simple-agent-harness
```

That's it. With no config file, sensible defaults apply: runs `claude` for up to 25 productive iterations, monitors for stale sessions, retries empty outputs, and handles rate limits with exponential backoff.

## Usage

```
simple-agent-harness [OPTIONS] [MAX_ITERATIONS]
```

### Arguments

| Argument | Description |
|---|---|
| `MAX_ITERATIONS` | Override max productive iterations (default: from config) |

### Options

| Flag | Description |
|---|---|
| `-c, --config <PATH>` | Config file path (default: `harness.toml`) |
| `-p, --prompt <PATH>` | Prompt file (overrides config) |
| `-o, --output-dir <PATH>` | Output directory (overrides config) |
| `--timeout <MINUTES>` | Stale timeout in minutes (overrides config) |
| `--retries <N>` | Max empty retries (overrides config) |
| `-v, --verbose` | Debug-level logging (watchdog checks, retry decisions) |
| `-q, --quiet` | Warn-level logging only |
| `--dry-run` | Validate config and print resolved settings, don't run |
| `--status` | Print current loop state and exit |

### Examples

```bash
# Run with defaults (reads harness.toml if present, else uses defaults)
simple-agent-harness

# Run 10 iterations with verbose logging
simple-agent-harness -v 10

# Use a custom config and prompt
simple-agent-harness -c my-harness.toml -p my-prompt.md

# Validate your config without running
simple-agent-harness --dry-run

# Check if a harness is currently running
simple-agent-harness --status
```

## Configuration

All configuration is optional. Create a `harness.toml` to override defaults:

```toml
[session]
max_iterations = 25              # Productive iterations before exiting
prompt_file = "PROMPT.md"        # Agent prompt file
output_dir = "."                 # Where session output files go
output_prefix = "claude-iteration"  # Output filename prefix
counter_file = ".iteration_counter" # Persists iteration count across runs

[agent]
command = "claude"               # Agent command to run
args = ["-p", "{prompt}", "--dangerously-skip-permissions", "--verbose", "--output-format", "stream-json"]
# {prompt} is replaced with the assembled prompt text

[watchdog]
check_interval_secs = 60        # How often to check for output growth
stale_timeout_mins = 20          # Kill session if no output for this long
min_output_bytes = 100           # Minimum bytes for a "productive" session

[retry]
max_empty_retries = 2            # Retries for empty/short sessions
retry_delay_secs = 5             # Delay between retries

[backoff]
initial_delay_secs = 2           # Initial rate-limit backoff
max_delay_secs = 600             # Cap backoff at 10 minutes
max_consecutive_rate_limits = 5  # Exit after this many consecutive rate limits

[shutdown]
stop_file = "STOP"               # Touch this file to stop the loop gracefully

[hooks]
pre_session = []                 # Shell commands to run before each session
post_session = []                # Shell commands to run after each session

[prompt]
# file = "PROMPT.md"             # Alternative to session.prompt_file
prepend_commands = []            # Commands whose stdout is prepended to the prompt

[output]
# event_log = "harness-events.jsonl"  # Append-only JSONL event log

[commit_detection]
patterns = ["bd-finish", "(?i)git commit", "(?i)\\bcommitted\\b"]
```

**Precedence:** Defaults < Config file < CLI flags

## Features

### Watchdog

Monitors agent output in real time. If the session produces no new output for `stale_timeout_mins`, the process is killed (SIGTERM, then SIGKILL after 5s) and the iteration proceeds.

### Retry Logic

Sessions producing fewer than `min_output_bytes` are retried up to `max_empty_retries` times. Retries don't count as productive iterations and don't trigger post-session hooks.

### Rate Limit Detection

Inspects the final result event in session JSONL output for `is_error: true` with rate-limit keywords. Successful sessions are never classified as rate-limited, even if the agent discussed rate limiting in its output.

### Exponential Backoff

On rate limits: `initial_delay * 2^consecutive_count`, capped at `max_delay_secs`. Resets after a successful session. Exits the loop after `max_consecutive_rate_limits` consecutive hits.

### Commit Detection

Scans session output for configurable regex patterns (default: `git commit`, `committed`, `bd-finish`). Results are reported in event logs and passed to post-session hooks.

### Hooks

Shell commands that run before/after each session:

```toml
[hooks]
pre_session = ["git pull --rebase"]
post_session = ["./notify.sh"]
```

**Pre-session environment:**
- `HARNESS_ITERATION` — current productive iteration
- `HARNESS_GLOBAL_ITERATION` — total iterations including retries
- `HARNESS_PROMPT_FILE` — path to the prompt file

**Post-session environment** (all of the above, plus):
- `HARNESS_OUTPUT_FILE` — path to the session output file
- `HARNESS_EXIT_CODE` — agent process exit code
- `HARNESS_OUTPUT_BYTES` — bytes written to output
- `HARNESS_SESSION_DURATION` — session wall-clock time in seconds
- `HARNESS_COMMITTED` — `true` if commit patterns were detected

Pre-hook failure skips the iteration. Post-hook failures are logged but don't stop the loop.

### Prompt Assembly

The prompt sent to the agent is assembled from:
1. Output of `prepend_commands` (separated by `---`)
2. Contents of the prompt file

```toml
[prompt]
prepend_commands = ["git diff --stat", "bd ready"]
```

### Graceful Shutdown

Multiple ways to stop the loop:
- **STOP file** — `touch STOP` triggers a clean exit (file is deleted on detection)
- **SIGINT** — first signal finishes the current session then exits
- **Double SIGINT** — second signal within 3s force-kills the agent immediately
- **SIGTERM** — same as first SIGINT

### Status File

A `harness.status` JSON file is maintained in the output directory with current state, iteration counts, PID, uptime, and output info. Query it with `--status`.

### Event Log

Optional append-only JSONL log with one entry per session:

```toml
[output]
event_log = "harness-events.jsonl"
```

Each entry includes: timestamp, iteration numbers, output bytes, exit code, duration, committed flag, retry count, and rate-limit flag.

## Files

| File | Purpose |
|---|---|
| `harness.toml` | Configuration (optional) |
| `PROMPT.md` | Agent prompt |
| `.iteration_counter` | Persists iteration count across runs |
| `STOP` | Touch to trigger graceful shutdown |
| `harness.status` | Current loop state (JSON) |
| `claude-iteration-{N}.jsonl` | Session output files |
| `harness-events.jsonl` | Event log (if configured) |

## Testing

```bash
cargo test
```
