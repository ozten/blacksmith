# Automated Architecture Analysis and Two-Phase Task Metadata

## Motivation

The companion document ([Multi-Agent Repo Coordination](./multi-agent-repo-coordination.md)) describes the core workflow: task scheduling, worktree isolation, and mechanical integration loops. That design assumes a human writes tasks, estimates affected sets, and notices when the codebase structure degrades. This document removes the human from those loops.

The goal is a system where a human says "build feature X" and the system decomposes, schedules, executes, integrates, and restructures the codebase autonomously. Humans review outputs, not process.

---

## The Case for Automation

Three responsibilities in the core workflow resist human-out-of-the-loop operation:

1. **Estimating affected sets.** A human tags each task with the modules it touches. This is tedious, error-prone, and stale the moment another task integrates.

2. **Detecting architectural decay.** A human notices that integration keeps failing around the same module and decides to refactor. This requires pattern recognition across many tasks over time — something humans are bad at tracking and slow to act on.

3. **Updating stale metadata after refactors.** A refactor changes file paths and module boundaries. Every pending task's affected-set metadata is now wrong. A human has to go update tickets.

All three share a property: they're derivable from information the system already has. The codebase has an import graph. The task system has a history of integration failures. The issue tracker has task descriptions. Automating these isn't speculative — it's connecting data sources that already exist.

---

## Two-Phase Task Metadata

### The Problem with Static Affected Sets

In the base system, a task's affected set is written once at planning time and treated as ground truth. This fails in two ways:

- **Stale after refactors.** Task 12 restructures the module tree. Tasks 13–20, planned against the old structure, now reference files that don't exist.
- **Wrong from the start.** The planner guesses that "add rate limiting" touches `auth` and `middleware`. It actually also touches `config` and `tests/integration`. The affected set was always incomplete.

### Metadata as Materialized View

Task metadata is a projection of task intent onto the current codebase state. When either input changes (the task is redefined, or the codebase moves forward), the projection is recomputed.

```
issue tracker                codebase (HEAD of main)
     │                              │
     │  intent, deps,               │  file tree, import graph,
     │  acceptance criteria          │  public API surface
     │                              │
     └─────────────┬────────────────┘
                   │
            derivation engine
                   │
                   ▼
           task metadata (cached)
           ├── affected_files: [...]
           ├── affected_modules: [...]
           ├── blast_radius: [...]
           ├── boundary_signatures: [...]
           ├── estimated_conflicts: [...]
           └── base_commit: abc123
```

The metadata is as detailed as you want — files, line ranges, function signatures, estimated conflicts with other tasks. It just isn't permanent. It's regenerated whenever main advances.

### Layer 1: Intent Analysis (Slow, Stable)

An LLM reads the issue and produces a semantic understanding of what areas of the codebase are involved. This is the expensive step and is cached against the issue content hash.

```toml
[intent_analysis]
task_id = "task-13"
content_hash = "a8f3c1..."  # hash of issue title + description + acceptance criteria
target_areas = [
    { concept = "auth_endpoints", reasoning = "Rate limiting applies to auth API surface" },
    { concept = "middleware_stack", reasoning = "Rate limiting is typically a middleware concern" },
    { concept = "config", reasoning = "Rate limit thresholds need to be configurable" },
]
```

This analysis survives refactors. "Auth endpoints" is a concept, not a file path. Even if every file in the auth module is renamed or split, the intent analysis remains valid. It only invalidates when the issue itself is edited (scope change, redefinition).

### Layer 2: File Resolution (Fast, Volatile)

A static analysis pass maps concepts from layer 1 onto concrete files and modules at the current HEAD.

```toml
[file_resolution]
task_id = "task-13"
base_commit = "abc123"
intent_hash = "a8f3c1..."  # links back to which intent analysis produced this

[[file_resolution.mappings]]
concept = "auth_endpoints"
resolved_files = ["src/auth/handlers.rs", "src/auth/routes.rs"]
resolved_modules = ["auth"]

[[file_resolution.mappings]]
concept = "middleware_stack"
resolved_files = ["src/middleware/mod.rs", "src/middleware/chain.rs"]
resolved_modules = ["middleware"]

[[file_resolution.mappings]]
concept = "config"
resolved_files = ["src/config/mod.rs", "src/config/rate_limits.rs"]
resolved_modules = ["config"]

[file_resolution.derived]
affected_modules = ["auth", "middleware", "config"]
blast_radius = ["auth", "middleware", "config", "api"]  # includes transitive dependents
boundary_signatures = [
    "pub fn auth::handlers::login(req: Request) -> Response",
    "pub trait middleware::Middleware",
]
```

