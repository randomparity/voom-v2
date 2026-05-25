# Sprint 12 Simplification Review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the safe simplification recommendations from the `main..HEAD` review and track valid deferred work.

**Architecture:** Keep the workflow executor on the new control-plane transcode path only. Avoid broad cross-crate contract changes in this branch closeout unless they are mechanically safe.

**Tech Stack:** Rust workspace, tokio, sqlx, `just` commands, GitHub CLI.

---

### Task 1: Delete Dead Legacy Transcode Executor Path

**Files:**
- Modify: `crates/voom-control-plane/src/workflow/executor.rs`

- [x] **Step 1: Remove the unreachable branch after `dispatch_control_plane_transcode`**

Delete the second `if validate_transcode_result { ... }` block in `dispatch_ticket_inner`, because the first branch returns immediately for the same condition.

- [x] **Step 2: Remove dead plumbing**

Remove `transcode_staging_path`, the `validate_transcode_result` and `transcode_staging_path` parameters from `consume_dispatch_stream` and `handle_terminal_frame`, and the terminal-frame mutation/validation that only served the unreachable branch.

- [x] **Step 3: Remove dead helper stack and imports**

Remove `TranscodeWorkerPayload`, `transcode_worker_payload`, `create_transcode_staging_parent`, `reject_existing_symlink_components`, `reject_symlink_dir`, `select_transcode_source_location`, `require_transcode_local_location`, `transcode_output_file_name`, and `required_str` if no remaining callers exist. Remove unused transcode protocol imports.

- [x] **Step 4: Verify executor tests**

Run: `cargo test -p voom-control-plane workflow::executor`
Expected: all executor tests pass.

### Task 2: Remove Planner Codec Allocation

**Files:**
- Modify: `crates/voom-plan/src/planner.rs`

- [x] **Step 1: Replace lowercase allocation**

Change `is_hevc_codec` to:

```rust
fn is_hevc_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265")
}
```

- [x] **Step 2: Verify planner tests**

Run: `cargo test -p voom-plan planner`
Expected: planner tests pass.

### Task 3: File Deferred Follow-Up Issues

**Files:**
- No code changes.

- [x] **Step 1: File contract-centralization issue**

Use `gh issue create` for the repeated transcode output/profile contract helpers across worker protocol, control plane, worker, and planner. Include the review evidence and note public semantics/test risk.

Filed: https://github.com/randomparity/voom-v2/issues/62

- [x] **Step 2: File verification-boundary issue**

Use `gh issue create` for evaluating whether `require_output_file_matches_result` should be removed in favor of verification/commit checks. Include the failure timing and audit-shape risk.

Filed: https://github.com/randomparity/voom-v2/issues/63

- [x] **Step 3: File compliance reporting simplification issue if not implemented**

Use `gh issue create` for consolidating repeated `ComplianceExecuteData` partial construction and/or deriving ticket operation without parsing payload. Include snapshot and output-contract risks.

Filed:
- https://github.com/randomparity/voom-v2/issues/64
- https://github.com/randomparity/voom-v2/issues/65

### Task 4: Final Verification and Commit

**Files:**
- Modified source files and this plan.

- [x] **Step 1: Run formatting**

Run: `just fmt`
Expected: command exits 0.

- [x] **Step 2: Run focused tests**

Run: `cargo test -p voom-control-plane workflow::executor`
Run: `cargo test -p voom-plan planner`
Expected: both commands exit 0.

- [x] **Step 3: Run broader check if time permits**

Run: `just ci`
Expected: command exits 0, or report the exact failure.

- [x] **Step 4: Commit**

Run:

```bash
git add docs/superpowers/plans/2026-05-25-sprint-12-simplification-review.md crates/voom-control-plane/src/workflow/executor.rs crates/voom-plan/src/planner.rs
git commit -m "refactor: simplify sprint 12 transcode workflow"
```
