# Issue 50 Typed Scheduler Reasons Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace raw scorer reason strings with a scheduler-owned typed reason API while keeping durable strings stable.

**Architecture:** `voom-scheduler` owns `ScoreReasonCode`. The scorer emits typed reasons and serializes them to JSON only at the explanation boundary. `voom-control-plane` exhaustively maps `ScoreReasonCode` to store `SchedulerReasonCode`.

**Tech Stack:** Rust enums, serde_json explanation output, existing scheduler/control-plane/store tests.

---

### Task 1: Add Scheduler Reason Type

**Files:**
- Modify: `crates/voom-scheduler/src/lib.rs`
- Modify: `crates/voom-scheduler/src/lib_test.rs`

- [ ] **Step 1: Update tests for typed reason fields**

Change scheduler unit tests that compare `ScoreDecision.reason_code` to strings
so they compare to `ScoreReasonCode` variants, while leaving explanation JSON
reason array assertions as string checks.

- [ ] **Step 2: Run scheduler tests to confirm compile failures**

Run: `cargo test -p voom-scheduler`

Expected: FAIL because `ScoreReasonCode` does not exist and
`ScoreDecision.reason_code` is still a string.

- [ ] **Step 3: Implement `ScoreReasonCode`**

Add a public enum with variants for all current scorer reasons:
`Selected`, `NoReadyTicket`, `MissingCapability`, `MissingGrant`,
`OperationDenied`, `WorkerNotExecutable`, `NodeNotExecutable`,
`HeartbeatExpired`, `UnsupportedArtifactAccess`, `WorkerCapacityFull`,
`NodeCapacityFull`, and `NoEligibleCandidate`.

Add `as_str()` and `priority()` methods. Change scorer internals so hard-gate
reason lists are `Vec<ScoreReasonCode>`, JSON explanation reasons serialize via
`as_str()`, and `ScoreDecision.reason_code` stores `ScoreReasonCode`.

- [ ] **Step 4: Run scheduler tests**

Run: `cargo test -p voom-scheduler`

Expected: PASS.

### Task 2: Update Control-Plane Integration

**Files:**
- Modify: `crates/voom-control-plane/src/cases/remote_execution.rs`
- Modify: `crates/voom-control-plane/src/cases/remote_execution_test.rs`

- [ ] **Step 1: Update tests for typed scorer reasons**

Change direct scorer assertions in remote acquire tests to compare
`ScoreDecision.reason_code` against `ScoreReasonCode` variants. Keep durable
decision assertions as string checks through store `SchedulerReasonCode`.

- [ ] **Step 2: Run remote acquire tests to confirm compile failures**

Run: `cargo test -p voom-control-plane remote_acquire`

Expected: FAIL until control-plane conversion accepts typed scheduler reasons.

- [ ] **Step 3: Replace string parser with exhaustive mapping**

Import `voom_scheduler::ScoreReasonCode` and alias the store enum as
`StoreSchedulerReasonCode`. Change `decision_from_score`, `suppression_key`, and
summaries to use `score.reason_code.as_str()` where public strings are required.
Replace `scheduler_reason(&str)` with
`scheduler_reason(ScoreReasonCode) -> StoreSchedulerReasonCode` using an
exhaustive match.

Replace duplicate control-plane reason-priority helpers with
`ScoreReasonCode::parse(reason).map(|reason| (reason.priority(), reason))` for
aggregating explanation JSON.

- [ ] **Step 4: Run remote acquire tests**

Run: `cargo test -p voom-control-plane remote_acquire`

Expected: PASS.

### Task 3: Verify Store Compatibility and Lint

**Files:**
- Review: `crates/voom-store/src/repo/scheduler_decisions.rs`

- [ ] **Step 1: Run store scheduler decision tests**

Run: `cargo test -p voom-store scheduler_decisions`

Expected: PASS, proving durable reason strings still parse.

- [ ] **Step 2: Run lint**

Run: `just lint`

Expected: PASS.

- [ ] **Step 3: Run adversarial code review**

Review for public JSON string changes, non-exhaustive reason conversion, and
layering violations between scheduler, control-plane, and store.

- [ ] **Step 4: Run simplification review**

Review for duplicated reason priority logic and unnecessary parser/conversion
helpers.
