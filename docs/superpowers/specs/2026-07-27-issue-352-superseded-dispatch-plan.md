# Issue #352: Superseded lineage dispatch implementation plan

## Base and ordering

- Working base: `main` at `fde2d7949364524aaa156055cc28f866a634bac0`.
- Branch: `fix/reject-superseded-dispatch`.
- Campaign predecessors #346, #358, and #353 are not merged at plan approval
  time. Rebase after each is merged, then rerun focused tests and `just ci`.

## Review charter

Implement ADR 0045 and the superseded-dispatch design without migrations,
automatic replanning, use leases, DSL/compiled-wire changes, worker eligibility
changes (#343), verification execution (#334), or later campaign work
(#338/#339). Exact job-output correlation after dispatch begins remains tracked
by #378; this change must keep pre-dispatch stale failures out of that existing
partial-finalization inference.

Success means fresh and resumed phase invocations atomically validate every
planned active tip while creating all root tickets; stale plans fail with no
ticket, lease, client call, or phase effects, and tests inspect durable state
and events.

## Step 1 — Store-owned active-version predicate

Files:

- `crates/voom-store/src/repo/media/identity.rs`
- `crates/voom-store/src/repo/media/identity_test.rs`

Behavior:

- Add an `IdentityRepo` in-transaction predicate accepting expected
  `(FileAssetId, FileVersionId)` pairs.
- Query the greatest live version ID for each asset without joining snapshots.
- Return `STALE_IDENTITY_EVIDENCE` with expected/current/repair context on the
  first mismatch or absent live version.

Red tests:

- newer snapshotted version rejects;
- newer unprobed version rejects;
- no live version rejects;
- exact active version succeeds.

Expected pre-implementation failure:

- the trait method does not exist and the focused test does not compile.

Verification:

- `cargo test -p voom-store active_file_version`
- `just fmt-check`
- `just lint`

Commit:

- `feat(store): add active lineage dispatch predicate`

## Step 2 — Composable ticket/event transaction helpers

Files:

- `crates/voom-control-plane/src/cases/execution/tickets.rs`
- `crates/voom-control-plane/src/cases/execution/tickets_test.rs`

Behavior:

- Extract crate-private `create_ticket_in_tx` and
  `mark_ready_if_unblocked_in_tx` methods.
- Keep public wrappers and exact event payloads/order unchanged.
- Prove a caller can create and ready multiple tickets in one transaction and
  that rollback leaves neither rows nor events.

Red tests:

- an explicit caller-owned transaction cannot currently compose both use cases.

Verification:

- `cargo test -p voom-control-plane cases::execution::tickets`
- `just fmt-check`
- `just lint`

Commit:

- `refactor(control-plane): compose ticket lifecycle in transactions`

## Step 3 — Atomic guarded root-ticket creation

Files:

- `crates/voom-control-plane/src/workflow/execution/executor/mod.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/errors.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/tickets.rs`
- `crates/voom-control-plane/src/workflow/execution/executor/mod_test.rs`

Behavior:

- Render root `NewTicket` specifications before any durable write.
- Add a `PlannedLineageGuard` whose constructor accepts the
  planned-disposition count and rejects zero, count mismatch, and
  duplicate-asset expectations.
- Add a guarded in-job invocation requiring that guard type alongside the
  existing caller-facing method for this intermediate commit. Step 4 removes
  the production bypass atomically with coordinator migration.
- Open `BEGIN IMMEDIATE`, invoke the store
  predicate, create all roots and ticket events, mark all roots ready and emit
  ready events, then commit once.
- Add `WorkflowRunError.dispatch_started`: false for every error before the
  guarded commit and true only after it.
- Audit and update every `WorkflowRunError` constructor in `executor/mod.rs`
  and `executor/errors.rs`; tests cover validation, root-batch, loop, isolated,
  and fatal failures.
- Preserve existing test-only unguarded executor behavior.
- Ensure worker selection, lease acquisition, task spawn, and client dispatch
  remain strictly after the guarded commit.

Red tests:

- stale expectation currently creates at least one ticket;
- a stale multi-root expectation currently cannot roll the root batch back;
- the guarded transaction's writer-order test currently has no production
  boundary to exercise.

Atomicity test:

- hold an uncommitted newer file version in a separate `BEGIN IMMEDIATE`
  promoter transaction;
- start the production guarded in-job invocation expecting the old version;
- prove the executor remains pending while the promoter owns the writer lock;
- commit the promoter and require the executor to return
  `STALE_IDENTITY_EVIDENCE` with no root/event rows;
- mutate the production opener to deferred `BEGIN` and require this test to
  fail, then restore it.
- assert root/predicate/commit failures carry `dispatch_started = false` and an
  induced post-root failure carries `true`.

Verification:

- `cargo test -p voom-control-plane workflow::execution::executor`
- `just fmt-check`
- `just lint`

Commit:

- `feat(workflow): add atomic guarded root dispatch`

## Step 4 — Wire fresh and resumed phase plans through the guard

Files:

- `crates/voom-control-plane/src/workflow/coordinator/mod.rs`
- `crates/voom-control-plane/src/workflow/coordinator/mod_test.rs`

Behavior:

- Derive expectations only from `Disposition::Planned` files.
- Pass them through `dispatch_phase` to the guarded executor invocation.
- Derive expectations and the `WorkflowExecutionShape` count from one zipped
  entering-file/disposition helper so a partial guard fails construction.
- In the same commit, gate the replaced unguarded in-job invocation under
  `cfg(test)`. The final production API has no bypass and the branch compiles
  before and after the migration.
- Carry an executor run summary into partial-phase finalization only when
  `dispatch_started` is true; a stale root-batch rejection must use `None`.
- Extract a crate-private prepared-resume helper parallel to the existing fresh
  helper; the public resume wrapper still validates prior-job existence and
  prepares inputs before delegating.
- Keep prior-job tickets historical and every replacement/later phase on a new
  guarded root batch.

Red tests:

- fresh prepared V1 run after V2 promotion returns a worker failure and has
  durable dispatch state instead of stale rejection;
- a two-file phase with only the second branch stale dispatches today and
  catches a first-only/truncated guard;
- prepared resume after V2 promotion does the same.

Green assertions for both:

- exact stale code and contextual message;
- failed new job with V1 run start;
- job opened/failed events and failed reason;
- no new ticket, lease, ticket/lease event, workflow summary, phase summary,
  file-phase row, or artifact-produced version;
- no partial coordinator outcome and no produced-version field referring to
  the externally promoted V2;
- zero calls to an eligible in-process dispatch client;
- V2 remains active.

Resume-only assertions:

- prior failed job and historical ticket/events are unchanged;
- replacement history/seeds do not claim V2 completed.

Sensitivity:

- remove/invert the store predicate comparison;
- run both focused stale tests and capture their intended failure;
- restore and rerun green.

Verification:

- `cargo test -p voom-control-plane superseded`
- `cargo test -p voom-control-plane workflow::coordinator`
- `just fmt-check`
- `just lint`

Commit:

- `fix(workflow): reject superseded phase dispatch`

## Step 5 — Review, full verification, and campaign handoff

Files:

- complete diff against `fde2d79`
- design and ADR documentation

Actions:

- Re-read exports, immediate callers, transaction/event helpers, resume
  reconciliation, and all changed tests.
- Run the bounded adversarial code review and fix every defensible finding.
- Run security review because worker dispatch and durable authorization are a
  trust boundary.
- Run simplification review and apply only behavior-preserving reductions.
- Rebase after merged #346, #358, and #353 in campaign order.
- Rerun focused tests and the complete guardrail after the final rebase.

Verification:

- `cargo test -p voom-store active_file_version`
- `cargo test -p voom-control-plane superseded`
- `cargo test -p voom-control-plane workflow::execution::executor`
- `cargo test -p voom-control-plane workflow::coordinator`
- `just ci`

Commit:

- documentation belongs with the behavior it specifies; any review fix is a
  separate logical conventional commit.

Shipping:

- push without force;
- open a factual PR with `Closes #352`;
- post `WORK:REVIEW`;
- wait for all CI checks and clean mergeability;
- transition #352 to `status:awaiting-merge`;
- do not merge; the campaign orchestrator owns serial merge authority.
