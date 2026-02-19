# Improving Backpressure in Our Agentic Harness
*A plan for building an agentic harness that reliably builds agentic harnesses*

## Why this exists
Agents can produce far more code, diffs, and “progress” than humans traditionally can. That flips the bottleneck:

- **Code generation is cheap**
- **Establishing truth (correctness, completion, safety) is expensive**

So our harness must function less like a “runner” and more like a **control system**: it shapes agent behavior, constrains failure modes, budgets expensive verification, and prevents runaway scope.

The central mechanism we’ll improve is **backpressure**.

---

## Philosophy: harness as control system, not a chat loop

### 1) Agents are workers; the harness is the judge
Agents propose changes and evidence. The harness decides:
- what work may proceed
- what verification must happen next
- whether completion is proven
- when to stop

> **“Done” is evidence-based, not vibe-based.**

### 2) Verification is not a single suite, it’s a gradient
Instead of a static test pyramid, we run a **verification gradient**:
- **fast / frequent** checks gate most behavior
- **expensive** checks are budgeted and triggered by risk
- **formal-ish** checks are surgical for concurrency and termination

### 3) Determinism is a product feature
Non-determinism is the root cause of irreproducible failures in agentic systems.
We design for:
- deterministic scheduling (given the same inputs, seed, and events)
- trace recording and replay
- replayable regression tests derived from real failures

---

## What “backpressure” means for our harness
In systems, **backpressure** is how consumers force producers to slow down when validation can’t keep up.

In our harness:
- **producer** = “builder” behavior (implementing features, writing code, adding more tasks)
- **consumer** = “checker” behavior (tests, contracts, invariants, proofs, review gates)
- **backpressure** = harness rules that prevent the agent from producing new work until the current work is verified (or explicitly accepted).

### Why this matters for agentic build systems
Without strong backpressure, agents:
- pile changes on top of failing foundations
- expand scope to avoid fixing errors
- drift from the spec
- create flaky, non-reproducible pipelines
- never “know they’re done” because nothing enforces a stopping proof

Backpressure is how we *shape spiky intelligence into dependable production behavior*.

---

## Core shift: from “write more tests” to “build stronger oracles”
A test is only as good as its **oracle**.

### Oracles: what they are
An **oracle** is the mechanism that decides whether an output is correct.

Common oracle types we will use:
- **Assertion oracle**: `assert_eq!(actual, expected)`
- **Invariant oracle**: properties that must always hold (safety rules)
- **Differential oracle**: compare two implementations or old vs new behavior
- **Reference oracle**: compare against a simpler spec implementation
- **Snapshot oracle**: compare against a golden output

Agents can generate thousands of tests, but if the oracle is weak, we get false confidence at scale.

---

## Metamorphic testing: correctness when exact expected output is unknown
### What it is
A **metamorphic test** checks **relationships** between outputs under transformations of inputs, rather than checking an exact answer.

Useful when:
- “correct schedule” isn’t unique
- exact output is costly to compute
- the best we can enforce is “must obey these laws”

### Examples for orchestration/scheduling
We encode laws like:
- **Determinism:** same seed + same event stream ⇒ same decisions
- **Monotonic priority:** increasing priority should not make a ticket scheduled later (within policy constraints)
- **Non-interference:** adding an unrelated independent ticket should not reorder an existing dependency chain (unless policy allows)
- **Idempotency:** replaying a completed step shouldn’t corrupt state
- **Budget invariance:** retries and backoffs never violate budget constraints

Metamorphic tests become some of our highest-leverage harness guardrails.

---

## Trace-based replay: turning nondeterminism into regression tests

### What “trace-based replay” means in practice
We record an append-only **event log** (trace) of:
- external interactions (ticket API, git, shell, agent runtime)
- scheduler decisions and state transitions
- command outputs + errors
- timing/backoff decisions
- seeds and configuration snapshots

Then we can:
1. reproduce failures deterministically
2. convert real-world failures into regression tests
3. debug scheduling, backpressure, and concurrency without “live” dependencies

### What we record (minimum viable trace)
**Inputs**
- ticket payloads, dependency graph snapshot
- config, concurrency limits, budgets
- RNG seed(s)

**Agent calls**
- prompt/tooling IDs (hashes), parameters
- agent outputs (or output blob refs)

