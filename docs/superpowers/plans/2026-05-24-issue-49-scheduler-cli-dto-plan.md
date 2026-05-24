# Issue 49 Scheduler CLI DTO Mapping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove meaningful duplication from scheduler CLI decision DTO mapping while preserving exact JSON output.

**Architecture:** Keep the existing list and show DTO structs as the serialization boundary. Add a private scalar helper that performs the shared `SchedulerDecision` field mapping once, then use it from both DTO conversions.

**Tech Stack:** Rust, serde serialization, sqlx-backed scheduler decision fixtures, insta snapshots.

---

### Task 1: Pin List and Show DTO Mapping Behavior

**Files:**
- Modify: `crates/voom-cli/src/commands/scheduler_test.rs`

- [ ] **Step 1: Strengthen DTO mapping tests**

Add assertions to `decision_data_maps_full_record` so the unit test protects the
full shared scalar mapping for both DTOs, not only `id`, `outcome`, and
`explanation_json`:

```rust
let summary = DecisionSummaryData::from(created.clone());
let data = DecisionData::from(created);

assert_eq!(summary.id, 1);
assert_eq!(summary.created_at, "1970-01-01 0:00:00.0 +00:00:00");
assert_eq!(summary.outcome, "selected");
assert_eq!(summary.reason_code, "selected");
assert_eq!(summary.summary, "selected");
assert_eq!(summary.request_worker_id, Some(2));
assert_eq!(summary.request_node_id, Some(1));
assert_eq!(summary.ticket_id, Some(3));
assert_eq!(summary.selected_worker_id, Some(2));
assert_eq!(summary.selected_node_id, Some(1));
assert_eq!(summary.selected_lease_id, None);
assert_eq!(summary.candidate_count, 1);
assert_eq!(summary.selected_score, Some(100));
assert_eq!(summary.suppressed_count, 0);

assert_eq!(data.created_at, summary.created_at);
assert_eq!(data.updated_at, "1970-01-01 0:00:00.0 +00:00:00");
assert_eq!(data.reason_code, summary.reason_code);
assert_eq!(data.summary, summary.summary);
assert_eq!(data.request_worker_id, summary.request_worker_id);
assert_eq!(data.request_node_id, summary.request_node_id);
assert_eq!(data.ticket_id, summary.ticket_id);
assert_eq!(data.selected_worker_id, summary.selected_worker_id);
assert_eq!(data.selected_node_id, summary.selected_node_id);
assert_eq!(data.selected_lease_id, summary.selected_lease_id);
assert_eq!(data.candidate_count, summary.candidate_count);
assert_eq!(data.selected_score, summary.selected_score);
assert_eq!(data.suppressed_count, summary.suppressed_count);
assert_eq!(data.explanation_json, json!({"scoring_version":1}));
```

- [ ] **Step 2: Run test to verify baseline**

Run: `cargo test -p voom-cli scheduler::tests::decision_data_maps_full_record`

Expected: PASS, proving the existing behavior is pinned before refactor. This is
a behavior-preserving refactor, so the test should already pass before the
production edit.

### Task 2: Extract Shared Scalar Mapping

**Files:**
- Modify: `crates/voom-cli/src/commands/scheduler.rs`

- [ ] **Step 1: Add private scalar helper**

Add `DecisionScalarData` with all fields shared by `DecisionSummaryData` and
`DecisionData`, and implement `From<&SchedulerDecision>` for it. The helper must
own mapped values, including `String` fields and numeric ID projections.

- [ ] **Step 2: Update DTO conversions**

Change `DecisionSummaryData::from(SchedulerDecision)` and
`DecisionData::from(SchedulerDecision)` to construct `DecisionScalarData` once
from a borrowed decision, then assign the serialized DTO fields in their current
order.

- [ ] **Step 3: Run focused tests**

Run: `cargo test -p voom-cli scheduler`

Expected: PASS.

Run: `cargo test -p voom-cli --test scheduler_envelope`

Expected: PASS with no snapshot changes.

### Task 3: Verify and Review

**Files:**
- Review: `crates/voom-cli/src/commands/scheduler.rs`
- Review: `crates/voom-cli/src/commands/scheduler_test.rs`
- Review: `docs/superpowers/specs/2026-05-24-issue-49-scheduler-cli-dto-design.md`

- [ ] **Step 1: Run lint**

Run: `just lint`

Expected: PASS.

- [ ] **Step 2: Run adversarial code review**

Review the working-tree diff for output-shape changes, field-order regressions,
and whether the helper genuinely centralizes shared mapping.

- [ ] **Step 3: Run simplification review**

Review the final diff for unnecessary helper layering or avoidable copies.
