# Evolvable Metrics System — V2 Spec

Replaces the rigid `self-improvement` Python tool with an evolvable metrics backend built into `simple-agent-harness`.

## Problem

The current system has three evolvability failures:

1. **Fixed-column sessions table.** Every new metric requires `ALTER TABLE`, updating INSERT/UPDATE/SELECT queries, updating the parser, updating the dashboard, and updating the brief generator. Adding "context window utilization" today would touch 6 functions across 200 lines.

2. **Hardcoded extraction rules.** The parser greps for `phpunit`, `lint:fix`, `bd-finish` — all specific to one WordPress plugin project. Moving to a different project (or even changing our toolchain within this project) means rewriting the parser.

3. **Enum constraints baked into schema.** Severity, status, and outcome are CHECK-constrained strings. Adding a new category means a migration, which SQLite doesn't handle gracefully (no ALTER CONSTRAINT).

## Design Principles

- **Schema-last**: The system accepts arbitrary key-value metrics. Schema (types, thresholds, display rules) is defined in config, not in the database.
- **Extract-via-config**: What to look for in session output is defined in pattern rules, not in code.
- **Append-only core**: The fundamental storage is an append-only event log. Materialized views and aggregations are derived and rebuildable.
- **Project-agnostic**: The harness knows nothing about WordPress, PHPUnit, or beads. It knows about "sessions that produce output files."

---

## Storage Model

### Events Table (append-only)

The single source of truth. One row per significant occurrence.

```sql
CREATE TABLE events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    ts        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    session   INTEGER NOT NULL,          -- global iteration number
    kind      TEXT NOT NULL,             -- event type (free-form, dotted namespace)
    value     TEXT,                      -- JSON value (number, string, object, array)
    tags      TEXT                       -- comma-separated free-form tags
);

CREATE INDEX idx_events_session ON events(session);
CREATE INDEX idx_events_kind ON events(kind);
CREATE INDEX idx_events_ts ON events(ts);
```

**No enums. No CHECK constraints.** The `kind` field is a free-form dotted string. Convention, not enforcement.

### Event Kinds (conventions, not schema)

Namespace pattern: `category.metric`

```
session.start              value: {"prompt_file": "PROMPT.md", "config": {...}}
session.end                value: {"exit_code": 0, "duration_secs": 1847, "output_bytes": 128456}
session.outcome            value: "completed" | "cutoff" | "failed" | "timeout" | "empty" | "rate_limited"
session.retry              value: {"attempt": 2, "reason": "empty", "prev_bytes": 0}

turns.total                value: 67
turns.narration_only       value: 4
turns.parallel             value: 8
turns.tool_calls           value: 142

cost.input_tokens          value: 1450000
cost.output_tokens         value: 38000
cost.estimate_usd          value: 24.57

watchdog.check             value: {"stale_secs": 60, "output_bytes": 48200, "growing": true}
watchdog.kill              value: {"stale_mins": 20, "final_bytes": 48200}

commit.detected            value: {"method": "bd-finish", "message": "FORM-01c: ..."}
commit.none                value: {"reason": "cutoff"}

# Project-specific (extracted via configurable patterns)
extract.test_runs          value: 3
extract.full_suite_runs    value: 1
extract.lint_runs          value: 1
extract.bead_id            value: "udgd"
extract.bead_title         value: "CHECKOUT-01b: Client-side JS validation"
```

New metrics appear by emitting new event kinds. No migration needed. The dashboard and brief generator query by kind pattern, so new kinds surface automatically in raw views and can be added to formatted views via config.

### Observations Table (derived, rebuildable)

A materialized per-session summary for fast dashboard queries. Rebuilt from events on demand.

```sql
CREATE TABLE observations (
    session   INTEGER PRIMARY KEY,       -- global iteration number
    ts        TEXT NOT NULL,
    duration  INTEGER,                   -- seconds
    outcome   TEXT,
    data      TEXT NOT NULL              -- JSON object: all metrics for this session
);
```

The `data` column is a JSON object assembled from all events for that session:

```json
{
  "turns.total": 67,
  "turns.narration_only": 4,
  "turns.parallel": 8,
  "turns.tool_calls": 142,
  "cost.estimate_usd": 24.57,
  "extract.bead_id": "udgd",
  "extract.test_runs": 3,
  "session.duration_secs": 1847,
  "session.output_bytes": 128456,
  "commit.detected": true
}
```

Rebuild command: `simple-agent-harness metrics rebuild` — drops and recreates from `events`.

### Improvements Table

Same purpose as V1 but with free-form fields instead of enums:

