# Issue 88 Remux Failure Context Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace repeated remux failure-event optional argument lists with one local failure context.

**Architecture:** Keep `execute_remux_core` linear. Add a local context type in `crates/voom-control-plane/src/remux/mod.rs` that accumulates known facts and calls the existing event recording API.

**Tech Stack:** Rust, async functions, existing remux event tests, `cargo test`, `just`.

---

## Files

- Modify: `crates/voom-control-plane/src/remux/mod.rs`
- Verify: `crates/voom-control-plane/src/remux/mod_test.rs`
- Verify: `crates/voom-cli/tests/compliance_envelope.rs`

### Task 1: Add Failure Context And Preserve Behavior

- [ ] **Step 1: Run baseline remux tests**

Run:

```bash
cargo test -p voom-control-plane remux
```

Expected: PASS.

- [ ] **Step 2: Add `RemuxFailureContext`**

In `crates/voom-control-plane/src/remux/mod.rs`, add:

```rust
#[derive(Clone, Copy)]
struct RemuxFailureContext<'a> {
    cp: &'a ControlPlane,
    input: &'a ExecuteRemuxInput,
    source_location_id: Option<FileLocationId>,
    selection: Option<&'a RemuxSelection>,
    staging_path: Option<&'a Path>,
    result: Option<&'a RemuxResult>,
    staged: Option<&'a commit::StagedRemuxArtifact>,
}
```

Add `new`, `with_source_location`, `with_selection`, `with_staging_path`,
`with_result`, `with_staged`, and `record_failure` methods. `record_failure`
must call `events::record_failed` with the same `RemuxFailedEventInput` fields
as the current `record_failure` and `record_partial_failure` helpers.

- [ ] **Step 3: Use the context in `execute_remux_core`**

Create `let failure = RemuxFailureContext::new(cp, &input);` at the start.
For each successful fact-producing step, shadow the context:

```rust
let failure = failure.with_source_location(selected.location.id);
let failure = failure.with_selection(&selection);
let failure = failure.with_staging_path(&staging_path);
let failure = failure.with_result(&result);
let failure = failure.with_staged(&staged);
```

Replace every `record_failure(...)` and `record_partial_failure(...)` call in
`execute_remux_core` with `failure.record_failure(&err).await?`.

- [ ] **Step 4: Delete old failure helpers**

Delete `record_failure` and `record_partial_failure` once all call sites use the
context.

- [ ] **Step 5: Verify targeted behavior**

Run:

```bash
cargo test -p voom-control-plane remux
cargo test -p voom-cli --test compliance_envelope execute_scanned_remux_existing_target_outputs_failure_envelope
just fmt-check
```

Expected: PASS.
