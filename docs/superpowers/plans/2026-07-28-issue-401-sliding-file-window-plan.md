# Issue #401 Sliding File Window Implementation Plan

## Durable execution facts

- Branch: `feat/sliding-file-window-401`
- Base: `main`
- Worktree: `/home/dave/src/voom-v2-worktrees/issue-401`
- Assigned ADR: `0048`
- Assigned migration: `0028`
- Full guardrail: `just ci`
- Hosted gates: Ubuntu `just ci`, macOS `just ci`, cargo audit, coverage

## Outcome

Replace the whole-input phase loop with a durable `max_in_flight_files` window.
Files advance independently but sequentially through their own policy phases.
A slot is refilled only after terminal promotion and source-safe intermediate
cleanup. Preserve the single job, per-file gates/error handling, resume,
cancellation, summaries, and add-only output contract.

The tasks are tightly coupled through the progress-row transition contract and
will be implemented directly in order. Every task follows red → green →
refactor and stages only its explicit files.

## Task 1: Persist window capacity and file progress

### Fit

Provide the durable admission/cursor primitive required before changing
coordinator control flow.

### Files

- `migrations/0028_sliding_file_window.sql`
- `crates/voom-store/src/repo/execution/workflow_summaries.rs`
- `crates/voom-store/src/repo/execution/workflow_summaries_test.rs`
- `crates/voom-store/src/schema_test.rs`
- `crates/voom-store/src/migrator.rs` if embedded migration assertions require it

### TDD

1. Add failing schema/repository tests for:
   - positive job-level capacity;
   - atomic window/run-start/progress insertion;
   - stable input ordinal listing;
   - conditional pending-to-active admission that never exceeds capacity;
   - duplicate admission returning no transition;
   - file-phase insertion plus expected-cursor advancement in one transaction;
   - terminal transition timestamps;
   - rejection of missing window, cursor mismatch, invalid timestamps, and
     malformed states.
2. Add migration 0028 and typed repository models/methods.
3. Keep existing file-phase first-write-wins behavior. A replay may return its
   existing row only when the progress cursor proves that exact phase already
   completed; it must never advance twice.

### Acceptance

Repository tests prove the durable limit, cursor, and state machine. Foreign
keys cascade with the owning job and reference run starts. No JSON durable
payload is introduced.

### Verify

`cargo test -p voom-store workflow_summaries`

## Task 2: Add the configured file-window option

### Fit

Expose the positive capacity the persistence layer records. This is the
campaign-approved narrow scope expansion outside the coordinator/store files.

### Files

- `crates/voom-control-plane/src/cases/policy/compliance.rs`
- `crates/voom-control-plane/src/cases/policy/compliance_test.rs`
- `crates/voom-cli/src/cli.rs`
- `crates/voom-cli/src/commands/policy/compliance.rs`
- relevant CLI parser/command tests and snapshots only if changed

### TDD

1. Add failing tests for the default of four, explicit positive CLI plumbing,
   and zero rejection before a job opens.
2. Add `max_in_flight_files: usize` to `ComplianceExecutionOptions`.
3. Add `--max-in-flight-files <N>` to `compliance execute`, preserving the
   existing command envelope and all unrelated defaults.
4. Destructure the new option exhaustively when converting executor options;
   it belongs to the coordinator and is not forwarded as operation capacity.

### Acceptance

Default and explicit limits reach the coordinator. Zero returns the existing
structured configuration error path. No output field changes.

### Verify

`cargo test -p voom-control-plane compliance`
`cargo test -p voom-cli compliance`

## Task 3: Scope executor invocations per file and make cursor completion atomic

### Fit

Concurrent per-file pipelines must never claim each other's tickets, and a
completed operation must not leave a cursor behind its durable file-phase row.

### Files

- `crates/voom-control-plane/src/workflow/coordinator/finalize.rs`
- `crates/voom-control-plane/src/workflow/coordinator/finalize_test.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`
- directly related workflow-summary repository methods/tests from Task 1

### TDD

1. Add failing tests where two files execute the same phase concurrently and
   each ticket lookup sees only its deterministic job/file/phase invocation.
2. Thread a stable invocation id based on job, input ordinal, and phase ordinal
   through dispatch, ticket-state lookup, failure isolation, finalization, and
   unfinalized verification recovery.
3. Finalize one file-phase at a time. Insert/return its row and advance
   `workflow_file_progress.next_phase_ordinal` atomically.
4. Preserve gate history and refresh the file's version/snapshot only after the
   commit evidence and cursor transition succeed.

### Acceptance

