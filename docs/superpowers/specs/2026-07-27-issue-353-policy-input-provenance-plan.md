# Issue #353 Policy Input Snapshot Provenance Implementation Plan

**Goal:** Reject missing, non-file, or mismatched generic policy-input snapshot
links inside the write transaction without persisting partial aggregate state.

**Design:** Convert the draft into an opaque validation proof, then prepare an
opaque, sorted, deduplicated, JSON-encoded identity query before taking a
database lock. Use that query for one `json_each`-backed identity-repository
read in an immediate SQLite transaction, validate every linked media member in
memory, then pass the proof to the policy-input repository without repeating
whole-draft validation. Keep the existing `NOT_FOUND` and `CONFLICT` codes and
make no persisted schema or wire changes.

**Success criteria:**

- every linked generic member targets the snapshot's exact file version;
- missing snapshots are `NOT_FOUND`, while non-file and mismatched links are
  `CONFLICT`;
- no aggregate table or event changes after a rejected write;
- valid current and historical links remain writable;
- the in-transaction repository API cannot accept an unvalidated draft or
  repeat validation under the immediate writer lock;
- public failures follow the design's model, transaction, identity-read, then
  draft-order member precedence;
- scan-import behavior is unchanged; and
- focused tests and `just ci` pass without warnings.

## Task 1: Pin provenance, precedence, and rollback with failing tests

**Files:**

- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs_test.rs`

1. Add a helper that turns the existing scanned-snapshot setup into a generic
   `PolicyInputSetDraft` with a `FileVersion` target and linked snapshot.
2. Add a durable-state helper that reads counts for all eight policy-input
   aggregate tables.
3. Add an event helper using `ControlPlane::list_events`.
4. For tests that close the subject pool, open a second `ControlPlane` on a
   separate pool first. Use that observer for all table and event post-state
   reads after the subject pool closes.
5. Add behavior tests for:
   - an exact valid link round-trip;
   - an invalid model plus invalid link, with both a live and closed subject
     pool, proving model validation wins before transaction acquisition;
   - a valid model with a closed subject pool, proving transaction failure is
     `DB_UNREACHABLE`;
   - a missing snapshot;
   - a valid snapshot linked from a non-file target;
   - one member combining a missing snapshot and non-file target, proving
     `NOT_FOUND` wins;
   - two invalid members whose snapshot-ID order differs from their draft
     order, proving the first draft member's contextual error wins;
   - two members where the first link is valid and the second belongs to a
     different file version;
   - a valid model with a member-level conflict after
     `media_snapshots` is renamed, proving the bulk identity-read database
     error wins before member validation by asserting the lookup operation's
     error context, not only its public code; and
   - an exact historical link after the selected version is retired.
6. On every failure, assert the exact public error code, zero rows in every
   aggregate table, and an unchanged event count through a usable observer.
7. Run and confirm the intended failures:

   ```sh
   cargo test -p voom-control-plane --lib cases::policy::policy_inputs::tests
   ```

   The current generic writer returns database errors for missing IDs, accepts
   non-file and mismatched links, lets a closed pool win before model
   validation, and has no bulk-read failure tier.

**Checkpoint:** keep the red tests uncommitted until their implementation is
green; record the observed failure reasons before Task 2. Do not invoke commit
hooks against a red working tree.

## Task 2: Make validation state explicit without breaking callers

**Files:**

- Modify: `crates/voom-policy/src/data/model.rs`
- Modify: `crates/voom-policy/src/data/model_test.rs`
- Modify: `crates/voom-policy/src/lib.rs`
- Modify: `crates/voom-store/src/repo/policy/policy_inputs.rs`
- Modify: `crates/voom-store/src/repo/policy/policy_inputs_test.rs`
- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs.rs`

1. Add `ValidatedPolicyInputSetDraft` in the policy domain. Its constructor
   consumes a raw draft, runs `validate_input_set`, and returns the existing
   validation error type.
2. Expose immutable inspection and consuming extraction only. Do not implement
   mutable dereferencing or an unchecked constructor.
3. Replace `SqlitePolicyInputRepo::create_input_set_in_tx`'s raw draft
   parameter with the proof and remove its internal validation pass.
4. Keep `create_input_set` accepting a raw draft, but construct the proof
   before opening its transaction.
5. Update both control-plane callers in the same change. Generic creation
   constructs the proof before its existing transaction begin at this
   checkpoint; scan creation constructs it after its required reads and before
   the first write.
6. Add policy tests that valid input is preserved and invalid input cannot
   produce a proof.
