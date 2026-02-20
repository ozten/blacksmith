# Blacksmith V7 — Speck Integration for Affected-Set Scheduling

Blacksmith's scheduler uses `affected:` declarations on beads to avoid scheduling conflicting tasks in parallel. Today these declarations are hand-authored and frequently missing or stale. This spec adds integration hooks so that [speck](../speck/) — the planning and verification tool — can derive, maintain, and refresh affected-set metadata automatically.

Builds on V3 (multi-agent coordination and conflict-aware scheduling). Complements SPEC-v5 (automated architecture and metadata) by externalizing the derivation engine to speck rather than building it into blacksmith.

---

## Problem

Three failure modes in the current affected-set workflow:

1. **Missing declarations.** Beads created by `prd-to-beads` or manually often omit the `affected:` line. The scheduler treats these optimistically — it allows them to run in parallel with anything. Conflicts are caught at integration time (merge failures, test failures), wasting an entire agent session.

2. **Wrong declarations.** Even when present, hand-authored affected sets are guesses. The author estimates which files a task will touch before the work starts. These estimates are frequently incomplete — an agent discovers mid-task that it also needs to modify `config.rs` or `tests/integration/`. The scheduler made a parallelism decision based on stale data.

3. **Stale after integration.** When a refactor task integrates to main and moves files around, every pending bead's affected set references the old file structure. There's no mechanism to update them. The scheduler either over-serializes (blocking on files that no longer exist at those paths) or under-serializes (missing conflicts on the renamed files).

All three are derivation problems. Blacksmith has the scheduling engine but not the codebase understanding needed to derive affected sets. Speck has the codebase understanding (codebase map, task spec exploration, abstract-to-concrete resolution) but no connection to the scheduler.

---

## Design Principle

**Blacksmith consumes affected sets. Speck produces them.**

Blacksmith does not gain codebase analysis capabilities. It remains a harness — it reads `affected:` lines from bead descriptions and schedules based on glob overlap. The derivation, resolution, and refresh logic lives entirely in speck.

The integration is through bead metadata: speck writes `affected:` lines into bead descriptions via `spec sync beads`, and blacksmith reads them via its existing `parse_affected_set()`.

---

## Integration Architecture

```
spec plan (explores codebase, decomposes requirements)
    │
    ├── task specs with abstract refs + concrete file lists
    │
    └── spec sync beads
        │
        └── writes affected: globs into bead descriptions
            │
            blacksmith scheduler reads them (unchanged)
            │
            blacksmith integrates task to main
            │
            post-integration hook
            │
            └── spec map --refresh + spec sync beads --refresh
                │
                └── re-resolves pending beads against new HEAD
```

---

## Changes to Blacksmith

### 1. Post-integration hook: `speck_refresh`

After a successful integration (merge to main, tests pass, bead closed), blacksmith runs an optional post-integration command to refresh affected-set metadata on pending beads.

**Config:**

```toml
[hooks]
post_integration = "spec sync beads --refresh"
```

When set, blacksmith shells out to this command after each successful integration. The command receives the integrated bead ID and new main commit hash as environment variables:

```
BLACKSMITH_INTEGRATED_BEAD=speck-e03
BLACKSMITH_MAIN_COMMIT=abc123f
```

If the command fails, blacksmith logs a warning and continues. Stale metadata is a scheduling quality issue, not a correctness issue — the integration loop still catches real conflicts.

**Affected files:** `src/coordinator.rs` (post-integration step), `src/config.rs` (new `hooks` config section)

### 2. Affected-set fallback parsing

When a bead has no explicit `affected:` line but does have a markdown section listing files (e.g., `## Affected files` with bullet points), `parse_affected_set()` falls back to extracting file paths from that section.

This handles beads authored before speck integration or by tools that use markdown conventions instead of the `affected:` format.

**Parsing rules:**
- Scan for a heading containing "affected" (case-insensitive): `## Affected files`, `### Affected Files`, etc.
- Extract lines that start with `•`, `-`, or `*` followed by a file path (detected by containing `/` or ending in a known extension)
- Strip annotations like `(new)`, `(modified)`, `(deleted)`
- Return the file paths as literal globs (no wildcards — these are explicit file lists)

**Affected files:** `src/scheduler.rs` (extend `parse_affected_set`)

### 3. Affected-set provenance logging

When the scheduler assigns a task, log the source of the affected set:

```
INFO assigning bead=speck-e03 affected_source=explicit affected_count=9
INFO assigning bead=speck-9p7 affected_source=markdown_fallback affected_count=4
WARN assigning bead=speck-xyz affected_source=none (optimistic scheduling)
```

This makes it visible which beads are benefiting from speck-derived metadata vs. falling back to optimistic scheduling.

**Affected files:** `src/scheduler.rs` (logging in `schedule_next`), `src/coordinator.rs` (logging at assignment time)

### 4. Expansion event recording

When the integration loop detects that an agent modified files outside its declared affected set, record an expansion event in the database.

**Detection:** After a successful integration, diff the branch against its merge base. Compare the set of actually-modified files against the bead's declared `affected:` globs. Files modified but not covered by any glob are expansions.

**Storage:**

```sql
CREATE TABLE affected_set_expansions (
    id INTEGER PRIMARY KEY,
    bead_id TEXT NOT NULL,
    predicted_globs TEXT,         -- JSON array of declared globs
    actual_files TEXT,            -- JSON array of files the agent actually modified
    expansion_files TEXT,         -- JSON array of files not covered by any declared glob
    recorded_at TEXT NOT NULL
);
```

**Purpose:** This data feeds back to speck. Speck can query expansion history to improve its derivation accuracy — if tasks about "auth" consistently expand to include "config", speck learns to include config in future auth-related affected sets.

**Affected files:** `src/integrator.rs` (expansion detection after merge), `src/coordinator.rs` (DB table creation), new migration

---

## What Does NOT Change

- **Scheduler algorithm.** `next_assignable_tasks()` and `globs_overlap()` are unchanged. They continue to operate on `affected_globs: Option<Vec<String>>`.
- **Optimistic scheduling.** Beads with no affected set (from any source) are still allowed to run. The integration loop remains the safety net.
- **Bead format.** The `affected:` line format is unchanged. Speck writes the same format that humans would.
- **Integration loop.** Merge, build, test, fix — unchanged. Expansion detection is an observability addition, not a behavior change.

---

## Verification

### Post-integration hook
- Unit test: config parses `[hooks] post_integration = "..."` correctly
- Integration test: mock a successful integration, verify the hook command is invoked with correct env vars
- Integration test: hook failure is logged as warning, does not block subsequent scheduling

### Fallback parsing
- Unit test: markdown with `## Affected files` and bullet list extracts correct paths
- Unit test: `(new)` / `(modified)` annotations are stripped
- Unit test: explicit `affected:` line takes precedence over markdown fallback
- Unit test: no heading = returns `None` (same as today)

### Provenance logging
- Manual: run blacksmith with a mix of beads (explicit, markdown, none) — verify log lines show correct sources

### Expansion recording
- Unit test: given declared globs and actual file list, correctly identifies expansion files
- Integration test: after integration, expansion event is written to DB
- Manual: query `affected_set_expansions` table after a run, verify data makes sense

---

## Sequencing

1. **Fallback parsing** (unblocks immediate use with existing beads)
2. **Provenance logging** (visibility into scheduling decisions)
3. **Post-integration hook** (enables speck refresh loop)
4. **Expansion recording** (feeds back to speck for derivation improvement)

Items 1–2 are useful standalone, even before speck exists. Items 3–4 require speck's `sync beads --refresh` to be implemented.
