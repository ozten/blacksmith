# Operations Guide

Day-to-day procedures for stopping, inspecting, and resuming blacksmith.

## Graceful Shutdown

### During Active Sessions

Create a STOP file in the project root:

```bash
touch STOP
```

- The coordinator checks for the STOP file at the start of each scheduling loop
- Current session(s) complete normally — workers finish their active bead
- The STOP file is automatically deleted once detected
- Worktrees are preserved for manual inspection
- Clean exit with summary

### Signal-Based Shutdown

```bash
# Finish current session then exit (same as STOP file)
kill -TERM <pid>
# Or press Ctrl+C once

# Force-kill the agent immediately (second Ctrl+C within 3s)
# Sends SIGKILL to the agent process group
```

| Signal | Behavior |
|---|---|
| **SIGINT** (first) | Finishes current session, sends SIGTERM to child process group, then exits |
| **SIGINT** (second, within 3s) | Sends SIGKILL to agent process group — immediate termination |
| **SIGTERM** | Same as first SIGINT |

### Emergency Force Kill

```bash
pkill -SIGKILL blacksmith
```

Use only when the process is hung. May leave:
- Orphaned agent processes
- Uncommitted work in worktrees
- Beads stuck in `in_progress` state

## After Shutdown

### 1. Check for Orphaned Processes

```bash
ps aux | grep -E 'claude|blacksmith' | grep -v grep
```

Kill any orphans manually if found.

### 2. Inspect Worktrees

```bash
ls -la .blacksmith/worktrees/
```

After graceful shutdown, worktrees are preserved for review. This is normal — inspect them, then clean up:

```bash
git worktree remove .blacksmith/worktrees/worker-N-beads-xxx
```

### 3. Check Bead States

```bash
bd list --status=in_progress
```

After a clean shutdown, this should be empty. If beads are stuck, reset them:

```bash
bd update <id> --status=open
```

### 4. Review Recent Sessions

```bash
blacksmith --status
blacksmith metrics events --session N
```

## Resuming After Shutdown

### After Code Changes

```bash
cargo build --release
./target/release/blacksmith --version   # Verify binary
```

### Resume

```bash
./target/release/blacksmith
```

The coordinator:
- Resumes from the last counter value
- Re-queries beads for ready work
- Spawns fresh workers and worktrees
- Recovers beads orphaned by a previous crash (resets `in_progress` → `open`)

### Configuration Check

Before resuming, verify your config is valid:

```bash
blacksmith --dry-run
cat blacksmith.toml
```

## Common Issues

### STOP File Ignored

The coordinator only checks the STOP file at the top of its scheduling loop — not mid-session. Wait for the current session to complete. Check state with:

```bash
blacksmith --status
```

### Worktrees Not Cleaned Up

Normal after graceful shutdown (preserved for review). Clean manually:

```bash
git worktree remove .blacksmith/worktrees/worker-*
```

The next run will also clean orphaned worktrees automatically.

### Beads Stuck in `in_progress`

The coordinator didn't finish cleanup (crash or force-kill). Reset manually:

```bash
bd update <id> --status=open
```

On next startup, the coordinator also runs orphan recovery automatically.

### Singleton Lock Prevents Startup

A previous process crashed without releasing `.blacksmith/lock`:

```bash
# Verify no process is actually running
blacksmith --status
ps aux | grep blacksmith

# If no process found, delete the stale lock
rm .blacksmith/lock
```

## Related Documentation

- [Core Loop — Graceful Shutdown](core-loop.md#graceful-shutdown) — shutdown signal reference table
- [Configuration — `[shutdown]`](configuration.md#shutdown) — STOP file path config
- [Multi-Agent Coordination](multi-agent.md) — coordinator, worktrees, integration queue
- [Troubleshooting](troubleshooting.md) — error codes, diagnostics, recovery procedures
