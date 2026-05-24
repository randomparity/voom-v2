# Issue 52 Scheduler Decision Insert SQL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Share scheduler decision insert setup and remove the redundant suppression-equivalence pre-read.

**Architecture:** Keep repository API and schema unchanged. Add private insert-preparation helpers in `scheduler_decisions.rs`, define common insert SQL once, and rely on the upsert `RETURNING` conflict path for incompatible suppression-key reuse.

**Tech Stack:** Rust, sqlx SQLite queries, existing scheduler decision repository tests.

---

### Task 1: Pin Suppression Conflict Behavior

**Files:**
- Modify: `crates/voom-store/src/repo/scheduler_decisions_test.rs`

- [ ] **Step 1: Strengthen existing conflict test**

In `suppression_key_reuse_requires_equivalent_decision`, assert that the error
message still contains `already belongs to a different decision` in addition to
the `Conflict` error code.

- [ ] **Step 2: Run store tests**

Run: `cargo test -p voom-store scheduler_decisions`

Expected: PASS before refactor, proving the current public conflict behavior is
pinned.

### Task 2: Refactor Insert Setup

**Files:**
- Modify: `crates/voom-store/src/repo/scheduler_decisions.rs`

- [ ] **Step 1: Add shared insert constants and helper**

Add private constants for the scheduler decision insert columns and placeholder
values. Add `PreparedSchedulerDecisionInsert` with `now` and `explanation`
fields, plus `prepare_decision_insert(&NewSchedulerDecision)`.

- [ ] **Step 2: Use helper in `create_in_tx`**

Call `prepare_decision_insert`, build the shared insert SQL with
`RETURNING {DECISION_COLS}`, bind using the prepared values, and keep the
existing `scheduler_decisions insert` database error label.

- [ ] **Step 3: Use helper in `create_or_suppress_in_tx`**

Remove `validate_suppression_equivalence_in_tx`. Use the same prepared insert
data and common insert SQL prefix, append the existing upsert clause, and keep
the existing no-row conflict error message.

- [ ] **Step 4: Delete pre-read helper**

Remove `validate_suppression_equivalence_in_tx` if it has no callers.

### Task 3: Verify and Review

**Files:**
- Review: `crates/voom-store/src/repo/scheduler_decisions.rs`
- Review: `crates/voom-store/src/repo/scheduler_decisions_test.rs`

- [ ] **Step 1: Run store tests**

Run: `cargo test -p voom-store scheduler_decisions`

Expected: PASS.

- [ ] **Step 2: Run remote acquire tests**

Run: `cargo test -p voom-control-plane remote_acquire`

Expected: PASS.

- [ ] **Step 3: Run lint**

Run: `just lint`

Expected: PASS.

- [ ] **Step 4: Run adversarial and simplification reviews**

Check for SQL drift, changed conflict behavior, unnecessary helper abstraction,
and any remaining duplicate validation/serialization setup.
