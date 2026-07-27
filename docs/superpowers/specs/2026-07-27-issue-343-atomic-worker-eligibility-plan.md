# Issue #343 — Atomic worker eligibility implementation plan

## Base and guardrails

- Base: `main` at `9712762461a2d864cac7dccd984a81bb1733e19b`
- Branch: `fix/atomic-worker-eligibility`
- Focused checks:
  - `cargo test -p voom-store operation_eligibility`
  - `cargo test -p voom-store acquire_rejects`
  - `cargo test -p voom-control-plane eligibility`
  - `cargo test -p voom-control-plane no_eligible_worker`
- Full check: `just ci`
- Merge gate: #352 must merge first, followed by rebase and full reverification.
- Tracked exclusion: #379 owns atomic local `max_parallel` enforcement; this
  plan must preserve candidate-time behavior and remote acquisition rechecks.

## Step 1 — Encode the effective store predicate

Files:

- `crates/voom-store/src/repo/execution/workers.rs`
- `crates/voom-store/src/repo/execution/workers_test.rs`
- `crates/voom-store/src/repo/mod.rs`

Add worker lifecycle to `WorkerOperationEligibility`, its fail-closed
`is_eligible()` decision, and a store-owned operation-candidate read. Add
behavior tests for all lifecycle and grant combinations, split allow/deny rows,
candidate deduplication, and effective limit aggregation.

Expected red: split allow/deny remains selectable and lifecycle is not part of
the existing eligibility result.

Verification:

- `cargo test -p voom-store operation_eligibility`
- `cargo test -p voom-store operation_candidates`

Commit boundary: store-owned effective worker eligibility.

## Step 2 — Make lease acquisition authoritative and atomic

Files:

- `crates/voom-store/src/repo/execution/leases.rs`
- `crates/voom-store/src/repo/execution/leases_test.rs`
- affected `voom-store` integration fixtures

Move the ticket mutation into a savepoint, acquire write ownership before the
eligibility recheck, roll back the savepoint on every error, and insert the
lease only for an effective worker. Update truthful happy-path fixtures with
matching capabilities and grants.

Expected red: stale workers, missing capability/grant, and a later deny still
acquire leases; a failed post-mutation check would leave partial ticket state.

Verification:

- `cargo test -p voom-store acquire_rejects`
- `cargo test -p voom-store --test ticket_lease_lifecycle`
- `cargo test -p voom-store`

Commit boundary: atomic lease eligibility enforcement.

## Step 3 — Consume store candidates and verify durable events

Files:

- `crates/voom-control-plane/src/workflow/execution/executor/spawn.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`
- `crates/voom-control-plane/src/cases/execution/leases_test.rs`
- affected control-plane test fixtures

Replace local grant-row authorization with the store candidate read. Preserve
the executor's local reservation overlay. Add split allow/deny and
selection-then-deny regressions that inspect ticket, lease, and event state.
Update unrelated lease happy paths to seed real eligibility.

Expected red: an allow row can still dispatch despite another deny, and direct
control-plane rejection lacks coverage for durable event absence.

Verification:

- `cargo test -p voom-control-plane no_eligible_worker`
- `cargo test -p voom-control-plane eligibility`
- `cargo test -p voom-control-plane`

Commit boundary: shared candidate selection and durable regressions.

## Step 4 — Review, simplify, and ship

Review the complete branch against `main` for authorization bypasses,
transaction races, partial writes, malformed durable JSON, event mismatches,
and unrelated scope. Apply only defensible findings. Run simplification review,
focused tests, and `just ci`; commit any review fixes separately.

Push the branch, open a PR with `Closes #343`, record `WORK:REVIEW` and
`WORK:TRAJECTORY`, and drive remote checks to green. Do not merge. Keep the
issue behind #352 until the orchestrator requests the required rebase.
