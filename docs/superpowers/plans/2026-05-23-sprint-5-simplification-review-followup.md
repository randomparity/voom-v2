# Sprint 5 Simplification Review Followup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Address the highest-confidence simplification findings from the `main..HEAD` branch review without changing planner behavior or CLI output contracts.

**Architecture:** Keep behavior-preserving simplifications inside existing crate boundaries. Prefer local planner traversal cleanup, central fixture label parsing in `voom-policy`, and allocation cleanup in control-plane. Defer repository projections and hash-preimage changes because they affect larger ownership boundaries or public plan identifiers.

**Tech Stack:** Rust workspace, tokio, serde, clap, insta, `just` commands.

---

## Review Findings And Decisions

- Accepted: consolidate duplicated `CompiledOperation` traversal in `crates/voom-plan/src/planner.rs`.
- Accepted: replace planner `Option<bool>` condition results with an explicit enum.
- Accepted: reuse existing blocked-operation helper in nested conditional/rules branches.
- Accepted: move fixture label parsing from CLI into `voom-policy::FixtureName`.
- Accepted: remove the unnecessary clone before deserializing stored compiled policy JSON.
- Deferred: planning-specific repository projection for durable input sets. It is a valid future simplification, but it changes the store API and narrows data loaded by `plan show`.
- Deferred: single hash preimage for `plan_id` and `plan_hash`. It risks changing public IDs and snapshots.
- Deferred: CLI config helper. It is low-value relative to the current plan and touches older command dispatch paths outside Sprint 5 planning.

## Success Criteria

- Planner output remains behavior-compatible for existing tests and snapshots.
- Fixture label parsing has one source of truth in `voom-policy`.
- Control-plane no longer clones `version.compiled_json` before `serde_json::from_value`.
- Relevant focused tests pass.
- `just fmt-check`, `just lint`, and `just test` pass before committing.

### Task 1: Centralize Fixture Labels

**Files:**
- Modify: `crates/voom-policy/src/fixtures.rs`
- Modify: `crates/voom-policy/src/fixtures_test.rs`
- Modify: `crates/voom-cli/src/commands/plan.rs`
- Modify: `crates/voom-cli/src/commands/plan_test.rs`

- [x] **Step 1: Add policy-level fixture parsing tests**

Add tests that assert `FixtureName::as_str()` and `FromStr` round-trip both public labels and reject unknown labels.

- [x] **Step 2: Run focused policy fixture tests**

Run: `cargo test -p voom-policy fixtures`

- [x] **Step 3: Implement `FixtureName::as_str` and `FromStr`**

Keep the error type simple and stable enough for CLI mapping.

- [x] **Step 4: Replace CLI-local parser**

Make `voom-cli` use `input_fixture.parse::<FixtureName>()` and keep the existing `BAD_ARGS` message.

- [x] **Step 5: Run focused CLI plan command tests**

Run: `cargo test -p voom-cli fixture_name`

### Task 2: Simplify Planner Traversal

**Files:**
- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`

- [x] **Step 1: Add focused characterization tests**

Cover condition behavior for missing fields and unsupported comparisons so the refactor protects the meaning of `Unknown`.

- [x] **Step 2: Run focused planner tests**

Run: `cargo test -p voom-plan planner`

- [x] **Step 3: Introduce explicit condition result enum**

Replace `Option<bool>` return values with `ConditionEval::{Matched, NotMatched, Unknown}` and update call sites.

- [x] **Step 4: Collapse duplicated operation dispatch**

Use one snapshot-scoped operation walker for set-container, conditional, rules, and unsupported operations. Delete phase-level wrapper dispatch methods that only loop snapshots.

- [x] **Step 5: Reuse blocked-operation helper**

Replace repeated blocked loops with the existing slice helper.

- [x] **Step 6: Run focused planner tests again**

Run: `cargo test -p voom-plan planner`

### Task 3: Remove Owned JSON Clone

**Files:**
- Modify: `crates/voom-control-plane/src/cases/plans.rs`

- [x] **Step 1: Move stored compiled JSON into deserialization**

Change `serde_json::from_value(version.compiled_json.clone())` to consume the owned value.

- [x] **Step 2: Run focused control-plane planning tests**

Run: `cargo test -p voom-control-plane plans`

### Task 4: Workspace Verification And Commit

**Files:**
- Modified files from Tasks 1-3
- Add: this plan document

- [x] **Step 1: Format**

Run: `just fmt`

- [x] **Step 2: Verify formatting, lint, and tests**

Run: `just fmt-check`
Run: `just lint`
Run: `just test`

- [x] **Step 3: Review final diff**

Run: `git diff --check`
Run: `git diff --stat`

- [x] **Step 4: Commit**

Commit message: `refactor: simplify sprint 5 planning followups`
