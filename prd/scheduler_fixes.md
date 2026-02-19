# PRD: Scheduler & Coordinator Fixes from cantrip Test Run

## Context

A test run of blacksmith against `../cantrip` (3 workers, codex adapter, 27 beads) revealed several issues in the coordinator and scheduler. The run completed only 2 integrations in ~5 minutes before the user killed it due to log spam from the scheduler spinning in a tight loop.

Key observations:
- `bd list --json` returns `issue_type` ("task" or "epic") and `dependencies` with `type: "parent-child"` — sufficient to detect epics with children at scheduling time.
- Epics have acceptance criteria "All child issues completed and integrated" — they are containers, not directly implementable work.
- Worktrees are created fresh from main, so the bd database in each worktree is stale relative to the JSONL that was synced into main.

## Fix 1: Filter epics with open children from the scheduling pool

**Problem**: Epics get assigned to workers. Agents either waste a full session discovering the epic is too large (session 7: cantrip-z95.4), or spend many turns exploring before working on a child (session 9: cantrip-z95.2). The scheduler should only schedule leaf work items.

**Solution**: In `parse_ready_beads_json()`, after parsing the JSON, identify beads where `issue_type == "epic"` AND at least one `dependency` with `type == "parent-child"` exists where the child is still open. Filter these from the ready pool. Epics with NO open children should remain schedulable (they may need auto-closing or are monolithic work items).

**Affected files**: `src/coordinator.rs` (the `parse_ready_beads_json` and `parse_and_filter_beads` functions), `src/scheduler.rs` (the `ReadyBead` struct may need an `issue_type` field)

**Verification**:
- Run: `cargo test`
- Expect: All existing tests pass plus new tests covering epic filtering.
- Add unit test: given a JSON list with an epic that has open children and a leaf task, only the leaf task appears in the ready pool.
- Add unit test: an epic with zero open children (all closed) remains in the ready pool.
- Manual: run blacksmith against cantrip — epics should not appear in worker assignment logs; only leaf tasks should be assigned.

## Fix 2: Run `bd sync --import-only` in worktree before spawning agent

**Problem**: Every agent's first turn fails with "Database out of sync with JSONL" because the worktree is checked out from main (which has the JSONL) but the bd SQLite database in the worktree is stale. Agents recover, but it wastes 1-2 turns per session.

**Solution**: After creating/resetting the worktree and before spawning the agent process, run `bd sync --import-only` in the worktree directory. If this fails, log a warning but proceed (the agent can still recover).

**Affected files**: `src/pool.rs` (the `spawn_worker` method or worktree setup logic)

**Verification**:
- Run: `cargo test`
- Expect: All tests pass.
- Manual: run blacksmith against cantrip — agent sessions should no longer show "Database out of sync with JSONL" errors in their first turn. Check session JSONL files for absence of `bd sync --import-only` commands from the agent.

## Fix 3: Debounce "filtered out N beads with unresolved dependencies" log

**Problem**: When idle workers exist but no work is assignable (all ready beads conflict with in-progress work), the coordinator logs `INFO filtered out N beads with unresolved dependencies` every 2 seconds. In the test run this produced 60+ identical lines in ~2 minutes, making it hard to spot real events.

**Solution**: Track the previous `(blocked_count, ready_count)` tuple. Only log at INFO level when the values change. On subsequent identical polls, either skip the log entirely or log at DEBUG level. Reset the suppression when values change.

**Affected files**: `src/coordinator.rs` (the `parse_and_filter_beads` function and/or the main coordinator loop where `query_ready_beads` is called)

**Verification**:
- Run: `cargo test`
- Expect: All tests pass.
- Manual: run blacksmith against a project with blocked beads — the "filtered out" message should appear once at INFO, then not repeat until the counts change.

## Fix 4: Auto-close epics when all children are closed

**Problem**: When a child task is closed (e.g., cantrip-z95.3.1), the parent epic (cantrip-z95.3) remains open. This means the epic stays in the scheduling pool and can be re-assigned to workers. It also means the `completed_beads` progress counter doesn't reflect epic completions.

**Solution**: After a successful integration that closes a bead, check if the closed bead's parent epic has all children now closed. If so, auto-close the parent via `bd close <parent-id> --reason="all children completed"`. This can chain upward (closing a child might close a parent epic, which might close a grandparent).

Implementation: shell out to `bd show <bead-id> --json` to get the parent and sibling info, or query `bd list --status=open --json` (already called each scheduling pass) to check if any open beads have a parent-child dependency on the just-closed bead's parent. If no open children remain for that parent, close it.

**Affected files**: `src/coordinator.rs` (after successful integration, add auto-close check), possibly a new helper function

**Verification**:
- Run: `cargo test`
- Expect: All tests pass.
- Add integration test: mock a scenario where closing the last child of an epic triggers auto-close of the parent.
- Manual: run blacksmith against cantrip — when all children of an epic are completed, the epic should auto-close without being assigned to a worker.

## Fix 5: Fix progress counter to reflect actual bead closures

**Problem**: The progress display shows `Progress: 0/25 beads` even after 2 successful integrations. The `completed_beads` in-memory counter increments correctly, but `print_coordinator_integration_progress` reads from `bead_metrics` in the DB which counts closed beads. Since integration merges code but doesn't call `bd close`, the DB count stays at 0.

**Solution**: After a successful integration (fast-forward to main), call `bd close <bead-id>` with the commit message as the reason. This ensures the bead is marked closed in the bd system, the progress counter in the DB reflects reality, and downstream beads that depend on this one become unblocked.

**Affected files**: `src/integrator.rs` (after successful fast-forward, call bd close), `src/coordinator.rs` (the `close_bead_direct` function may be reusable)

**Verification**:
- Run: `cargo test`
- Expect: All tests pass.
- Manual: run blacksmith against cantrip — after each successful integration, the progress line should show incrementing completed count (e.g., `Progress: 1/25 beads`, then `2/25 beads`). Verify with `bd list --status=closed` that beads are actually closed.
