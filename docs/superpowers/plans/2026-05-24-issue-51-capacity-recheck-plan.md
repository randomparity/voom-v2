# Issue 51 Capacity Recheck Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Document the selected remote-acquire capacity recheck as an intentional transaction-local guard.

**Architecture:** No behavior changes. Add one focused comment at the selected-path worker/node capacity recheck boundary in `remote_execution.rs`.

**Tech Stack:** Rust, existing remote-acquire tests, clippy.

---

### Task 1: Document Selected-Path Capacity Recheck

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`

- [ ] **Step 1: Add boundary comment**

Add a short comment immediately before the worker capacity re-read explaining
that scoring uses advisory candidate facts, while the selected path rechecks
transaction-local capacity immediately before lease creation so capacity-full
decisions record the current observed active/limit values.

- [ ] **Step 2: Run remote acquire tests**

Run: `cargo test -p voom-control-plane remote_acquire`

Expected: PASS.

- [ ] **Step 3: Run focused node limit test**

Run:
`cargo test -p voom-control-plane node_default_limit_blocks_second_concurrent_remote_acquire`

Expected: PASS.

- [ ] **Step 4: Run lint**

Run: `just lint`

Expected: PASS.

- [ ] **Step 5: Run adversarial and simplification reviews**

Confirm the comment is precise, non-speculative, and does not invite removing
the correctness recheck later.
