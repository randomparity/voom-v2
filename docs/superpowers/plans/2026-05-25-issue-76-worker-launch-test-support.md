# Issue 76 Worker Launch Test Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate duplicated integration-test worker launch/setup code into shared dev-only test support.

**Architecture:** Add `voom-test-support` as a workspace crate used only from dev-dependencies. Its worker helper owns process spawn, `BOUND addr` parsing, worker registration, capability/grant setup, binary resolution, and shutdown cleanup.

**Tech Stack:** Rust integration tests, tokio/sqlx/control-plane APIs, Cargo workspace dev-dependencies.

---

### Task 1: Create Test Support Crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/voom-test-support/Cargo.toml`
- Create: `crates/voom-test-support/src/lib.rs`
- Create: `crates/voom-test-support/src/worker.rs`

- [x] Add `crates/voom-test-support` to workspace members and `[workspace.dependencies]`.
- [x] Implement binary resolution helpers.
- [x] Implement `TestWorkerConfig` and `TestWorkerLaunch`.
- [x] Run `cargo test -p voom-test-support`.

### Task 2: Migrate CLI Test Helpers

**Files:**
- Modify: `crates/voom-cli/Cargo.toml`
- Modify: `crates/voom-cli/tests/support/voom_cli.rs`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`

- [x] Add `voom-test-support` as a `voom-cli` dev-dependency.
- [x] Replace `TranscodeWorkerLaunch` internals in `support/voom_cli.rs` with the shared helper.
- [x] Replace `RemuxProviderLaunch` in `compliance_envelope.rs` with the shared helper.
- [x] Run `cargo test -p voom-cli --test compliance_envelope execute_outputs_report_and_execution_summary`.

### Task 3: Migrate Control-Plane Integration Helpers

**Files:**
- Modify: `crates/voom-control-plane/Cargo.toml`
- Modify: `crates/voom-control-plane/tests/video_transcode_flow.rs`
- Modify: `crates/voom-control-plane/tests/compliance_execute.rs`

- [x] Add `voom-test-support` as a `voom-control-plane` dev-dependency.
- [x] Replace local `TranscodeWorkerLaunch`, `read_bound_addr`, `build_worker_binary`, and `worker_binary`.
- [x] Replace adjacent local `RemuxProviderLaunch` in `compliance_execute.rs` found during simplification review.
- [x] Run `cargo test -p voom-control-plane --test video_transcode_flow`.
- [x] Run `cargo test -p voom-control-plane --test compliance_execute`.

### Task 4: Verify and Review

**Files:**
- Validate all changed files.

- [x] Run `cargo test -p voom-cli --test chaos_librarian_e2e -- chaos_run_scan_root_follows_materialized_location_prefix`.
- [x] Run `just fmt-check`.
- [x] Run `just lint`.
- [x] Run `just test`.
- [x] Run `just ci`.
- [x] Run adversarial code review and address material findings.
- [x] Run simplification review and address the most relevant recommendations.
- [ ] Commit the branch.