This invalidates every time main advances. Regeneration is cheap because it's pure static analysis — parse the import graph, locate symbols, compute transitive closure. No LLM involved.

### Regeneration Strategy

Not every commit to main requires regenerating every task's metadata. The system can be lazy:

- **On scheduling.** When the scheduler considers a task for assignment, check if `base_commit` matches current main. If not, regenerate layer 2 for that task. Only tasks that are candidates for assignment pay the regeneration cost.
- **On integration.** After a task integrates to main, mark all cached layer-2 data as potentially stale. Don't regenerate immediately — wait until the scheduler needs it.
- **On refactor integration.** If the integrated task was flagged as a refactor (structural changes, not just feature work), proactively regenerate layer 2 for all pending tasks. Refactors are more likely to invalidate metadata than feature work.

```rust
fn ensure_fresh_metadata(task: &Task, current_main: CommitHash) -> TaskMetadata {
    let intent = match intent_cache.get(task.id) {
        Some(cached) if cached.content_hash == task.content_hash() => cached,
        _ => {
            let analysis = llm_analyze_intent(task);
            intent_cache.set(task.id, analysis.clone());
            analysis
        }
    };

    let resolution = match resolution_cache.get(task.id) {
        Some(cached) if cached.base_commit == current_main => cached,
        _ => {
            let resolved = resolve_against_codebase(&intent, current_main);
            resolution_cache.set(task.id, resolved.clone());
            resolved
        }
    };

    TaskMetadata::from(intent, resolution)
}
```

### What This Eliminates

- Humans never write affected sets. The system derives them.
- Refactors never stale pending tasks. Layer 2 regenerates automatically.
- The planner doesn't need to understand file structure. It works with concepts.
- The migration map from refactors is unnecessary for the planning layer. (It's still useful for in-progress worktrees where an agent's files moved mid-task.)

---

## Automated Architecture Agent

### Purpose

The architecture agent detects when the codebase structure is degrading relative to the multi-agent workflow's needs, and produces refactor tasks that enter the normal planning queue. It doesn't execute refactors — it identifies them and creates issues.

### Input Signals

The agent consumes two categories of input:

#### Static Analysis of Current Codebase

Run against HEAD of main. These are the structural smells:

**Fan-in hotspots.** Files imported by a high percentage of the codebase. If `models.rs` is imported by 30 of 40 modules, any task touching it conflicts with almost everything.

```
fan_in_score(file) = count(modules importing file) / total_modules
```

Files above a threshold (e.g., 0.3) are splitting candidates.

**God files.** Files exceeding a size threshold that contain multiple unrelated concerns. Detected by measuring internal cohesion — do the symbols defined in the file reference each other, or are they independent clusters?

**Circular dependencies.** Module A imports B, B imports A. This makes affected sets transitive in both directions, effectively merging them from the scheduler's perspective.

**Boundary violations.** Modules reaching into each other's internals rather than using public APIs. Detected by checking whether imports reference `pub` items or reach through `pub(crate)` / non-public paths.

**Wide public API surface.** Modules that export many symbols create large blast radii. A module exporting 50 public functions is harder to isolate in the task graph than one exporting 5.

#### Historical Signal from the Task System

Accumulated across task completions over time:

**Affected-set expansions.** Tasks where the coding agent discovered it needed modules not in its original metadata. Each expansion is a data point: "the derivation engine predicted these modules, but the agent actually needed these." Frequent expansions around the same module indicate its boundaries don't contain changes well.

```toml
[[expansion_event]]
task_id = "task-47"
predicted_modules = ["auth"]
actual_modules = ["auth", "models", "middleware"]
expansion_reason = "Auth changes required updating shared model types"
timestamp = "2025-03-15T10:30:00Z"
```

**Integration loop iterations.** Tasks where the integration fix loop ran more than once. High iteration counts mean the interfaces between modules were unclear or the blast radius was underestimated.

**Entangled rollbacks.** Tasks where reverting one required cascading reverts to others. Each entanglement is a missing dependency edge that the module structure should have made explicit.

**Metadata drift.** When layer-2 regeneration produces significantly different results for the same layer-1 intent. If "add a feature to auth" resolved to 3 files last week and 11 files this week, the auth module's boundaries are expanding. This signal is unique to the two-phase caching model — it's an early warning that doesn't require any integration failures to fire.