7. Require the whole workspace to compile at this boundary:

   ```sh
   cargo test -p voom-policy --lib data::model::tests
   cargo test -p voom-store --all-features --lib policy_inputs
   cargo check --workspace --all-features --all-targets
   ```

   The closed-pool model-precedence test from Task 1 turns green here; the
   provenance and bulk-read cases remain red.

**Checkpoint:** the proof, repository API replacement, and every caller form
one compile-green API change, but remain uncommitted while the Task 1 behavior
tests are red. Record the passing component tests and workspace compilation.

## Task 3: Add and distinguish the set-based identity read

**Files:**

- Modify: `crates/voom-store/src/repo/media/identity.rs`
- Modify: `crates/voom-store/src/repo/media/identity_test.rs`

1. Add an opaque `MediaSnapshotFileVersionQuery` whose constructor sorts,
   deduplicates, converts, and JSON-encodes snapshot IDs before a transaction
   begins. Keep the encoded representation private.
2. Add a named SQL constant containing the one-bind `json_each` query.
3. Add
   `IdentityRepo::get_media_snapshot_file_versions_in_tx`, accepting the
   prepared query and returning deterministic
   `(snapshot ID, file-version ID)` pairs for the IDs that exist.
4. Return an empty vector before executing SQL when the prepared input is
   empty.
5. Order results by snapshot ID. Missing IDs are omitted for the policy caller
   to classify; duplicate inputs yield one row.
6. Add store tests that:
   - rename `media_snapshots` inside a dedicated transaction, prove an empty
     prepared query succeeds without touching SQL, and prove a non-empty query
     fails;
   - inspect the exact production SQL constant and assert it contains
     `json_each(?)` and exactly one placeholder. SQLite exposes its
     connection-local variable-number limit only through `sqlite3_limit`, SQLx
     has no safe setter, and the workspace forbids unsafe code. Record this
     constraint rather than weakening the crate's safety policy for a test;
   - cover duplicate, existing, and missing IDs with stable result ordering;
     and
   - submit more than 1,000 IDs as a behavior check without claiming that
     count is the runtime parameter limit or a supported aggregate-size bound.
7. Run:

   ```sh
   cargo test -p voom-store --all-features --lib \
     get_media_snapshot_file_versions_in_tx
   ```

**Checkpoint:** tests and implementation stay uncommitted with their consumer
because the method has no independent product behavior. Record the focused
test result.

## Task 4: Validate every generic link before insertion

**Files:**

- Modify:
  `crates/voom-control-plane/src/cases/policy/policy_inputs.rs`

1. Inspect the proof to collect linked snapshot IDs and construct
   `MediaSnapshotFileVersionQuery` before changing generic creation to
   `begin_immediate_tx`.
2. Call
   `IdentityRepo::get_media_snapshot_file_versions_in_tx` once with the
   prepared query and index its result by snapshot ID.
3. Iterate every linked member in original draft order.
4. Return `NotFound` when its snapshot ID is absent from the result.
5. Return `Conflict` when the linked member does not target a file version or
   when the snapshot's version differs.
6. Pass the proof to `create_input_set_in_tx` only after the whole linked list
   passes, then commit.
7. Run the focused tests from Tasks 1 through 3. All must pass.
8. Run the existing scan-input tests to prove the specialized liveness
   contract remains unchanged:

   ```sh
   cargo test -p voom-control-plane --lib create_policy_input_set_from_scan
   ```

9. Perform and restore these focused mutations, confirming the named behavior
   test fails for each:
   - remove the missing-snapshot branch;
   - remove the non-file target branch;
   - check target shape before snapshot existence;
   - traverse linked members by sorted snapshot ID instead of draft order; and
   - remove exact-version equality.

**Commit boundary:**

```text
fix(control-plane): validate policy input provenance
```

Commit the Task 1 tests, proof API, set-based identity read, provenance check,
and atomic write boundary together only after the complete working tree is
green. This is one logical behavior change and keeps hooks from observing a
deliberately red intermediate state.

## Task 5: Verify and hand off

1. Re-read the complete `main...HEAD` diff for unrelated changes, insertions
   before validation, error-code drift, accidental liveness checks, and any
   claim to solve #375's independent writer-budget problem.
2. Run:

   ```sh
   just fmt
   git diff --check
   cargo test -p voom-policy --lib data::model::tests
   cargo test -p voom-store --all-features --lib policy_inputs
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
5. Commit any review fixes as separate logical changes after their focused
   verification.
6. Push and open a PR closing #353, but do not merge it before #344, #346, and
   #358. After those campaign PRs merge, rebase onto current main, rerun the
   focused tests and `just ci`, push, and wait for green CI before merge.