Cross-file ticket isolation and replay idempotency tests pass. A cursor cannot
advance without its matching row or lag behind an acknowledged row.

### Verify

`cargo test -p voom-control-plane workflow::coordinator`

## Task 4: Run the bounded sliding coordinator and terminalize before refill

### Fit

Replace the barrier loop with the behavior visible to issue #401.

### Files

- `crates/voom-control-plane/src/workflow/coordinator/mod.rs`
- a focused new coordinator child module and sibling test file if needed to
  keep functions within repository limits
- `crates/voom-control-plane/src/workflow/coordinator/planning.rs`
- `crates/voom-control-plane/src/workflow/coordinator/promotion.rs`
- `crates/voom-control-plane/src/workflow/coordinator/promotion_test.rs`
- `crates/voom-control-plane/src/workflow/coordinator/resume.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`

### TDD

1. Add controlled-runtime tests that prove:
   - file A starts phase 2 while slow file B remains in phase 1;
   - active progress never exceeds the durable limit;
   - a pending file is admitted only after its predecessor reaches terminal;
   - planning block releases one slot;
   - continue-on-error blocks only its file and refills;
   - abort failure and cancellation stop refill and drain admitted work.
2. Implement a bounded `JoinSet` of per-file pipelines. Each pipeline plans and
   dispatches phases sequentially from refreshed facts. The parent admits in
   durable order, stops admission on fatal/cancelled job state, drains, and
   preserves every completed result.
3. Add terminalization:
   - promote the branch's terminal main and sidecar locations add-only;
   - find earlier same-lineage job-produced locations under canonical
     committed working directories;
   - exclude the active chain tip and all source/unscoped locations;
   - remove the intermediate, treating not-found as replay success;
   - retire only that produced location while retaining all evidence;
   - mark progress terminal, then admit a replacement.
4. Blocked files terminalize without promotion. Promotion/cleanup errors leave
   the slot active and fail the run.

### Acceptance

Cross-file overlap, strict window/refill, immediate promotion, immediate safe
cleanup, success/block/continue/failure/cancellation all have behavior tests.
Functions remain at most 100 lines and cyclomatic complexity at most eight.

### Verify

`cargo test -p voom-control-plane workflow::coordinator`
`cargo test -p voom-control-plane --test phase_barrier_flow`

## Task 5: Reconstruct summaries and resume without repeats

### Fit

Keep the existing durable reporting and restart contract after removing phase
barriers.

### Files

- `crates/voom-control-plane/src/workflow/coordinator/resume.rs`
- `crates/voom-control-plane/src/workflow/coordinator/planning.rs`
- `crates/voom-control-plane/src/workflow/coordinator/finalize.rs`
- their sibling test files
- `crates/voom-control-plane/tests/phase_barrier_flow.rs`

### TDD

1. Add resume tests for:
   - completed phases create no new tickets;
   - each branch is admitted at most once;
   - prior active branches precede prior pending branches;
   - a completed-but-unpromoted file terminalizes without re-execution;
   - progress/phase-row disagreement fails before opening a new job.
2. Reconstruct each phase's refreshed planning input and gate decisions from
   durable run starts/history/file-phase rows/snapshots/tickets. Fold one
   phase/report row per ordinal at drain, including partial failure and
   cancellation.
3. Merge job telemetry without double-counting concurrent invocation snapshots.
4. Keep cancelled jobs cancelled; do not rewrite them failed.

### Acceptance

Resume cannot repeat a completed operation or duplicate admission. Partial and
successful job/phase/file-phase reports are coherent and ordered.

### Verify

`cargo test -p voom-control-plane workflow::coordinator`
`cargo test -p voom-control-plane --test phase_barrier_flow`

## Task 6: Operator documentation and complete verification

### Files

- `docs/runbooks/operator-real-media-execution.md`
- issue-specific design/ADR/plan corrections required by implementation

Do not edit `docs/adr/README.md`; the campaign orchestrator owns the ADR 0048
index row.

### Work

1. Document the default/override, window-vs-worker-capacity distinction,
   immediate terminalization, staging bound, cancellation, and resume checks.
2. Re-read the diff for unnecessary abstraction and stale phase-barrier claims.
3. Run focused format/lint/tests while iterating, then `just ci`.
4. Run the branch adversarial review loop and resolve every defensible finding.
5. Run the simplification review. Any behavioral cleanup triggers another
   adversarial pass.

### Acceptance

The full local guardrail is green with zero warnings. Hosted Ubuntu/macOS
aggregate checks, audit, and coverage are green, and the PR is mergeable.