### Analysis Process

The architecture agent runs on triggers:

- **Periodic.** After every N tasks complete (e.g., 10), run full analysis.
- **Threshold-triggered.** When rolling averages of expansion events or integration iterations exceed a threshold.
- **Pre-planning.** Before decomposing a large feature into tasks, run analysis to ensure the codebase can support the expected parallelism.

The analysis proceeds in stages:

1. **Compute structural metrics** for all modules (fan-in, size, cohesion, cycle participation, API surface width).
2. **Correlate with historical signals.** Overlay expansion events, integration struggles, and entanglements onto the module graph. Weight recent events more heavily.
3. **Identify candidates.** Modules that score high on structural smells AND appear frequently in historical failure signals are strong splitting/refactoring candidates. Modules with only structural smells but no operational problems are left alone — the architecture is serving the workflow fine despite looking messy.
4. **Generate proposals.** For each candidate, produce a specific, actionable refactoring proposal.

### Proposal Format

```toml
[[proposal]]
id = "arch-007"
kind = "split_module"
priority = "high"
confidence = 0.85

[proposal.target]
file = "src/models.rs"
current_fan_in = 0.72  # imported by 72% of modules
current_line_count = 1847

[proposal.evidence]
expansion_events = 6      # in last 20 tasks
integration_iterations = 3.2  # average for tasks touching this file
entangled_rollbacks = 2
metadata_drift = "resolved files increased from 3 to 11 over 15 commits"

[proposal.recommendation]
description = "Split models.rs into domain-specific type modules"
suggested_structure = [
    { path = "src/auth/types.rs", symbols = ["User", "Session", "Token", "AuthError"] },
    { path = "src/billing/types.rs", symbols = ["Invoice", "LineItem", "PaymentMethod"] },
    { path = "src/shared/types.rs", symbols = ["Timestamp", "Pagination", "Id"] },
]

[proposal.impact]
estimated_max_fan_in_after = 0.25
tasks_with_stale_metadata = ["task-53", "task-54", "task-57"]
```

### Proposal Validation

Before a proposal becomes a refactor task, it passes automated checks:

- **Dependency feasibility.** Does the proposed split create circular dependencies? Run the proposed module graph through a cycle detector.
- **API surface preservation.** Do all current consumers of the module still have access to the symbols they need through the new structure? Walk the import graph against the proposed layout.
- **Test coverage.** Are there existing tests that exercise the boundaries being changed? If not, flag that tests should be added as part of the refactor task.
- **Conflict check.** Would the refactor task conflict with any currently in-progress tasks? If so, defer until those complete.

Proposals that pass validation are converted to refactor tasks in the issue tracker and enter the normal scheduling flow. Refactor tasks are treated as epoch boundaries — no other task runs concurrently with them.

### The Feedback Loop

```
codebase state + task history
         │
         ▼
  architecture analysis
         │
         ▼
  splitting proposals
         │
         ▼
  refactor tasks (epoch boundaries)
         │
         ▼
  cleaner module boundaries
         │
         ▼
  smaller derived affected sets
         │
         ▼
  better parallelism, fewer integration failures
         │
         ▼
  improved task history signals
         │
         └──────────────────────────────► back to top
```

The system converges toward a module structure optimized for multi-agent work. This structure is empirically grounded in actual conflict data rather than aesthetic preference. The architecture evolves in response to measured operational pressure.

---

## Refactors and In-Progress Work

The hardest case: a refactor task integrates while another agent is mid-task in a worktree built against the old structure.

### The Migration Map

Even though the planning system doesn't need migration maps (layer-2 metadata regenerates automatically), in-progress worktrees do. The refactor agent produces a structured mapping:

```toml
[[moves]]
from = "src/models.rs"
to_split = [
    "src/auth/types.rs",
    "src/billing/types.rs",
    "src/shared/types.rs",
]

[[symbol_relocations]]
symbol = "User"
old_path = "crate::models::User"
new_path = "crate::auth::types::User"

[[symbol_relocations]]
symbol = "Invoice"
old_path = "crate::models::Invoice"
new_path = "crate::billing::types::Invoice"
```

### Handling the In-Progress Agent

When a refactor integrates to main and another agent has an in-progress worktree:

1. **Notify the agent** that main has structurally changed.
2. **Provide the migration map** alongside the notification.
3. **The agent (or integrator) applies the map** to update imports and paths in the worktree branch.
4. **Merge main into the branch** (the normal integration step, which now includes the refactor).
5. **Run the fix loop** as usual.