**Commands**
- command line, cwd, env hash
- exit code, stdout/stderr (or hashes + blobs)

**Decisions**
- “picked ticket X”
- “spawned agent Y”
- “queued retry for job Z”
- state machine transitions

**Time**
- logical timestamps
- timeouts/deadlines
- sleep/backoff intervals

### Replay mode
Replay mode replaces real world ports with a deterministic driver that replays recorded outputs in order:
- ticket responses are served from trace
- commands “return” recorded stdout/stderr/exit codes
- agent outputs are served from trace
- scheduler sees the same event stream, reproducing the bug reliably

---

## Architecture: separate “pure decisions” from “impure execution”
To make all of the above testable, we structure the orchestrator into two layers:

### Layer A — Pure (Decision / Control)
- dependency resolution
- ticket readiness computation
- scheduling decisions (priority, fairness, limits)
- state machine transitions
- termination detection (“done proof”)
- budgeting (tokens/time/cost)

This layer is deterministic and heavily unit/property tested.

### Layer B — Impure (Execution / IO)
- ticket API calls
- git operations
- running shell commands/tests
- calling agent runtimes
- filesystem workspace management
- wall-clock time

This layer is behind “ports” (traits/interfaces) so it can be faked and replayed.

---

## Backpressure policies we will implement

### 1) Verification-gated progress
Agents may not continue expanding work until verification has caught up.

Examples:
- After N edits/tool calls ⇒ must run verification gate
- No new tasks spawned while any “required gate” is red
- When tests fail ⇒ next actions must be *fix-only* until green

### 2) Bounded Work-In-Progress (WIP)
Prevent runaway scope and indefinite expansion.
- cap number of in-flight tickets
- cap number of unverified diffs
- cap number of open TODOs or “pending” states
- cap retries per failure class

### 3) Budgeted expensive checks
E2E and heavyweight checks are not default; they are risk-triggered and budgeted.
- small changes: fast checks + properties + contracts
- boundary changes: contract + replay + targeted integration
- high-risk changes: E2E rotation set + possibly formal-ish checks

### 4) Evidence-based completion gates
The harness stops only when completion is proven via evidence, not by agent assertion.

---

## “Done detection” as a proof obligation
**The orchestrator may exit iff all of these are true:**
1. No runnable tickets (nothing eligible based on dependencies/policy)
2. No in-flight jobs (agents/commands running)
3. No pending retries scheduled in the future (or policy says “stop waiting”)
4. No newly-unblocked dependencies (graph stabilized)
5. All tickets are in terminal states:
   - `Done`, `Failed(Permanent)`, or `Canceled` (with policy-defined semantics)
6. A final evidence report exists (what happened and why it’s safe to stop)

This must be testable under concurrency and races.

---

## Verification gradient: our practical “control loop”
We implement a loop that uses backpressure to force verification:

1. **Select work** (ticket readiness + dependency graph)
2. **Plan + implement small chunk**
3. **Run verification gate**
4. If gate fails ⇒ **fix-only** loop (no expansion)
5. If gate passes and acceptance criteria satisfied ⇒ **close + terminate**

### Gate ordering (typical)
**Fast**
- lint/format
- type checking
- unit tests
- invariant checks
- property/metamorphic tests (seeded)

**Medium**
- integration tests with faked ports
- contract tests (API/schema/event compatibility)
- deterministic replay tests (if trace available)

**Slow / budgeted**
- E2E scenario set (rotating, risk-based)
- performance/regression checks
- mutation tests (periodic or targeted)

**Surgical formal-ish**
- concurrency interleavings / termination modeling for the core reducer/scheduler

---

## Testing strategy by category (what we build)

### A) Unit tests (fast, ubiquitous)
We unit-test:
- dependency graph algorithms (toposort, cycles, readiness)
- scheduler policy (fairness, priority, limits)
- state machine transitions (allowed transitions only)
- invariants (safety rules)
- budgeting logic
- termination predicate (“done proof”)

### B) Property-based tests (high leverage)
We generate randomized:
- dependency DAGs (and some cycles)
- ticket priorities and constraints
- job durations and interleavings
- transient failures / timeouts / rate limits
- cancellation and restart events

