# Sprint 11 Simplification Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the low-risk simplification findings from the `main..HEAD` Sprint 11 review without changing staged artifact behavior.

**Architecture:** Keep behavior-preserving cleanups inside existing crate boundaries. Centralize public error-code parsing in `voom-core`, remove redundant control-plane pool adapter traits, and reduce unfiltered artifact-list query work without changing state filtering semantics.

**Tech Stack:** Rust, sqlx/SQLite, existing `just` commands, sibling unit tests.

---

## Review Findings Accepted

- `voom-control-plane::artifact::verify` hand-maintains an `ErrorCode` string parser. Add a single parser on `voom_core::ErrorCode` and reuse it.
- `verify.rs` and `inspect.rs` define private `ControlPlanePool` traits that only return `&self.pool`. Nearby artifact modules use `cp.pool` directly.
- `artifact list` loads every artifact handle before applying the unfiltered limit, and `show/list` does an extra handle lookup before reading the same handle facts.

## Review Findings Deferred

- Filesystem promotion helper consolidation is deferred because `commit.rs` and `fs.rs` intentionally differ in cleanup/recovery behavior after install failures. Combining them safely needs a separate recovery-focused plan and tests.
- Full repository-level centralization of handle facts and live-staging selection is deferred because it changes the `ArtifactRepo` trait and several call sites. The accepted cleanup removes the adapter traits and one extra query first.

## Task 1: Centralize ErrorCode Parsing

**Files:**
- Modify: `crates/voom-core/src/error.rs`
- Modify: `crates/voom-core/src/error_test.rs`
- Modify: `crates/voom-control-plane/src/artifact/verify.rs`

- [ ] **Step 1: Add a failing parser test**

Add a test in `crates/voom-core/src/error_test.rs`:

```rust
#[test]
fn error_code_from_wire_str_round_trips_every_variant() {
    for code in ErrorCode::ALL {
        let parsed = ErrorCode::from_wire_str(code.as_str()).unwrap();
        assert_eq!(parsed, code);
    }
    assert!(ErrorCode::from_wire_str("NOT_A_CODE").is_none());
}
```

Run:

```bash
cargo test -p voom-core error_code_from_wire_str_round_trips_every_variant
```

Expected: compile failure because `ErrorCode::ALL` and `ErrorCode::from_wire_str` do not exist.

- [ ] **Step 2: Add the parser**

In `crates/voom-core/src/error.rs`, add `ErrorCode::ALL` and `ErrorCode::from_wire_str`.

- [ ] **Step 3: Use the parser in verification reporting**

Replace the local candidate loop in `crates/voom-control-plane/src/artifact/verify.rs` with `ErrorCode::from_wire_str`.

- [ ] **Step 4: Verify**

Run:

```bash
cargo test -p voom-core error_code_from_wire_str_round_trips_every_variant
cargo test -p voom-control-plane artifact::verify
```

Expected: both commands pass.

## Task 2: Remove Redundant Pool Adapter Traits

**Files:**
- Modify: `crates/voom-control-plane/src/artifact/verify.rs`
- Modify: `crates/voom-control-plane/src/artifact/inspect.rs`

- [ ] **Step 1: Replace adapter calls**

Replace `cp.pool_for_test_or_internal()` with `&cp.pool` in `verify.rs` and `inspect.rs`.

- [ ] **Step 2: Delete local traits**

Delete each private `ControlPlanePool` trait and impl from `verify.rs` and `inspect.rs`.

- [ ] **Step 3: Verify**

Run:

```bash
cargo test -p voom-control-plane artifact::verify artifact::inspect
```

Expected: tests pass.

## Task 3: Simplify Artifact Inspection Queries

**Files:**
- Modify: `crates/voom-control-plane/src/artifact/inspect.rs`
- Modify: `crates/voom-control-plane/src/artifact/inspect_test.rs`

- [ ] **Step 1: Add characterization coverage**

Add tests showing missing artifact still returns `NOT_FOUND` and unfiltered list returns newest handles up to the requested limit.

- [ ] **Step 2: Remove extra handle lookup**

Make `read_handle_facts` use `fetch_optional` and return `NotFound` when the handle is absent. Use that result as the handle identity in `build_artifact_detail` instead of calling `get_handle` first.

- [ ] **Step 3: Limit unfiltered handle loading**

Change `list_handle_ids_newest_first` to accept an optional limit. For `ArtifactListInput { state: None, limit }`, include `LIMIT ?` in the SQL. For state-filtered lists, keep loading all handle IDs because filtering still depends on derived detail state.

- [ ] **Step 4: Verify**

Run:

```bash
cargo test -p voom-control-plane artifact::inspect
```

Expected: tests pass.

## Final Verification And Commit

- [ ] Run `just fmt`.
- [ ] Run `just ci`.
- [ ] Commit the simplification changes:

```bash
git add crates/voom-core/src/error.rs crates/voom-core/src/error_test.rs crates/voom-control-plane/src/artifact/verify.rs crates/voom-control-plane/src/artifact/inspect.rs crates/voom-control-plane/src/artifact/inspect_test.rs docs/superpowers/plans/2026-05-25-sprint-11-simplification-review.md
git commit -m "refactor(control-plane): simplify artifact review findings"
```
