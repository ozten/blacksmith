# Prompt: Derive Verification Artifacts from a PRD (Invariants, Contracts, Oracles, and Backpressure Gates)

You are a senior software verification engineer embedded on an agentic-systems team.  
You have just read a Product Requirements Document (PRD) for a system we are building. The PRD includes some verification details, but they are incomplete.

Your task is to **extract and strengthen the PRD into a concrete verification plan** that an agentic harness can enforce with backpressure.

## Output requirements
- Write your output in **structured Markdown** with the exact headings listed below.
- Prefer **crisp, testable statements**.
- When something is ambiguous or underspecified, **do not guess silently**—write it as an **assumption** or a **question**.
- Use **RFC 2119 keywords** (MUST / SHOULD / MAY) where appropriate.
- Include both **human-meaningful** descriptions and **machine-checkable** formulations.
- For every critical requirement, propose at least one **oracle** (how we know it’s correct) and at least one **automated check** (how to enforce it).
- Keep everything **implementation-agnostic** (do not assume our codebase).

---

## 1) System summary (for verification context)
1. In 5–10 bullets, summarize what the system does, its boundaries, and its main flows.
2. List the major components and external dependencies (APIs, databases, runtimes, tools).
3. Identify which parts are **stateful** and which are **stateless**.

---

## 2) Glossary and state model
1. Define key entities and their identifiers (IDs, keys, names).
2. Provide a state machine for each major entity/workflow:
   - States
   - Allowed transitions
   - Terminal states
   - Forbidden transitions
3. Call out concurrency and timing aspects (parallelism, retries, idempotency).

---

## 3) Assumptions and open questions
- List all assumptions you had to make due to missing PRD details.
- List concrete questions that must be answered to finalize verification.
- Identify which assumptions are **high-risk** and must be confirmed before shipping.

---

## 4) Invariants (global “must always be true” rules)
Derive invariants from the PRD. These must be stable properties of the system.

For each invariant:
- **Invariant statement** (MUST form)
- **Scope** (component / workflow / global)
- **Threat model** (how it could be violated)
- **Oracle(s)** (how we detect correctness)
- **Enforcement** (what automated check prevents/regresses it)
- **Failure handling** (what happens if violated)

Examples of invariants you might include (adapt to PRD):
- Safety invariants (no data loss, no unauthorized action, no double-spend)
- Workflow invariants (can’t “close” unless tests pass; can’t transition without prerequisites)
- Resource invariants (budgets, rate limits, concurrency caps)

---

## 5) Contracts (boundary agreements)
Identify **contracts at each boundary**:
- API requests/responses (schemas, versions, compatibility)
- CLI inputs/outputs (exit codes, stdout/stderr format, machine-readable output modes)
- File formats (JSON, YAML, protobuf, etc.)
- Database schema and migration constraints
- Event/queue message schemas

For each contract:
- **Producer** / **Consumer**
- **Schema** (fields, types, required/optional)
- **Compatibility rules** (forward/backward)
- **Validation** (how to check at runtime/CI)
- **Contract tests** (consumer-driven or provider verification)
- **Versioning strategy** (semantic versioning, schema evolution rules)

---

## 6) Oracles: how we know we’re correct (beyond “more tests”)
For each major area (core logic, scheduling, state machine, external effects):
- Identify what the PRD expects
- Identify the strongest feasible oracle(s):
  - Assertions / expected outputs
  - Invariants
  - Differential testing (reference vs optimized implementation)
  - Snapshot/golden results
  - Replay-based equivalence
  - Metamorphic relationships (see next section)

Explain tradeoffs: false positives/negatives, brittleness, cost.

---

## 7) Metamorphic properties (laws the system must obey)
Where exact expected output is hard to specify, define metamorphic properties.

For each property:
- **Transformation** on input/environment
- **Expected relationship** between outputs
- **Why it matters** (what it catches)
- **Automated test approach**

Examples to consider (adapt to PRD):
- Determinism under same seed + same event log
- Monotonicity (increasing priority shouldn’t make something scheduled later)
- Non-interference (adding unrelated independent work shouldn’t reorder dependent chain)
- Idempotency (re-running same step yields same final state)
- Stability under retries (transient failures do not change correctness)

---

## 8) Backpressure gates (verification-driven control loop)
Design the harness gates that prevent runaway behavior.

1. Define the **verification gradient**:
   - Fast gates (lint, typecheck, unit)
   - Medium gates (integration, contract, replay)
   - Slow gates (E2E, perf, mutation)
   - Formal-ish gates (model checking / proofs) if warranted

2. For each workflow phase (plan → implement → test → close → exit):
   - What must be true to proceed?
   - What evidence must be produced?
   - What failures force **fix-only mode**?

3. Define WIP/budget controls:
   - Max parallelism
   - Retry limits
   - Token/time/cost budgets
   - “Stuck” detection thresholds

---

## 9) Trace + replay requirements (debuggability and determinism)
Specify what must be captured to reproduce failures:
- Event types and minimal fields (IDs, timestamps, state transitions)
- What to capture in LIGHT vs HEAVY modes
- Redaction requirements (secrets, PII)
- Replay mode requirements (ports mocked by trace)

Include at least:
- A minimal event schema outline
- A replay equivalence definition (“what must match?”)

---

## 10) Test plan matrix (what to build)
Provide a table-like structured plan (no need for literal table) listing:
- Unit tests
- Property-based tests
- Integration tests (with fakes)
- Contract tests
- Replay regression tests
- E2E scenarios (curated set)
- Formal-ish checks (if applicable)

For each category:
- Key coverage targets
- Top risk it addresses
- Suggested pass/fail criteria
- What to run on PR vs nightly vs release

---

## 11) “Definition of Done” proof for the harness
Define what it means for the system (and each ticket/work item) to be done.

Must include:
- Evidence required (tests run, contracts verified, trace recorded)
- Completion conditions (no runnable work, no inflight work, no pending retries)
- Safe failure semantics (permanent failures block dependents, etc.)
- Output report format for completion

---

## 12) Prioritized verification backlog (next steps)
Create a prioritized list of verification work:
- P0: must-have for correctness/safety
- P1: should-have for reliability/scale
- P2: nice-to-have for optimization and confidence

Each item should be phrased as an actionable engineering task.

---

### Final instruction
Do not write code. Do not propose specific libraries unless asked.  
Focus on **deriving verification artifacts** that a harness can enforce as backpressure.