We assert:
- invariants are never violated
- determinism holds for same seed + same events
- liveness holds under “eventual success” assumptions
- no starvation (if policy demands fairness)

### C) Integration tests (deterministic fakes)
We test the full orchestrator logic against fake ports:
- scripted ticket system responses
- scripted agent outputs
- scripted command outputs (including failures)
- recorded timing/backoff behaviors

Scenarios:
- happy path to close
- fix loop from failing tests to pass
- dependency chain A→B
- parallel tickets respecting resource limits
- retry/backoff on transient API/tool failure
- resume from checkpoint after crash

### D) E2E tests (small, curated, release gate)
We run a small number of real end-to-end flows:
- golden path
- intentional fail-then-fix
- concurrency scenario (two independent tickets)
- dependency scenario (A must complete before B)
- crash/restart resume
- no-op detection (already satisfied)

Goal: validate wiring and boundaries, not scheduling correctness.

### E) Formal-ish checks (targeted, concurrency + termination)
We apply stronger methods to:
- the event loop / reducer
- concurrency behavior (no deadlocks, no lost events)
- termination detection under races

---

## Rust tooling map: what we’ll use
We’ll standardize on widely used Rust ecosystem tools for each need:

### Core testing
- Built-in: `cargo test`, `#[test]`

### Property-based testing
- `proptest` (strategies + shrinking)
- `quickcheck` (QuickCheck-style)

### Snapshot / golden testing
- `insta`

### CLI integration testing
- `assert_cmd` (spawn CLI + assertions)
- (often paired with `predicates` and temp dir helpers)

### Mutation testing
- `cargo-mutants`

### Concurrency interleavings (model checking style)
- `loom` (systematically explores thread interleavings)

### Contract testing
- Pact ecosystem (Rust support) for consumer/provider contracts where relevant

### Formal-ish verification (selective)
- `kani` for bounded verification / checking (best used surgically)
- (complementary to `loom` for concurrency interleavings)

---

## Implementation plan (capability migration, not codebase-specific)

### Phase 1 — Establish the control surface
- Define explicit ticket/job state machine
- Encode invariants and termination predicate
- Introduce ports (traits) for all IO boundaries
- Add structured event logging

**Deliverable:** deterministic “pure” core + traceable “impure” shell.

### Phase 2 — Backpressure gates and fix-only loops
- Implement verification gates
- Enforce fix-only behavior after failures
- Add WIP limits and retry policies
- Add evidence report generation and enforcement

**Deliverable:** harness that can’t “run away” and can prove “done.”

### Phase 3 — Property + metamorphic coverage
- Add DAG generators and failure injectors
- Implement metamorphic laws for scheduler and termination
- Seeded determinism checks and schedule stability

**Deliverable:** broad correctness coverage without brittle examples.

### Phase 4 — Trace-based replay + regression harness
- Implement replay mode using trace logs
- Convert real failures into replayable tests
- Add tooling to snapshot/replay traces in CI

**Deliverable:** nondeterminism becomes a debugging asset.

### Phase 5 — Curated E2E + formal-ish checks
- Build small set of E2E scenarios against sandbox systems
- Apply loom to concurrency hotspots
- Apply formal-ish checks to termination and critical invariants where warranted

**Deliverable:** high confidence without relying on flaky E2E.

---

## Operational guardrails (how we keep it healthy)
- Every bug should become:
  - a deterministic replay trace, or
  - a property/metamorphic regression, or
  - a unit/invariant test
- Flaky tests are treated as correctness failures
- Expensive checks (E2E/mutation/perf) are budgeted and policy-driven
- “Close ticket” requires machine-checkable evidence, not an agent statement

---

## Success metrics
We measure improvement in:
- reduced “agent runaway” (unbounded diffs, scope creep, endless loops)
- lower flake rate via replayable traces
- faster time-to-root-cause for orchestration bugs
- fewer regressions escaping due to stronger oracles
- determinism of scheduling decisions under replay
- reliable termination (“done proof” holds under concurrency and retries)

---

## Summary: what “improving backpressure” buys us
Improving backpressure means:
- verification keeps pace with generation
- failure causes narrowing and fixing, not expansion
- completion is provable and enforced
- concurrency and nondeterminism become testable
- the harness can safely scale to agent-built complexity

This is how we build an agentic harness that can reliably build agentic harnesses.