```sql
CREATE TABLE improvements (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ref        TEXT UNIQUE,              -- human-friendly ID: R1, R15, etc.
    created    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    resolved   TEXT,                     -- timestamp when fixed/closed
    severity   TEXT NOT NULL,            -- free-form: critical, high, medium, low, info
    status     TEXT NOT NULL DEFAULT 'open',  -- free-form: open, fixed, wontfix, monitoring, stale
    title      TEXT NOT NULL,
    body       TEXT,                     -- markdown description
    tags       TEXT,                     -- comma-separated
    meta       TEXT                      -- JSON blob for anything else
);

CREATE INDEX idx_improvements_status ON improvements(status);
```

No CHECK constraints. The CLI validates against a configurable set of allowed values, but the database accepts anything. This means you can add `status = "deferred"` or `severity = "critical"` without touching the schema.

---

## Configurable Extraction Rules

The parser is driven by rules in `harness.toml`, not hardcoded logic.

```toml
[metrics.extract]
# Each rule: scan session output for a pattern, emit an event

[[metrics.extract.rules]]
kind = "extract.bead_id"
# Scan tool_use commands for this regex. First capture group = value.
pattern = 'bd update (\S+) --status.?in.?progress'
transform = "last_segment"   # split on "-", take last segment
first_match = true           # stop after first match

[[metrics.extract.rules]]
kind = "commit.detected"
pattern = 'bd-finish'
emit = true                  # emit boolean true if pattern found anywhere

[[metrics.extract.rules]]
kind = "extract.bead_title"
pattern = 'bd-finish\.sh\s+\S+\s+"([^"]+)"'
first_match = true

[[metrics.extract.rules]]
kind = "extract.test_runs"
pattern = "phpunit"
source = "tool_commands"     # only scan tool_use command fields
count = true                 # emit count of matches, not the match itself

[[metrics.extract.rules]]
kind = "extract.full_suite_runs"
pattern = "phpunit"
anti_pattern = "--filter"    # only count matches that DON'T also match this
source = "tool_commands"
count = true

[[metrics.extract.rules]]
kind = "extract.lint_runs"
pattern = "lint:fix|lint.*composer"
source = "tool_commands"
count = true

# Easy to add new ones:
[[metrics.extract.rules]]
kind = "extract.file_reads"
pattern = '"name":\s*"Read"'
source = "raw"               # scan raw JSONL lines
count = true
```

**Rule fields:**
- `kind` — event kind to emit
- `pattern` — regex to search for
- `anti_pattern` — exclude matches that also match this (optional)
- `source` — where to search: `tool_commands` (tool_use input.command fields), `text` (assistant text blocks), `raw` (raw JSONL lines). Default: `tool_commands`.
- `transform` — post-processing: `last_segment` (split on `-`, take last), `int` (parse as integer), `trim` (strip whitespace). Default: raw capture group.
- `first_match` — stop after first match, emit the value. Default: false.
- `count` — emit count of matches instead of match content. Default: false.
- `emit` — emit a fixed value (true/false/string) if pattern is found. Default: not set.

Adding a new metric = adding 3-4 lines of TOML. No code changes.

### Built-in Metrics (not configurable)

These are always extracted by the harness because they come from the JSONL structure, not pattern matching:

- `turns.total` — count of `type: "assistant"` events
- `turns.narration_only` — assistant events with text blocks but no tool_use blocks
- `turns.parallel` — assistant events with 2+ tool_use blocks
- `turns.tool_calls` — total tool_use blocks across all assistant events
- `cost.input_tokens` / `cost.output_tokens` / `cost.estimate_usd` — from usage data
- `session.duration_secs` — wall clock time (from harness, not JSONL)
- `session.output_bytes` — output file size
- `session.exit_code` — agent process exit code

---

## Targets & Thresholds (configurable)

Replace hardcoded `>= 85%`, `< 20%`, `> 10%` with config:

```toml
[metrics.targets]
# Each target: a metric kind, a comparison, a threshold, and display info

[[metrics.targets.rules]]
kind = "turns.narration_only"
compare = "pct_of"           # compute as percentage of another metric
relative_to = "turns.total"
threshold = 20
direction = "below"          # "below" = good when under threshold
label = "Narration-only turns"
unit = "%"

[[metrics.targets.rules]]
kind = "turns.parallel"
compare = "pct_of"
relative_to = "turns.total"
threshold = 10
direction = "above"
label = "Parallel tool calls"
unit = "%"

[[metrics.targets.rules]]
kind = "commit.detected"
compare = "pct_sessions"     # percentage of sessions where this event exists
threshold = 85
direction = "above"
label = "Completion rate"
unit = "%"

[[metrics.targets.rules]]
kind = "turns.total"
compare = "avg"
threshold = 80
direction = "below"
label = "Avg turns per session"

[[metrics.targets.rules]]
kind = "cost.estimate_usd"
compare = "avg"
threshold = 30
direction = "below"
label = "Avg cost per session"
unit = "$"

# Project-specific target — easy to add/remove
[[metrics.targets.rules]]
kind = "extract.lint_runs"
compare = "avg"
threshold = 2
direction = "below"
label = "Lint runs per session"
```

