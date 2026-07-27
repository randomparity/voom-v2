# Issue #353 Policy Input Snapshot Provenance Implementation Plan

**Goal:** Reject missing, non-file, or mismatched generic policy-input snapshot
links inside the write transaction without persisting partial aggregate state.

**Design:** Validate the complete draft and prepare an opaque, sorted,
deduplicated, JSON-encoded identity query before taking a database lock. Use
that query for one `json_each`-backed identity-repository read in an immediate
SQLite transaction, validate every linked media member in memory, then call the
policy-input repository. Keep the existing `NOT_FOUND` and `CONFLICT` codes and
make no schema or wire changes.

**Success criteria:**

- every linked generic member targets the snapshot's exact file version;
- missing snapshots are `NOT_FOUND`, while non-file and mismatched links are
  `CONFLICT`;
- no aggregate table or event changes after a rejected write;
- valid current and historical links remain writable;
- scan-import behavior is unchanged; and
- focused tests and `just ci` pass without warnings.

## Task 1: Add a set-based bulk identity read

**Files:**

- Modify: `crates/voom-store/src/repo/media/identity.rs`
- Modify: `crates/voom-store/src/repo/media/identity_test.rs`

1. Add an opaque `MediaSnapshotFileVersionQuery` whose constructor sorts,
   deduplicates, converts, and JSON-encodes snapshot IDs before a transaction
   begins. Keep the encoded representation private.
2. Add
   `IdentityRepo::get_media_snapshot_file_versions_in_tx`, accepting the
   prepared query and returning deterministic
   `(snapshot ID, file-version ID)` pairs for the IDs that exist.
3. Return an empty vector before constructing SQL when the prepared input is
   empty.
4. Bind the pre-encoded JSON array to one query using
   `IN (SELECT value FROM json_each(?))`.
5. Order results by snapshot ID. Missing IDs are omitted for the policy caller
   to classify; duplicate inputs yield one row.
6. Add store tests for empty input, duplicate/existing/missing IDs, stable
   ordering, and a request larger than SQLite's ordinary parameter limit.
   Treat the large request as a one-bind query-shape test, not a supported
   aggregate-size claim; #375 owns the writer budget.
7. Run:

   ```sh
   cargo test -p voom-store --all-features --lib \
     get_media_snapshot_file_versions_in_tx
   ```

**Commit boundary:** tests and implementation stay in the issue's one logical
behavior commit because the method has no consumer until Task 3.

## Task 2: Pin generic provenance and atomicity with failing tests

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

## Task 3: Validate every link before insertion

**Files:**

- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs.rs`

1. Run `voom_policy::validate_input_set` before acquiring the write lock and
   map the error to the existing `PolicyValidationError` contract.
2. Collect linked snapshot IDs and construct
   `MediaSnapshotFileVersionQuery` before changing the generic writer to
   `begin_immediate_tx`.
3. Call
   `IdentityRepo::get_media_snapshot_file_versions_in_tx` once with the
   prepared IDs and index its result by snapshot ID.
4. Iterate every linked member in draft order.
5. Return `NotFound` when its snapshot ID is absent from the result.
6. Return `Conflict` when the linked member does not target a file version or
   when the snapshot's version differs.
7. Call `create_input_set_in_tx` only after the whole linked list passes, then
   commit.
8. Run the focused tests from Tasks 1 and 2. All must pass.
9. Run the existing scan-input tests to prove the specialized liveness
   contract remains unchanged:

   ```sh
   cargo test -p voom-control-plane --lib create_policy_input_set_from_scan
   ```

10. Temporarily remove the exact-version comparison, confirm the mismatch test
   fails because the write succeeds, then restore the comparison.

**Commit boundary:**

```text
fix(control-plane): validate policy input provenance
```

## Task 4: Verify and hand off

1. Re-read the complete `main...HEAD` diff for unrelated changes, insertions
   before validation, error-code drift, accidental liveness checks, and any
   claim to solve #375's independent writer-budget problem.
2. Run:

   ```sh
   just fmt
   git diff --check
   cargo test -p voom-store --all-features --lib \
     get_media_snapshot_file_versions_in_tx
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