Step 3 is the new piece. It's mechanical — find-and-replace on import paths guided by the symbol relocation table. If the agent's work only *uses* symbols that moved (it imports `User`), the fix is just updating import paths. If the agent *modified* a file that was split, the situation is harder — its changes need to be routed to the correct new file. This is where the integration loop earns its keep, because the compiler will tell you exactly what's missing where.

### When to Abort Instead

If the refactor fundamentally changes the approach the in-progress agent was taking (not just file paths but APIs, semantics, or design patterns), a mechanical migration won't work. The circuit breaker applies: if the fix loop exceeds N iterations after applying the migration map, abort the task and re-queue it. The re-queued task gets fresh layer-2 metadata derived against the post-refactor codebase and starts clean.

---

## Integration with Issue Trackers

The issue tracker is the source of truth for task intent and explicit dependencies. The automated system layers on top of it without replacing it.

### What Lives in the Issue Tracker

- Task descriptions and acceptance criteria (consumed by layer-1 intent analysis).
- Explicit dependency edges (hard constraints for the scheduler).
- Human-authored context, design decisions, and constraints.
- Status tracking and audit trail.

### What the System Derives and Stores Separately

- Layer-1 intent analysis (cached, keyed to issue content hash).
- Layer-2 file resolution (cached, keyed to commit hash).
- Historical signals (expansion events, integration metrics, entanglement records).
- Architecture proposals and their evidence.

The system reads from the issue tracker but writes derived data to its own store. This keeps the issue tracker clean and human-readable while the system maintains its operational metadata separately.

### Syncing

When an issue is updated (scope change, new acceptance criteria), the content hash changes and layer-1 intent analysis is invalidated on next access. When the system creates refactor tasks from architecture proposals, it writes them back to the issue tracker as new issues with full context and evidence.

---

## Operational Boundaries

### What This System Automates

- Affected-set estimation (replacing human tagging on tickets).
- Stale metadata recovery after refactors (replacing human ticket updates).
- Architectural smell detection and refactor proposal generation.
- Migration mapping for in-progress work during refactors.

### What Still Benefits from Human Judgment

- Approving or rejecting architecture proposals that change major structural decisions (e.g., merging two crates, introducing a new shared library). The system proposes; a human can review if desired.
- Resolving ambiguous task intent where the issue description is underspecified and the LLM's intent analysis has low confidence.
- Setting policy parameters: fan-in thresholds, integration loop circuit breaker limits, how aggressively to split modules versus tolerating some coupling.

### Knobs

| Parameter | What it controls | Conservative | Aggressive |
|-----------|-----------------|--------------|------------|
| `fan_in_threshold` | When a file is flagged as a splitting candidate | 0.5 | 0.2 |
| `integration_loop_max` | Circuit breaker for fix loop iterations | 5 | 2 |
| `expansion_event_threshold` | How many expansions before triggering architecture review | 5 in 20 tasks | 2 in 10 tasks |
| `metadata_drift_sensitivity` | How much file-count change triggers a drift signal | 3x increase | 1.5x increase |
| `refactor_auto_approve` | Whether refactor proposals execute without human review | false | true |

Start conservative. Tighten as you build confidence in the analysis quality.

---

## Convergence Properties

The combined system has a natural convergence dynamic:

1. **Bad module boundaries** cause integration failures and affected-set expansions.
2. **The architecture agent** detects the pattern and proposes splits.
3. **Splits create smaller, more cohesive modules** with narrower public APIs.
4. **Smaller modules** produce smaller affected sets in layer-2 derivation.
5. **Smaller affected sets** mean fewer scheduling conflicts and better parallelism.
6. **Better parallelism** means fewer expansion events and integration struggles.
7. **Fewer signals** mean the architecture agent proposes less, settling into a steady state.

The system finds its own equilibrium. The rate of architectural change decreases as the codebase structure aligns with the operational demands of multi-agent work. The equilibrium point is determined by the policy knobs — aggressive settings produce finer-grained modules and higher parallelism at the cost of more refactor overhead.

## Open Questions

1. Where does the architecture agent implementation live? Is it a skill or part of the blacksmith PROMPT or binary? Should we update the prd-to-beads SKILL or does this happen earlier in the workflow?
2. Some languages and frameworks make module detection easier and some harder? Can we always do this analysis or should we punt on some codebases?
3. Can we output mermaid or other type of diagrams so that the user can see the module architecture evolve over time?