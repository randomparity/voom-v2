# Issue #364 Operator Job Cancellation Implementation Plan

**Goal:** Expose audited job cancellation through the CLI and make a cancelled
job ineligible for new ticket leases.

**Design:** Reuse the existing control-plane transaction and job projection,
distinguish missing from terminal transitions in the store, validate the audit
reason before taking a transaction, and add the parent-job-open predicate to
both ready-ticket selection and the guarded ready-to-leased update. Preserve
ticket rows and already-held leases.

**Success criteria:**

- one standard job envelope reports successful cancellation;
- missing, terminal, blank-reason, and clap failures retain explicit public
  codes and exit behavior;
- job update and cancellation event commit together;
- failed cancellation changes no durable job, ticket, lease, or event state;
- cancelled-job tickets cannot be selected or newly leased;
- jobless and open-job tickets remain schedulable;
- held leases are not preempted; and
- focused tests and `just ci` pass without warnings.

## Task 1: Pin the operator contract with failing tests

**Files:**

- Add: `crates/voom-cli/tests/job_cancel_envelope.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/jobs_test.rs`
- Modify: `crates/voom-store/src/repo/execution/jobs_test.rs`

1. Add a CLI fixture that opens one job and creates a ready child ticket.
2. Cancel the open job through the binary and assert exit 0, one parseable
   envelope, `command: "job"`, the cancelled job projection, and epoch bump.
3. Through a second control-plane/store view, assert the job is cancelled,
   the ready ticket is unchanged, exactly one `job.cancelled` event carries the
   submitted reason, and no lease or lease/ticket event exists.
4. Add CLI cases for a missing ID, every terminal job state, and a whitespace
   reason. Assert the exact error code and durable job, ticket, lease, and event
   state after each failure.
5. Add clap cases for either omitted argument and assert `BAD_ARGS`, exit 1,
   and one envelope.
6. Add control-plane and store tests for reason validation and missing-versus-
   terminal transition classification.
7. Run and record the intended failures:

   ```sh
   cargo test -p voom-cli --test job_cancel_envelope
   cargo test -p voom-control-plane --lib cases::execution::jobs::tests
   cargo test -p voom-store --all-features --lib jobs
   ```

**Checkpoint:** keep red tests uncommitted until the implementation is green.

## Task 2: Implement the cancellation command and exact failures

**Files:**

- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/commands/job.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/jobs.rs`
- Modify: `crates/voom-store/src/repo/execution/jobs.rs`

1. Add `JobCommand::Cancel { job_id, reason }`.
2. Route it to a handler using `JobId`, `cp.clock().now()`, the existing job
   projection, and `emit_voom_error`.
3. Validate `reason` with `require_audit_field` before beginning the
   cancellation transaction.
4. After a zero-row open-state update, reread inside the transaction and return
   `NOT_FOUND` only for an absent job. Return `CONFLICT` for a terminal row.
5. Run the focused tests from Task 1 until green.
6. Temporarily remove reason validation and the missing-row branch. Confirm
   their named tests fail, then restore the implementation.

**Commit boundary:**

```text
feat(cli): add audited job cancellation
```

## Task 3: Pin and implement the scheduling stop

**Files:**

- Modify: `crates/voom-store/src/repo/execution/tickets_test.rs`
- Modify: `crates/voom-store/src/repo/execution/tickets.rs`
- Modify: `crates/voom-store/src/repo/execution/leases.rs`
- Modify: `crates/voom-control-plane/src/cases/execution/leases_test.rs`

1. Add ready-selection fixtures containing an open-job ticket, a
   cancelled-job ticket, and a jobless ticket. Assert only the open and jobless
   tickets are candidates.
2. Add a direct lease-acquisition test for a cancelled job. Assert `CONFLICT`,
   the ticket remains ready with unchanged attempt and epoch, no lease exists,
   and no lease/ticket event is appended.
3. Add a held-lease test: acquire first, cancel second, and assert the lease
   remains held and the ticket remains leased.
4. Add the parent-job-open `EXISTS` condition to ready-ticket selection.
5. Add the same condition to the guarded ready-to-leased update, composing
   with #343's savepoint and worker eligibility check.
6. Run:

   ```sh
   cargo test -p voom-store --all-features --lib ready_for_operations
   cargo test -p voom-control-plane --lib cases::execution::leases::tests
   ```

7. Temporarily remove the candidate condition and prove the selection test
   fails. Restore it, remove the acquisition condition, and prove the direct
   acquisition test fails. Restore the implementation.

**Commit boundary:**

```text
fix(store): stop leasing cancelled job tickets
```

## Task 4: Document and review the operator boundary

**Files:**

- Modify: `docs/runbooks/operator-real-media-execution.md`

1. Document the exact cancel command and standard envelope outcome.
2. State that pending and ready ticket rows remain visible but cannot acquire
   new leases.
3. State that held leases are not preempted, point to existing inspection
   commands, and do not imply that a scheduler force-release CLI exists.
4. Reread ADR 0046 and the runbook together for any implied worker kill or
   ticket-state rewrite.

**Commit boundary:**

```text
docs(runbook): document job cancellation boundary
```

## Task 5: Rebase, verify, and hand off

1. Wait for campaign predecessors #352 and #343 to merge, then rebase onto
   current `origin/main`. Resolve the lease-acquisition seam by preserving
   #343's store-owned worker predicate and savepoint rollback.
2. Re-read `main...HEAD` for unrelated changes, public error drift, ticket
   mutation, a claimed held-work abort, and candidate-only enforcement.
3. Run:

   ```sh
   just fmt
   git diff --check
   cargo test -p voom-cli --test job_cancel_envelope
   cargo test -p voom-control-plane --lib cases::execution::jobs::tests
   cargo test -p voom-control-plane --lib cases::execution::leases::tests
   cargo test -p voom-store --all-features --lib jobs
   cargo test -p voom-store --all-features --lib ready_for_operations
   just lint
   prek run
   just ci
   ```

4. Run the adversarial review loop to approval, including concurrency, durable
   failure state, and CLI contract passes.
5. Run the simplification review. Apply only behavior-preserving reductions
   and rerun focused tests.
6. Push and open a PR closing #364. Do not merge before #352 and #343. Once
   both are merged, rebase again, rerun focused tests and `just ci`, push, and
   wait for green hosted CI before campaign merge.
