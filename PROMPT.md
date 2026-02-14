# Task Execution Instructions

## CRITICAL: Execution Efficiency Rules (MUST FOLLOW)

These two rules are NON-NEGOTIABLE. Violating them wastes 25-35% of your turn budget.

### Rule A: ALWAYS batch independent tool calls in the SAME turn.
Every time you are about to call a tool, ask: "Is there another independent call I can make at the same time?" If yes, emit BOTH tool calls in the SAME message.

**Mandatory parallel patterns — use these EVERY session:**
- Session start: `bd ready` + `Read PROGRESS.txt` → ONE turn, TWO tool calls
- Reading source + test: `Read class-foo.php` + `Read Test_Foo.php` → ONE turn
- Multiple greps: `Grep("pattern1")` + `Grep("pattern2")` → ONE turn
- Session end: `Bash(composer lint:fix)` + `Bash(composer analyze)` → ONE turn (if they don't depend on each other's output)
- Reading multiple related files: `Read template.php` + `Read meta-box.php` → ONE turn

**A session with ZERO parallel calls is a failure.** Target at least 5 turns with 2+ parallel calls per session.

### Rule B: NEVER emit a text-only turn. Every assistant message MUST include at least one tool call.
WRONG: "Let me check the tests." (turn 1) → `Grep(tests/)` (turn 2)
RIGHT: "Let me check the tests." + `Grep(tests/)` (turn 1 — one message, includes both text AND tool call)

If you want to narrate what you're doing, include the narration AND the tool call in the same message. A text-only turn doubles your turn count for zero benefit.

### Rule C: After closing your bead, EXIT IMMEDIATELY.
Do NOT triage other beads. Do NOT run `bd ready` to find more work. Do NOT explore what to do next.
The sequence after closing is: write PROGRESS.txt → run `bd-finish.sh` → STOP.
Each session handles exactly ONE bead. The loop script handles picking the next one.

---

## Context Loading

The plugin architecture and test setup are documented in MEMORY.md — do NOT re-explore the codebase.
Only read files you are about to modify. Do NOT launch explore subagents (this means NO `Task` tool with `subagent_type: Explore`).

1. Run `bd ready` AND `Read PROGRESS.txt` in the SAME turn (Rule A — two parallel tool calls)

## Task Selection
Pick ONE task from the ready queue. **Always pick the highest-priority (lowest number) ready task.** Only deviate if PROGRESS.txt explains why a specific lower-priority task should go next (e.g., it's a quick follow-up to the last session's work).

**Remember Rule C**: You will work on exactly ONE task this session. After closing it, exit immediately.

### Failed-Attempt Detection
Before claiming a task, run `bd show <id>` and check its notes for `[FAILED-ATTEMPT]` markers.

- **0 prior failures**: Proceed normally.
- **1 prior failure**: Proceed, but read the failure reason carefully. If the reason mentions "too large" or "ran out of turns," consider whether you can realistically finish in 55 turns. If not, skip to the decomposition step below.
- **2+ prior failures**: Do NOT attempt implementation. Instead, decompose the bead into smaller sub-beads:
  1. Analyze the bead description and failure notes to understand why it keeps failing
  2. Break it into 2-5 smaller sub-beads (follow the break-down-issue workflow: create children, wire deps, make original blocked-by children)
  3. Write PROGRESS.txt noting the decomposition, then exit cleanly via `bd-finish.sh`
  4. The next session will pick up the newly-unblocked child beads

If ALL top-priority ready beads have 2+ failures and you've decomposed them, move to the next priority level.

## Execution Protocol
For the selected task (e.g., bd-X):

1. **Claim**: `bd update bd-X --status in_progress`

2. **Understand**: Run `bd show bd-X` for full task description. If the task references a PRD section, read it with an offset (see PRD index in AGENTS.md).

