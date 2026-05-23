# Simplify Compliance Execution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the concrete simplification-review recommendations for the Sprint 6 compliance branch without changing public report or CLI envelope shapes.

**Architecture:** Keep production compliance execution focused on orchestration: it should apply one generated report, consume already registered worker runtimes, and reuse workflow helpers for operation names. Keep fake providers and synthetic runtime clients in tests. Avoid the broader compliance-operation classifier consolidation in this patch because it changes a public modeling boundary.

**Tech Stack:** Rust workspace, tokio, sqlx, axum-adjacent control-plane services, insta CLI snapshots, `just` commands.

---

### Task 1: Share Generated Reports Between Execute and Apply

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Test: `crates/voom-control-plane/src/cases/compliance_test.rs`
- Test: `crates/voom-cli/tests/compliance_envelope.rs`

- [ ] **Step 1: Extract the apply body behind a generated-report helper**

Add a private helper on `ControlPlane`:

```rust
async fn apply_generated_compliance_report(
    &self,
    report_data: &ComplianceReportData,
    policy_version_id: PolicyVersionId,
) -> Result<ComplianceApplyData, VoomError>
```

Move the existing issue upsert, stale resolve, and event emission body into that helper. Use `&report_data.report` throughout the helper and return `ComplianceApplyData { report: report_data.report.clone(), issues: summary }`.

- [ ] **Step 2: Keep public apply behavior unchanged**

Change `apply_compliance_report` to:

```rust
let report_data = self.generate_compliance_report(policy_version_id, input_set_id).await?;
self.apply_generated_compliance_report(&report_data, policy_version_id).await
```

- [ ] **Step 3: Make execute reuse the generated report**

Change `execute_compliance_policy` and the test runtime-registry helper so they generate once, apply the same `ComplianceReportData`, and bridge from the same `plan` and `report`.

- [ ] **Step 4: Verify targeted behavior**

Run:

```bash
cargo test -p voom-control-plane compliance_execute
cargo test -p voom-cli --test compliance_envelope
```

Expected: all tests pass, or only intentional insta snapshot updates appear.

### Task 2: Keep Synthetic Worker Wiring Out Of Production Execution

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Modify: `crates/voom-control-plane/src/cases/compliance_test.rs`
- Modify: `crates/voom-control-plane/tests/compliance_execute.rs`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`

- [ ] **Step 1: Delete production synthetic runtime code**

Remove production-only fake runtime code from `compliance.rs`: `SyntheticPolicyClient`, synthetic worker registration, synthetic capability/grant writes, and any async pipe code used only by that fake.

- [ ] **Step 2: Load registered remux runtimes from worker capabilities**

Keep `policy_runtime_registry` as the production path. It should query registered/active workers with `remux` capability, parse `extra.endpoint` and `extra.secret`, and register `HttpClient` runtimes.

- [ ] **Step 3: Seed fake providers in tests**

Tests that need successful execute must explicitly register a worker and fake provider. Tests that need workflow-submission failure should pass an empty `WorkerRuntimeRegistry` through a test-only helper.

- [ ] **Step 4: Verify mutation boundaries**

Run:

```bash
cargo test -p voom-control-plane execute_mutates_issues_issue_events_and_workflow_tables_only
cargo test -p voom-control-plane compliance_execute_runs_set_container_as_remux_through_workflow_executor
```

Expected: execution mutates issue/event/workflow tables but not worker registration tables.

### Task 3: Reuse The Workflow Operation Name Helper

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Test: `crates/voom-control-plane/src/workflow/ticket_payload_test.rs`
- Test: `crates/voom-control-plane/src/cases/compliance_test.rs`

- [ ] **Step 1: Import the helper**

Use:

```rust
use crate::workflow::ticket_payload::operation_name;
```

- [ ] **Step 2: Delete the duplicate local match**

Remove the local `fn operation_name(operation: OperationKind) -> &'static str` from `compliance.rs`.

- [ ] **Step 3: Verify serialized operation names still match**

Run:

```bash
cargo test -p voom-control-plane ticket_payload compliance_execute
```

Expected: ticket payload and compliance execution tests pass.

### Task 4: Remove The Extra Issue Conflict Lookup

**Files:**
- Modify: `crates/voom-store/src/repo/issues.rs`
- Test: `crates/voom-store/src/repo/issues_test.rs`

- [ ] **Step 1: Collapse conflict lookup**

In `upsert_policy_noncompliant_in_tx`, after insert failure, call `select_issue_detail` once. If it returns `None`, return `VoomError::Database(format!("issues insert: {err}"))`; otherwise compare/update from that detail.

- [ ] **Step 2: Delete the unused helper**

Remove `select_issue_for_update` once no callers remain.

- [ ] **Step 3: Verify issue repository behavior**

Run:

```bash
cargo test -p voom-store repo::issues
```

Expected: created, unchanged, updated, and resolved issue paths pass.

### Task 5: Final Verification And Commit

**Files:**
- Modify: all files touched above

- [ ] **Step 1: Format**

Run:

```bash
just fmt
```

Expected: formatting succeeds.

- [ ] **Step 2: Run focused tests**

Run:

```bash
cargo test -p voom-store repo::issues
cargo test -p voom-control-plane compliance_execute
cargo test -p voom-cli --test compliance_envelope
```

Expected: all focused tests pass.

- [ ] **Step 3: Commit**

Run:

```bash
git add docs/superpowers/plans/2026-05-23-simplify-compliance-execution.md crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/compliance_test.rs crates/voom-control-plane/tests/compliance_execute.rs crates/voom-cli/tests/compliance_envelope.rs crates/voom-store/src/repo/issues.rs
git commit -m "refactor: simplify compliance execution"
```

Expected: commit succeeds with only simplification-review follow-up changes staged.