The dashboard and brief generator iterate over these rules dynamically. Adding a target = adding TOML. Removing one = deleting TOML. No code changes.

---

## CLI Interface

Subcommands under `simple-agent-harness metrics` (or standalone `sah-metrics` alias):

```
simple-agent-harness metrics <COMMAND>

Commands:
  log <file>                   Parse JSONL file, emit events, update observations
  status [--last N]            Dashboard: recent sessions, target status, trends
  analyze [--last N]           Deep analysis with recommendations
  brief [--last N]             Performance snippet for prompt injection
  targets                      Show configured targets vs recent performance
  query <kind> [--last N]      Raw query: show values for a metric kind across sessions
  events [--session N]         Dump raw events (optionally filtered by session)
  rebuild                      Rebuild observations table from events
  export [--format csv|json]   Export all observations

  improvement add <title> --severity <sev>
  improvement fix <ref> [--impact "..."]
  improvement list [--status <s>]
  improvement search <query>
```

### New: `query` command

Ad-hoc metric exploration without writing SQL:

```bash
$ sah metrics query extract.test_runs --last 10
Session  Value
  348    3
  347    2
  346    0      # empty session
  345    4
  ...

$ sah metrics query cost.estimate_usd --last 20 --aggregate avg
Average cost.estimate_usd over last 20 sessions: $22.41

$ sah metrics query turns.parallel --last 10 --aggregate trend
turns.parallel trend (last 10): 0 0 0 2 3 1 5 4 6 8  ↑ improving
```

### Updated: `brief` command

Reads targets from config, computes each one against recent sessions, emits warnings for misses and streaks. No hardcoded metric names.

```
## PERFORMANCE FEEDBACK (auto-generated)

Last session #348:
  Narration-only turns: 6% (target: <20%) — OK
  Parallel tool calls:  12% (target: >10%) — OK
  Avg turns: 67 (target: <80) — OK
  Lint runs: 1 (target: <2) — OK

All targets met. Keep it up.
```

When a target is missed for 3+ consecutive sessions, the brief escalates to a WARNING block (same as V1, but driven by config).

---

## Migration from V1

The V1 `self-improvement.db` has ~350 session rows. Migration path:

1. Read all rows from V1 `sessions` table
2. For each row, emit events into the new `events` table:
   - `turns.total` = `assistant_turns`
   - `turns.narration_only` = `narration_only_turns`
   - `turns.parallel` = `parallel_turns`
   - `turns.tool_calls` = `tool_calls`
   - `cost.estimate_usd` = `cost_estimate`
   - `extract.test_runs` = `test_runs`
   - `extract.lint_runs` = `lint_runs`
   - `extract.full_suite_runs` = `full_suite_runs`
   - `extract.bead_id` = `bead_id`
   - `extract.bead_title` = `bead_title`
   - `session.outcome` = `outcome`
   - `commit.detected` = `committed` (as boolean)
3. Rebuild `observations` table
4. Copy `improvements` rows directly (schema is compatible)
5. Copy `analysis_runs` into events as `analysis.run` events (or discard — they're derivable)

Command: `simple-agent-harness metrics migrate --from tools/self-improvement.db`

---

## Implementation Scope

This is part of the `simple-agent-harness` Rust binary, not a separate tool.

### Milestone 1 (with harness MVP)
- `events` table + `observations` table creation
- Built-in metric extraction (turns, cost, duration)
- `log` command (parse JSONL → emit events → update observations)
- `status` command (basic dashboard from observations)

### Milestone 2 (with harness observability)
- Configurable extraction rules from `harness.toml`
- Configurable targets from `harness.toml`
- `brief` command (driven by targets config)
- `query` command

### Milestone 3 (with harness hooks)
- `analyze` command with trend comparison
- `improvement` subcommands
- `migrate` command for V1 data
- `export` command

### Milestone 4 (polish)
- `rebuild` command
- `events` dump command
- Streak detection in brief (configurable streak window)
- Per-project config profiles (different targets for different repos)