3. **Implement**: Complete the task fully
   - Only read files you need to modify — architecture is in MEMORY.md
   - Follow existing code patterns (see MEMORY.md "Plugin Architecture" and "Testing")
   - New test classes must call `Tickets_Please::get_instance()->register_meta_fields()` in `setUp()`

4. **Verify** (use parallel calls per Rule A):
   ```bash
   # Run full test suite FIRST, then lint+analyze in parallel:
   cd wp/wp-content/plugins/tickets-please && php vendor/bin/phpunit
   # Then in ONE turn with TWO parallel Bash calls:
   ./composer.sh lint:fix
   ./composer.sh analyze
   ```
   Run lint:fix and analyze exactly ONCE each. Do not repeat them.

5. **Finish** — write PROGRESS.txt and call bd-finish.sh, then STOP (Rule C):
   - **Write PROGRESS.txt** (overwrite, not append) with a short handoff note:
     - What you completed this session
     - Current state of the codebase
     - Suggested next tasks for the next session
   - **Run the finish script**:
     ```bash
     ./bd-finish.sh bd-X "<brief description>" file1.php file2.php
     ```
     This handles: staging, committing, bd close, bd sync, auto-committing .beads/, appending to PROGRESS_LOG.txt, and git push — all in one command.
   - If no specific files to stage, omit the file list and it will stage all tracked modified files.
   - **After bd-finish.sh completes, STOP. Do not triage more work. Do not run bd ready. Session is done.**

## Turn Budget (R1)

You have a **hard budget of 80 assistant turns** per session. Track your turn count.

- **Turns 1-55**: Normal implementation. Write code, run targeted tests (`--filter`).
- **Turns 56-65**: **Wrap-up phase.** Stop new feature work. Run the full test suite + `lint:fix` + `analyze`. If passing, commit and close.
- **Turns 66-75**: **Emergency wrap-up.** If tests/lint are failing, make minimal fixes. If you can't fix in 10 turns, revert your changes (`git checkout -- .`), mark the failure (see below), write PROGRESS.txt, and exit cleanly.
- **Turn 76+**: **Hard stop.** Do NOT start any new work. If you haven't committed yet: revert, mark the failure, write PROGRESS.txt, and exit immediately. An uncommitted session is worse than a cleanly abandoned one.

If you realize before turn 40 that the task is too large to complete in the remaining budget, STOP immediately. Mark the failure, and exit. Do not burn 40 more turns on a doomed session.

### Marking a Failed Attempt
When bailing out of a task for any reason, always run:
```bash
bd update <id> --status=open --notes="[FAILED-ATTEMPT] <YYYY-MM-DD> <reason>"
```
Use a specific reason: `too-large`, `tests-failing`, `lint-unfixable`, `missing-dependency`, `context-overflow`, or a brief custom description. This marker is read by future sessions to detect beads that need decomposition (see Task Selection).

## Stop Conditions
- Complete exactly ONE task per iteration, then STOP (Rule C)
- After calling bd-finish.sh, do NOT continue. Do NOT triage. Do NOT run bd ready again.
- If task cannot be completed, mark the failure (see above), write PROGRESS.txt, exit cleanly
- If tests fail, debug and fix within this iteration

## Important
- Do not ask for clarification — make reasonable decisions
- Do NOT launch explore/research subagents (NO `Task` with `subagent_type: Explore`) — the architecture is in MEMORY.md
- Do NOT re-read files you already know from MEMORY.md
- Use `./composer.sh <cmd>` from project root — there is no global `composer` command
- Prefer small, atomic changes over large refactors
- Always run `php vendor/bin/phpunit` before committing
- Always run `./composer.sh lint:fix` then `./composer.sh analyze` before committing — exactly ONCE each
- Always use `./bd-finish.sh` to close out — do NOT manually run git add/commit/push/bd close/bd sync
- **EFFICIENCY**: Re-read Rules A, B, C above. Every text-only turn and every sequential-when-parallel tool call wastes your limited turn budget. Aim for 5+ parallel turns per session and 0 narration-only turns.
