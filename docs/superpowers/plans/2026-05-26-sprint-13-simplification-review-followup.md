# Sprint 13 Simplification Review Follow-Up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce low-risk complexity found by the branch simplification review and track larger valid recommendations as GitHub issues.

**Architecture:** Keep this branch focused on behavior-preserving simplifications. Apply small local refactors where existing tests can prove unchanged behavior; defer cross-crate schema, planner traversal, and shared worker-dispatch extraction because those touch public envelopes or broader architectural boundaries.

**Tech Stack:** Rust workspace, tokio, sqlx, serde, `just`, GitHub CLI.

---

## Review Decision

Implement now:
- Remove redundant remux dispatch wrapper in `crates/voom-control-plane/src/remux/dispatch.rs`.
- Compare remux validation iterators directly instead of collecting temporary vectors in `crates/voom-control-plane/src/remux/dispatch.rs`.
- Return borrowed mkvmerge tracks and reuse the selected kept-track mapping in `crates/voom-mkvtoolnix-worker/src/mkvmerge.rs`.

Defer as GitHub issues:
- Shared remux operation payload model and single validation contract.
- Single planner operation walker for conditionals/rules and remux grouping.
- Remux failure context accumulator for event recording.
- Shared worker NDJSON dispatch and bundled binary discovery helpers.
- Shared policy source resolver for transcode/remux workflow execution.
- Shared SQLite locked retry helper for workflow lease operations.

## Files

- Modify: `crates/voom-control-plane/src/remux/dispatch.rs`
- Modify: `crates/voom-control-plane/src/workflow/executor.rs`
- Modify: `crates/voom-control-plane/src/remux/dispatch_test.rs`
- Modify: `crates/voom-mkvtoolnix-worker/src/mkvmerge.rs`
- Verify: `crates/voom-mkvtoolnix-worker/src/mkvmerge_test.rs`
- Verify: `crates/voom-mkvtoolnix-worker/src/handler_test.rs`
- Verify: `crates/voom-control-plane/src/remux/dispatch_test.rs`

### Task 1: Simplify Remux Dispatch Wrapper And Result Comparison

- [ ] **Step 1: Add missing default mismatch coverage**

Add this test to `crates/voom-control-plane/src/remux/dispatch_test.rs`:

```rust
#[test]
fn validate_result_rejects_mismatched_default_stream_order() {
    let mut result = remux_result();
    result.default_snapshot_stream_ids = vec!["stream-a".to_owned()];

    let err = validate_result(&selected_source(), &selection, &result).unwrap_err();

    assert!(err
        .to_string()
        .contains("remux result default stream ids do not match request"));
}
```

- [ ] **Step 2: Run test to verify current behavior is covered**

Run: `cargo test -p voom-control-plane validate_result_rejects_mismatched_default_stream_order`

Expected: PASS. This is characterization coverage before refactoring.

- [ ] **Step 3: Remove redundant wrapper and direct vector allocations**

In `crates/voom-control-plane/src/remux/dispatch.rs`, make `dispatch_remux_with_client` call `dispatch_remux_with_client_context_and_progress` directly. Remove `dispatch_remux_with_client_context`.

Change `validate_result` to compare mapped iterators with `.eq(...)`, preserving the existing error messages.

- [ ] **Step 4: Update call sites**

In `crates/voom-control-plane/src/workflow/executor.rs`, change the remux execution call from `dispatch_remux_with_client_context` to `dispatch_remux_with_client_context_and_progress` with the same arguments.

- [ ] **Step 5: Verify targeted control-plane tests**

Run: `cargo test -p voom-control-plane remux::dispatch`

Expected: PASS.

### Task 2: Borrow Mkvmerge Tracks And Reuse Kept Mapping

- [ ] **Step 1: Change mapping lookup to borrow**

In `crates/voom-mkvtoolnix-worker/src/mkvmerge.rs`, change:

```rust
pub(crate) fn track_for_provider_index(&self, provider_index: u32) -> Option<MkvmergeTrack>
```

to:

```rust
pub(crate) fn track_for_provider_index(&self, provider_index: u32) -> Option<&MkvmergeTrack>
```

- [ ] **Step 2: Make selected tracks borrowed**

Change `selected_tracks` to return `Vec<&MkvmergeTrack>`, and update `extend_group_selection`, `extend_optional_group_selection`, and `track_order` to accept borrowed tracks.

- [ ] **Step 3: Reuse the selected kept tracks**

In `build_mkvmerge_args`, pass the already computed `keep` slice into `track_order` instead of resolving `selection.keep_streams` a second time. Keep missing-track error messages unchanged because they are already covered by tests.

- [ ] **Step 4: Verify targeted worker tests**

Run: `cargo test -p voom-mkvtoolnix-worker mkvmerge`

Expected: PASS.

Run: `cargo test -p voom-mkvtoolnix-worker handler`

Expected: PASS.

### Task 3: Deferred Issue Filing

- [ ] **Step 1: Confirm GitHub CLI repository access**

Run: `gh repo view --json nameWithOwner`

Expected: JSON with the current GitHub repository.

- [ ] **Step 2: File issues for deferred recommendations**

Create one issue per deferred recommendation listed in the review decision. Each issue should include evidence paths, expected simplification, and risk/verification notes.

Run one `gh issue create` command per issue.

Expected: GitHub returns created issue URLs.

### Task 4: Final Verification And Commit

- [ ] **Step 1: Format**

Run: `just fmt`

Expected: exit 0.

- [ ] **Step 2: Targeted tests**

Run: `cargo test -p voom-control-plane remux::dispatch`

Expected: PASS.

Run: `cargo test -p voom-mkvtoolnix-worker mkvmerge`

Expected: PASS.

Run: `cargo test -p voom-mkvtoolnix-worker handler`

Expected: PASS.

- [ ] **Step 3: Inspect diff**

Run: `git diff --stat` and `git diff --check`

Expected: diff limited to planned files plus this plan document; `git diff --check` exits 0.

- [ ] **Step 4: Commit**

Run:

```bash
git add docs/superpowers/plans/2026-05-26-sprint-13-simplification-review-followup.md crates/voom-control-plane/src/remux/dispatch.rs crates/voom-control-plane/src/remux/dispatch_test.rs crates/voom-control-plane/src/workflow/executor.rs crates/voom-mkvtoolnix-worker/src/mkvmerge.rs
git commit -m "refactor: simplify remux review findings"
```

Expected: commit succeeds.
