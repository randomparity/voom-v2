# Sprint 9 Simplification Review Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the low-risk simplification findings from the `main..HEAD` simplification review without changing scheduler behavior or CLI output.

**Architecture:** Keep scheduler scoring vocabulary owned by the store persistence enum where the control plane writes durable decisions. Keep remote-acquire suppression key text stable while removing duplicated formatting. Tighten scheduler candidate projections so scoring receives only facts it uses.

**Tech Stack:** Rust workspace, tokio/sqlx, serde JSON, `just` command runner.

---

## Accepted Review Findings

- Reuse store-side `SchedulerReasonCode` parsing instead of duplicating the reason string match in remote acquire.
- Share remote-acquire suppression key formatting between normal no-work decisions and capacity rechecks.
- Remove unused `TicketCandidate::payload` and the clone from `candidate_from_ticket`.
- Compute scheduler artifact access mode once per candidate and pass it into gate evaluation.

## Deferred Findings

- Typed scheduler reason API across `voom-scheduler` and `voom-store`: useful, but broader than a surgical cleanup.
- CLI DTO flattening: possible, but field order is part of the agent-facing snapshot contract and the reduction is not worth the contract churn here.
- Scheduler decision insert SQL extraction and suppression pre-read removal: worthwhile future store cleanup, but SQL conflict behavior is load-bearing and deserves its own focused change.
- Removing selected-path capacity re-reads: likely intentional as a transaction-local recheck before lease creation, so this plan leaves it explicit.
- Reworking multi-operation scorer buckets to avoid candidate clones: medium-risk API work; removing payload first captures the easy win.

## Files

- Modify: `crates/voom-store/src/repo/scheduler_decisions.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Modify: `crates/voom-scheduler/src/lib.rs`
- Modify: `crates/voom-scheduler/src/lib_test.rs`

---

### Task 1: Reuse Store Reason Parsing

- [ ] **Step 1: Expose a store-side reason parser**

In `crates/voom-store/src/repo/scheduler_decisions.rs`, make the existing parser public within the crate API:

```rust
impl SchedulerReasonCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        // existing body unchanged
    }

    pub fn parse(s: &str) -> Result<Self, VoomError> {
        // existing body unchanged
    }
}
```

- [ ] **Step 2: Replace the control-plane duplicate match**

In `crates/voom-control-plane/src/cases/remote_execution.rs`, replace the local `scheduler_reason` match with:

```rust
fn scheduler_reason(reason: &str) -> Result<SchedulerReasonCode, VoomError> {
    SchedulerReasonCode::parse(reason).map_err(|_| {
        VoomError::Internal(format!(
            "scheduler reason {reason:?} is not mapped to the persistence vocabulary"
        ))
    })
}
```

- [ ] **Step 3: Verify focused behavior**

Run:

```bash
cargo test -p voom-control-plane decision_from_score_rejects_unknown_reason_code
```

Expected: PASS.

### Task 2: Share Suppression Key Formatting

- [ ] **Step 1: Add a common formatter**

In `crates/voom-control-plane/src/cases/remote_execution.rs`, add:

```rust
fn remote_acquire_suppression_key(
    input: &RemoteAcquireInput,
    reason: &str,
    operation_fingerprint: &str,
) -> String {
    let bucket = input.lease_ttl_seconds.max(1) / 30;
    format!(
        "remote_acquire:node:{}:worker:{}:reason:{}:ops:{}:bucket:{}",
        input.node_id, input.worker_id, reason, operation_fingerprint, bucket
    )
}
```

- [ ] **Step 2: Delegate existing key builders**

Change `suppression_key` to call `remote_acquire_suppression_key(input, score.reason_code, &operation_fingerprint(&score.explanation))`.

Change `capacity_suppression_key` to call `remote_acquire_suppression_key(input, reason, operation)`.

- [ ] **Step 3: Verify pinned key behavior**

Run:

```bash
cargo test -p voom-control-plane suppression_key_includes_operation_fingerprint capacity_suppression_key_includes_operation_fingerprint
```

Expected: PASS.

### Task 3: Remove Unused Scheduler Candidate Payload

- [ ] **Step 1: Delete the field**

In `crates/voom-scheduler/src/lib.rs`, remove `pub payload: JsonValue` from `TicketCandidate`.

- [ ] **Step 2: Delete call-site clones**

In `crates/voom-control-plane/src/cases/remote_execution.rs`, remove `payload: ticket.payload.clone(),` from `candidate_from_ticket`.

In `crates/voom-scheduler/src/lib_test.rs`, remove the `payload: json!(...)` field from test candidate construction.

- [ ] **Step 3: Verify scheduler and remote acquire tests**

Run:

```bash
cargo test -p voom-scheduler
cargo test -p voom-control-plane remote_acquire
```

Expected: PASS.

### Task 4: Avoid Double Artifact Access Scans

- [ ] **Step 1: Change gate helper signature**

In `crates/voom-scheduler/src/lib.rs`, change:

```rust
fn hard_gate_reasons(candidate: &SchedulerCandidate) -> Vec<&'static str>
```

to:

```rust
fn hard_gate_reasons(
    candidate: &SchedulerCandidate,
    access_mode: Option<&str>,
) -> Vec<&'static str>
```

Inside the helper, replace `select_access_mode(&candidate.worker.artifact_access).is_none()` with `access_mode.is_none()`.

- [ ] **Step 2: Pass the precomputed mode**

In `SchedulerScorer::score`, compute `access_mode` before gate evaluation and call:

```rust
let access_mode = select_access_mode(&candidate.worker.artifact_access);
let reasons = hard_gate_reasons(candidate, access_mode);
```

- [ ] **Step 3: Verify scorer behavior**

Run:

```bash
cargo test -p voom-scheduler
```

Expected: PASS.

### Task 5: Final Verification and Commit

- [ ] **Step 1: Format and run focused checks**

Run:

```bash
just fmt
cargo test -p voom-scheduler
cargo test -p voom-control-plane remote_acquire
cargo test -p voom-control-plane decision_from_score_rejects_unknown_reason_code
just lint
```

Expected: all commands PASS.

- [ ] **Step 2: Commit**

Run:

```bash
git status --short
git add crates/voom-store/src/repo/scheduler_decisions.rs \
  crates/voom-control-plane/src/cases/remote_execution.rs \
  crates/voom-scheduler/src/lib.rs \
  crates/voom-scheduler/src/lib_test.rs \
  docs/superpowers/plans/2026-05-24-sprint-9-simplification-review-fixes.md
git commit -m "refactor: simplify scheduler review findings"
```
