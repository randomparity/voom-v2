# Issue 53 Candidate Clone Evaluation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document why the remaining per-operation candidate clones are acceptable under current remote-acquire scope.

**Architecture:** No behavior or scorer API change. Add one focused comment near the per-operation grouping in `score_remote_candidates`.

**Tech Stack:** Rust, existing scheduler and remote-acquire tests.

---

### Task 1: Document Candidate Clone Tradeoff

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`

- [ ] **Step 1: Add grouping comment**

Add a short comment before the `by_operation` grouping explaining that
multi-operation scoring keeps cloned homogeneous slices because candidate breadth
is currently bounded to one worker's ready-ticket snapshot and the scorer API
stays simple. Mention revisiting if candidate breadth expands.

- [ ] **Step 2: Run scheduler tests**

Run: `cargo test -p voom-scheduler`

Expected: PASS.

- [ ] **Step 3: Run remote acquire tests**

Run: `cargo test -p voom-control-plane remote_acquire`

Expected: PASS.

- [ ] **Step 4: Run lint**

Run: `just lint`

Expected: PASS.

- [ ] **Step 5: Run adversarial and simplification reviews**

Confirm the comment names a real bound, does not hide clone cost, and does not
introduce a speculative abstraction.
