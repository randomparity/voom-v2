# Issue #353 Policy Input Snapshot Provenance Implementation Plan

**Goal:** Reject missing, non-file, or mismatched generic policy-input snapshot
links inside the write transaction without persisting partial aggregate state.

**Design:** Validate the complete draft and every linked media member before
calling the policy-input repository. Use an immediate SQLite transaction,
identity-repository snapshot reads, the existing `NOT_FOUND` and `CONFLICT`
codes, and no schema or wire changes.

**Success criteria:**

- every linked generic member targets the snapshot's exact file version;
- missing snapshots are `NOT_FOUND`, while non-file and mismatched links are
  `CONFLICT`;
- no aggregate table or event changes after a rejected write;
- valid current and historical links remain writable;
- scan-import behavior is unchanged; and
- focused tests and `just ci` pass without warnings.

## Task 1: Pin generic provenance and atomicity with failing tests

**Files:**

- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs_test.rs`

1. Add a helper that turns the existing scanned-snapshot setup into a generic
   `PolicyInputSetDraft` with a `FileVersion` target and linked snapshot.
2. Add a durable-state helper that reads counts for all eight policy-input
   aggregate tables.
3. Add an event helper using `ControlPlane::list_events`.
4. Add behavior tests for:
   - an exact valid link round-trip;
   - a missing snapshot;
   - a valid snapshot linked from a non-file target;
   - two members where the first link is valid and the second belongs to a
     different file version; and
   - an exact historical link after the selected version is retired.
5. On each failure, assert the exact public error code, zero rows in every
   aggregate table, and an unchanged event count.
6. Run and confirm the intended failures:

   ```sh
   cargo test -p voom-control-plane --lib cases::policy::policy_inputs::tests
   ```

   The valid-link test currently passes by foreign key alone; the missing
   snapshot fails with a database error rather than `NOT_FOUND`, while the
   non-file and mismatch cases incorrectly succeed.

**Commit boundary:** tests are committed with the implementation because a
test-only commit would leave the branch red.

## Task 2: Validate every link before insertion

**Files:**

- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs.rs`

1. Change the generic writer to `begin_immediate_tx`.
2. Run `voom_policy::validate_input_set` before provenance reads and map the
   error to the existing `PolicyValidationError` contract.
3. Iterate every linked media member and load the named snapshot with
   `IdentityRepo::get_media_snapshot_in_tx`.
4. Return `NotFound` for a missing snapshot.
5. Return `Conflict` when the linked member does not target a file version or
   when the snapshot's version differs.
6. Call `create_input_set_in_tx` only after the whole linked list passes, then
   commit.
7. Run the focused tests from Task 1. All must pass.
8. Run the existing scan-input tests to prove the specialized liveness
   contract remains unchanged:

   ```sh
   cargo test -p voom-control-plane --lib create_policy_input_set_from_scan
   ```

9. Temporarily remove the exact-version comparison, confirm the mismatch test
   fails because the write succeeds, then restore the comparison.

**Commit boundary:**

```text
fix(control-plane): validate policy input provenance
```

## Task 3: Verify and hand off

1. Re-read the complete `main...HEAD` diff for unrelated changes, insertions
   before validation, error-code drift, and accidental liveness checks.
2. Run:

   ```sh
   just fmt
   git diff --check
   cargo test -p voom-control-plane --lib cases::policy::policy_inputs::tests
   just lint
   prek run
   just ci
   ```

3. Run the adversarial branch review loop to approval. Because caller-supplied
   provenance is a trust boundary, include a security-focused pass.
4. Run the simplification review. Apply only behavior-preserving reductions
   and rerun focused tests.
5. Commit the approved design, plan, implementation, and tests as the one
   logical behavior change.
6. Push and open a PR closing #353, but do not merge it before #344, #346, and
   #358. After those campaign PRs merge, rebase on current main, rerun the
   focused tests and `just ci`, push, and wait for green CI before merge.
