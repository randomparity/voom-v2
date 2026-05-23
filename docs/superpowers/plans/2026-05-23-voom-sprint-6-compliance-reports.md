# VOOM Sprint 6 Compliance Reports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Sprint 6 compliance reports, durable `policy_noncompliant` issue application, and narrow synthetic execution for Sprint 5-supported plan nodes.

**Architecture:** `voom-plan` owns pure deterministic report types and report generation from `ExecutionPlan`; it does not read durable state or interpret policy text. `voom-store` owns issue persistence and the `issues.dedupe_key` migration. `voom-control-plane` composes accepted-policy validation, planning, reporting, issue application, and policy-derived workflow execution, while `voom-cli` exposes the public single-envelope commands.

**Tech Stack:** Rust 2024, serde/serde_json canonical JSON, blake3 hashing, sqlx SQLite transactions, tokio integration tests, insta snapshots, existing sibling unit-test layout, `just ci`.

---

## Success Criteria

- `voom-plan` can generate deterministic `ComplianceReport` values from Sprint 5 `ExecutionPlan` values without depending on store, control-plane, CLI, scheduler, or worker crates.
- Report statuses map exactly as specified: no nodes -> `not_applicable`; `no_op` only -> `compliant`; planned only -> `noncompliant`; blocked only -> `blocked`; planned plus blocked -> `mixed`.
- Report ids and hashes are stable across identical plans; `report_hash` excludes itself and invocation metadata, and `report_id` uses its own stable preimage instead of hashing the hash.
- Golden report fixtures cover compliant, noncompliant, blocked, and mixed synthetic cases.
- Durable compliance use cases reject non-current accepted policy versions with `POLICY_VALIDATION_ERROR`; missing policy versions or input sets remain `NOT_FOUND`.
- `compliance report` is read-only for issues, events, jobs, tickets, leases, and artifacts.
- `compliance apply` mutates only issues and issue lifecycle events, is idempotent, deduplicates by the Sprint 6 key, and resolves only matching policy compliance issues.
- `compliance execute` applies issues first, then submits only supported planned `set_container` nodes as synthetic `Remux` operations through `WorkflowExecutor`.
- Issue create, update, and resolve changes append typed issue lifecycle events in the same transaction as the issue mutation.
- No-executable-work execute succeeds without creating a job; unsupported planned execution fails with `POLICY_EXECUTION_ERROR` after issue application is reported complete in the returned partial execute data.
- CLI commands emit exactly one JSON envelope on stdout with reviewed insta snapshots.
- `just ci` passes.

## File Map

- Modify: `crates/voom-core/src/error.rs`, `error_test.rs`: add `COMPLIANCE_REPORT_ERROR` and `POLICY_EXECUTION_ERROR`.
- Modify: `crates/voom-api/src/lib.rs`, `crates/voom-cli/src/commands/health.rs`: update exhaustive error-code handling.
- Modify: `crates/voom-plan/src/model.rs`, `planner.rs`, existing planner fixture tests: add structured observed-state exposure for plan nodes that already know observed facts.
- Modify: `crates/voom-plan/src/lib.rs`: export compliance modules.
- Create: `crates/voom-plan/src/compliance_model.rs`, `compliance_model_test.rs`: report, check, summary, provenance, diagnostic, eligibility, and status types.
- Create: `crates/voom-plan/src/compliance_hash.rs`, `compliance_hash_test.rs`: report canonical JSON, ids, hashes, and check ids.
- Create: `crates/voom-plan/src/compliance_report.rs`, `compliance_report_test.rs`: pure report generation from `ExecutionPlan`.
- Modify: `crates/voom-plan/src/fixtures.rs`, `fixtures_test.rs`: load golden compliance report fixtures.
- Create: `crates/voom-plan/fixtures/reports/container_metadata_compliant.json`, `container_metadata_noncompliant.json`, `container_metadata_blocked.json`, `container_metadata_mixed.json`.
- Create: `migrations/0008_issue_dedupe_key.sql`: nullable `issues.dedupe_key` and unique partial index.
- Create: `crates/voom-store/src/repo/issues.rs`, `issues_test.rs`: narrow policy issue repository.
- Modify: `crates/voom-store/src/repo/mod.rs`: export issue repository types.
- Modify: `crates/voom-store/src/schema_test.rs`, `tests/migration_inventory.rs`: include migration 0008 expectations.
- Modify: `crates/voom-events/src/kind.rs`, `payload.rs`, `kind_test.rs`, `payload_test.rs`: add `issue.opened`, `issue.updated`, and `issue.resolved`.
- Modify: `crates/voom-control-plane/src/lib.rs`: add `issues` repo field.
- Modify: `crates/voom-control-plane/src/cases/mod.rs`: expose compliance case module.
- Create: `crates/voom-control-plane/src/cases/compliance.rs`, `compliance_test.rs`: report/apply/execute use cases and mutation-count tests.
- Create: `crates/voom-control-plane/src/workflow/policy_bridge.rs`, `policy_bridge_test.rs`: map report/plan nodes to minimal `WorkflowPlan`.
- Modify: `crates/voom-control-plane/src/workflow/mod.rs`: export bridge.
- Create: `crates/voom-control-plane/tests/compliance_execute.rs`: process-backed synthetic execution integration tests.
- Modify: `crates/voom-cli/src/envelope.rs`, `envelope_test.rs`: add a data-bearing error envelope helper for partial execute failures.
- Modify: `crates/voom-cli/src/cli.rs`, `src/main.rs`, `src/commands/mod.rs`: add `compliance report/apply/execute`.
- Create: `crates/voom-cli/src/commands/compliance.rs`, `compliance_test.rs`: command data shapes and control-plane calls.
- Create: `crates/voom-cli/tests/compliance_envelope.rs`: CLI success and error snapshots.
- Create: `crates/voom-cli/tests/snapshots/compliance_envelope__*.snap`: reviewed snapshots.

## Task 1: Error Code Contract

**Files:**
- Modify: `crates/voom-core/src/error.rs`
- Modify: `crates/voom-core/src/error_test.rs`
- Modify: `crates/voom-api/src/lib.rs`
- Modify: `crates/voom-cli/src/commands/health.rs`

- [ ] **Step 1: Write failing tests**

Add to `crates/voom-core/src/error_test.rs`:

```rust
#[test]
fn compliance_report_error_has_stable_public_code() {
    let err = VoomError::ComplianceReport("deterministic serialization failed".to_owned());
    assert_eq!(err.code(), "COMPLIANCE_REPORT_ERROR");
    assert_eq!(err.error_code(), ErrorCode::ComplianceReportError);
}

#[test]
fn policy_execution_error_has_stable_public_code() {
    let err = VoomError::PolicyExecution("unsupported operation".to_owned());
    assert_eq!(err.code(), "POLICY_EXECUTION_ERROR");
    assert_eq!(err.error_code(), ErrorCode::PolicyExecutionError);
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-core compliance_report_error_has_stable_public_code policy_execution_error_has_stable_public_code`

Expected: compile failure naming missing `ComplianceReport`, `PolicyExecution`, `ComplianceReportError`, and `PolicyExecutionError`.

- [ ] **Step 3: Add variants**

In `ErrorCode`, add after `PlanGenerationError`:

```rust
/// A compliance report could not be generated or serialized deterministically.
ComplianceReportError,
/// Policy-derived planned work could not be bridged into executable workflow work.
PolicyExecutionError,
```

In `ErrorCode::as_str`, add:

```rust
Self::ComplianceReportError => "COMPLIANCE_REPORT_ERROR",
Self::PolicyExecutionError => "POLICY_EXECUTION_ERROR",
```

In `VoomError`, add after `PlanGeneration`:

```rust
#[error("compliance report error: {0}")]
ComplianceReport(String),
#[error("policy execution error: {0}")]
PolicyExecution(String),
```

In `VoomError::error_code`, add:

```rust
Self::ComplianceReport(_) => ErrorCode::ComplianceReportError,
Self::PolicyExecution(_) => ErrorCode::PolicyExecutionError,
```

Add both new `ErrorCode` variants to existing exhaustive test vectors and matches. In health hints, return `None`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-core compliance_report_error_has_stable_public_code policy_execution_error_has_stable_public_code`

Expected: PASS.

Commit:

```bash
git add crates/voom-core/src/error.rs crates/voom-core/src/error_test.rs crates/voom-api/src/lib.rs crates/voom-cli/src/commands/health.rs
git commit -m "feat: add compliance error codes"
```

## Task 2: Pure Compliance Report Model

**Files:**
- Create: `crates/voom-plan/src/compliance_model.rs`
- Create: `crates/voom-plan/src/compliance_model_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write failing serialization/status tests**

Create `crates/voom-plan/src/compliance_model_test.rs`:

```rust
use serde_json::json;

use super::*;

#[test]
fn report_status_serializes_as_snake_case_contract() {
    assert_eq!(serde_json::to_value(ReportStatus::NotApplicable).unwrap(), json!("not_applicable"));
    assert_eq!(serde_json::to_value(CheckStatus::Noncompliant).unwrap(), json!("noncompliant"));
    assert_eq!(serde_json::to_value(IssueActionHint::CreateOrUpdatePlanned).unwrap(), json!("create_or_update_planned"));
    assert_eq!(serde_json::to_value(ExecutionEligibility::Supported).unwrap(), json!("supported"));
}

#[test]
fn compliance_report_serializes_expected_public_shape() {
    let report = ComplianceReport {
        schema_version: 1,
        report_id: "report_test".to_owned(),
        report_hash: "blake3:test".to_owned(),
        plan_id: "plan_test".to_owned(),
        plan_hash: "blake3:plan".to_owned(),
        policy: CompliancePolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: Some(voom_core::PolicyDocumentId(1)),
            version_id: Some(voom_core::PolicyVersionId(2)),
        },
        input: ComplianceInputIdentity {
            slug: Some("synthetic".to_owned()),
            source_label: None,
            input_set_id: Some(voom_core::PolicyInputSetId(3)),
            fixture_labels: vec!["synthetic".to_owned()],
        },
        summary: ComplianceSummary {
            status: ReportStatus::Noncompliant,
            total_check_count: 1,
            compliant_check_count: 0,
            noncompliant_check_count: 1,
            blocked_check_count: 0,
            executable_check_count: 1,
            operation_counts_by_kind: [("set_container".to_owned(), 1)].into_iter().collect(),
        },
        checks: vec![ComplianceCheck {
            check_id: "check_test".to_owned(),
            node_id: "node_test".to_owned(),
            target: voom_policy::TargetRef::Synthetic {
                key: "movie-a".to_owned(),
                kind: "media_work".to_owned(),
            },
            compliance_kind: "container".to_owned(),
            operation_kind: "set_container".to_owned(),
            desired_state: json!({"container": "mkv"}),
            observed_state: Some(json!({"container": "mp4"})),
            check_status: CheckStatus::Noncompliant,
            reason: "container mp4 will be changed to mkv".to_owned(),
            issue_action_hint: IssueActionHint::CreateOrUpdatePlanned,
            execution_eligibility: ExecutionEligibility::Supported,
        }],
        diagnostics: Vec::new(),
        provenance: ComplianceProvenance::default(),
    };

    let value = serde_json::to_value(report).unwrap();
    assert_eq!(value["summary"]["status"], "noncompliant");
    assert_eq!(value["checks"][0]["compliance_kind"], "container");
    assert_eq!(value["checks"][0]["observed_state"]["container"], "mp4");
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-plan compliance_report_serializes_expected_public_shape`

Expected: compile failure naming missing compliance model types.

- [ ] **Step 3: Implement model types**

Create `crates/voom-plan/src/compliance_model.rs` with:

```rust
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Compliant,
    Noncompliant,
    Blocked,
    Mixed,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Compliant,
    Noncompliant,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueActionHint {
    None,
    CreateOrUpdatePlanned,
    CreateOrUpdateOpen,
    ResolveMatching,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEligibility {
    Supported,
    NoOp,
    Blocked,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceReport {
    pub schema_version: u32,
    pub report_id: String,
    pub report_hash: String,
    pub plan_id: String,
    pub plan_hash: String,
    pub policy: CompliancePolicyIdentity,
    pub input: ComplianceInputIdentity,
    pub summary: ComplianceSummary,
    pub checks: Vec<ComplianceCheck>,
    pub diagnostics: Vec<ComplianceDiagnostic>,
    pub provenance: ComplianceProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompliancePolicyIdentity {
    pub slug: String,
    pub source_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<voom_core::PolicyDocumentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<voom_core::PolicyVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceInputIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<voom_core::PolicyInputSetId>,
    pub fixture_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceSummary {
    pub status: ReportStatus,
    pub total_check_count: u32,
    pub compliant_check_count: u32,
    pub noncompliant_check_count: u32,
    pub blocked_check_count: u32,
    pub executable_check_count: u32,
    pub operation_counts_by_kind: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceCheck {
    pub check_id: String,
    pub node_id: String,
    pub target: crate::TargetRef,
    pub compliance_kind: String,
    pub operation_kind: String,
    pub desired_state: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<serde_json::Value>,
    pub check_status: CheckStatus,
    pub reason: String,
    pub issue_action_hint: IssueActionHint,
    pub execution_eligibility: ExecutionEligibility,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplianceDiagnosticCode {
    UnsupportedComplianceOperation,
    UnsupportedExecutionOperation,
    MissingDurablePolicyIdentity,
    MissingDurableInputIdentity,
    InvalidReportRequest,
    IssueApplicationConflict,
    DeterministicSerializationFailure,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceDiagnostic {
    pub severity: ComplianceDiagnosticSeverity,
    pub code: ComplianceDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<crate::TargetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ComplianceProvenance {
    pub reporter: String,
    pub format: String,
}

impl Default for ComplianceProvenance {
    fn default() -> Self {
        Self {
            reporter: "voom-plan".to_owned(),
            format: "sprint6-v1".to_owned(),
        }
    }
}
```

In `crates/voom-plan/src/lib.rs`, add:

```rust
pub mod compliance_model;

pub use compliance_model::{
    CheckStatus, ComplianceCheck, ComplianceDiagnostic, ComplianceDiagnosticCode,
    ComplianceDiagnosticSeverity, ComplianceInputIdentity, CompliancePolicyIdentity,
    ComplianceProvenance, ComplianceReport, ComplianceSummary, ExecutionEligibility,
    IssueActionHint, ReportStatus,
};
```

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-plan compliance_model`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/lib.rs crates/voom-plan/src/compliance_model.rs crates/voom-plan/src/compliance_model_test.rs
git commit -m "feat: add compliance report model"
```

## Task 3: Structured Observed State, Report Hashing, And Report Generation

**Files:**
- Modify: `crates/voom-plan/src/model.rs`
- Modify: `crates/voom-plan/src/model_test.rs`
- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`
- Modify: `crates/voom-plan/fixtures/plans/container_metadata_compliant.json`
- Modify: `crates/voom-plan/fixtures/plans/container_metadata_noncompliant.json`
- Create: `crates/voom-plan/src/compliance_hash.rs`
- Create: `crates/voom-plan/src/compliance_hash_test.rs`
- Create: `crates/voom-plan/src/compliance_report.rs`
- Create: `crates/voom-plan/src/compliance_report_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write failing structured observed-state tests**

Add to `crates/voom-plan/src/planner_test.rs`:

```rust
#[test]
fn set_container_plan_nodes_carry_structured_observed_container_when_known() {
    let plan = plan_noncompliant_container_fixture();
    let node = plan.nodes.iter().find(|node| node.operation_kind == "set_container").unwrap();

    assert_eq!(node.observed_state, Some(serde_json::json!({"container": "mp4"})));
}

#[test]
fn set_container_plan_nodes_leave_observed_state_absent_when_unknown() {
    let plan = plan_unknown_container_fixture();
    let node = plan.nodes.iter().find(|node| node.operation_kind == "set_container").unwrap();

    assert_eq!(node.status, NodeStatus::Blocked);
    assert_eq!(node.observed_state, None);
}
```

Run: `cargo test -p voom-plan set_container_plan_nodes_carry_structured_observed_container_when_known`

Expected: compile failure naming missing `PlanNode::observed_state`.

- [ ] **Step 2: Add observed state to plan nodes**

In `crates/voom-plan/src/model.rs`, add this optional field immediately after `operation_payload`:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub observed_state: Option<serde_json::Value>,
```

In `planner.rs`, extend `make_node` with an `observed_state: Option<serde_json::Value>` parameter and assign it into `PlanNode`.

For `expand_set_container_for_snapshot`, pass:

```rust
snapshot.container.as_ref().map(|container| serde_json::json!({ "container": container }))
```

For unsupported and insufficient-fact blocked nodes where the fact is unknown, pass `None`.

Update existing planner golden fixtures so known-container nodes include `observed_state` and unknown-container nodes omit it.

- [ ] **Step 3: Verify structured observed state**

Run: `cargo test -p voom-plan set_container_plan_nodes_carry_structured_observed_container_when_known set_container_plan_nodes_leave_observed_state_absent_when_unknown`

Expected: PASS.

- [ ] **Step 4: Write failing report generation tests**

Create `crates/voom-plan/src/compliance_report_test.rs` with helper plans built directly from `PlanNode` values. Add these tests with direct assertions on report summary and first-check fields:

```rust
no_op_node_maps_to_compliant_check_and_report
planned_node_maps_to_noncompliant_supported_check
blocked_node_maps_to_blocked_check
planned_plus_blocked_maps_to_mixed_report
empty_plan_maps_to_not_applicable_report
identical_plans_produce_identical_report_id_and_hash
```

Use this assertion pattern in the tests:

```rust
let report = generate_compliance_report(&plan).unwrap();
assert_eq!(report.summary.status, ReportStatus::Noncompliant);
assert_eq!(report.checks[0].check_status, CheckStatus::Noncompliant);
assert_eq!(report.checks[0].compliance_kind, "container");
assert_eq!(report.checks[0].execution_eligibility, ExecutionEligibility::Supported);
assert_eq!(report.checks[0].issue_action_hint, IssueActionHint::CreateOrUpdatePlanned);
assert_eq!(report.checks[0].desired_state, serde_json::json!({"container": "mkv"}));
assert_eq!(report.checks[0].observed_state, Some(serde_json::json!({"container": "mp4"})));
```

- [ ] **Step 5: Write failing hash tests**

Create `crates/voom-plan/src/compliance_hash_test.rs`:

```rust
use serde_json::json;

use super::*;

#[test]
fn report_hash_ignores_report_hash_field() {
    let mut left = json!({"report_id": "report_a", "report_hash": "blake3:left", "checks": []});
    let mut right = left.clone();
    right["report_hash"] = json!("blake3:right");

    assert_eq!(report_hash_from_value(&left).unwrap(), report_hash_from_value(&right).unwrap());

    left["checks"] = json!([{"check_id": "check_a"}]);
    assert_ne!(report_hash_from_value(&left).unwrap(), report_hash_from_value(&right).unwrap());
}

#[test]
fn report_id_uses_stable_preimage() {
    let id = report_id(&json!({"plan_id": "plan_a", "checks": ["check_a"]})).unwrap();
    assert!(id.starts_with("report_"));
    assert_eq!(id.len(), "report_".len() + 16);
}
```

- [ ] **Step 6: Run focused failure**

Run: `cargo test -p voom-plan compliance_report`

Expected: compile failure naming missing hash/report functions.

- [ ] **Step 7: Implement report hashing**

Create `crates/voom-plan/src/compliance_hash.rs` using the existing `canonical_json` style:

```rust
use serde_json::Value;

pub fn report_hash(report: &crate::ComplianceReport) -> Result<String, serde_json::Error> {
    let value = serde_json::to_value(report)?;
    report_hash_from_value(&value)
}

pub fn report_hash_from_value(value: &Value) -> Result<String, serde_json::Error> {
    let mut value = value.clone();
    strip_report_hash(&mut value);
    Ok(format!(
        "blake3:{}",
        blake3::hash(crate::hash::canonical_json(&value)?.as_bytes()).to_hex()
    ))
}

pub fn report_id(preimage: &Value) -> Result<String, serde_json::Error> {
    let hash = blake3::hash(crate::hash::canonical_json(preimage)?.as_bytes())
        .to_hex()
        .to_string();
    Ok(format!("report_{}", &hash[..16]))
}

#[must_use]
pub fn check_id(report_id_preimage: &str, node_id: &str, operation_kind: &str) -> String {
    let hash = blake3::hash(format!("{report_id_preimage}\n{node_id}\n{operation_kind}").as_bytes())
        .to_hex()
        .to_string();
    format!("check_{}", &hash[..16])
}

fn strip_report_hash(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("report_hash");
    }
}
```

- [ ] **Step 8: Implement report generation**

Create `crates/voom-plan/src/compliance_report.rs`:

```rust
use std::collections::BTreeMap;

use serde_json::json;

use crate::{
    CheckStatus, ComplianceCheck, ComplianceDiagnostic, ComplianceDiagnosticCode,
    ComplianceDiagnosticSeverity, ComplianceInputIdentity, CompliancePolicyIdentity,
    ComplianceProvenance, ComplianceReport, ComplianceSummary, ExecutionEligibility,
    ExecutionPlan, IssueActionHint, NodeStatus, PlanNode, ReportStatus,
};

#[derive(Debug)]
pub struct ComplianceReportError {
    pub diagnostic: ComplianceDiagnostic,
}

impl ComplianceReportError {
    #[must_use]
    pub fn into_voom_error(self) -> voom_core::VoomError {
        voom_core::VoomError::ComplianceReport(self.diagnostic.message)
    }
}

pub fn generate_compliance_report(plan: &ExecutionPlan) -> Result<ComplianceReport, ComplianceReportError> {
    let report_id_preimage = report_id_preimage(plan);
    let provisional_report_id = crate::compliance_hash::report_id(&report_id_preimage)
        .map_err(serialization_error)?;
    let checks = plan.nodes.iter().map(|node| check_from_node(&provisional_report_id, node)).collect();
    let diagnostics = compliance_diagnostics(plan, &checks);
    let summary = summarize_checks(&checks);

    let mut report = ComplianceReport {
        schema_version: 1,
        report_id: provisional_report_id,
        report_hash: String::new(),
        plan_id: plan.plan_id.clone(),
        plan_hash: plan.plan_hash.clone(),
        policy: CompliancePolicyIdentity {
            slug: plan.policy.slug.clone(),
            source_hash: plan.policy.source_hash.clone(),
            document_id: plan.policy.document_id,
            version_id: plan.policy.version_id,
        },
        input: ComplianceInputIdentity {
            slug: plan.input.slug.clone(),
            source_label: plan.input.source_label.clone(),
            input_set_id: plan.input.input_set_id,
            fixture_labels: plan.input.fixture_labels.clone(),
        },
        summary,
        checks,
        diagnostics,
        provenance: ComplianceProvenance::default(),
    };
    report.report_hash = crate::compliance_hash::report_hash(&report).map_err(serialization_error)?;
    Ok(report)
}
```

Add private helpers in the same file:

- `report_id_preimage(plan)` returns JSON with `schema_version`, `plan_id`, `plan_hash`, policy identity, input identity, and node ids/statuses.
- `check_from_node(report_id, node)` maps `NodeStatus::NoOp -> compliant`, `Planned -> noncompliant`, `Blocked -> blocked`.
- `compliance_kind(node)` returns `"container"` for `operation_kind == "set_container"` and `"unsupported"` otherwise.
- `desired_state` copies `node.operation_payload`.
- `observed_state` copies `node.observed_state` and never parses `status_reason` or other user-facing prose.
- issue hints are `ResolveMatching` for compliant, `CreateOrUpdatePlanned` for supported noncompliant, `CreateOrUpdateOpen` for blocked insufficient facts, and `None` for unsupported operations.
- execution eligibility is `Supported` only for `status = Planned` and `operation_kind = "set_container"`, `NoOp` for no-op, `Blocked` for blocked supported operations, and `Unsupported` for unsupported operations.
- `compliance_diagnostics` emits `UnsupportedComplianceOperation` for checks whose `compliance_kind` is `"unsupported"`.

In `crates/voom-plan/src/lib.rs`, export the module and functions.

- [ ] **Step 9: Verify and commit**

Run: `cargo test -p voom-plan compliance_report`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/model.rs crates/voom-plan/src/model_test.rs crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs crates/voom-plan/src/lib.rs crates/voom-plan/src/compliance_hash.rs crates/voom-plan/src/compliance_hash_test.rs crates/voom-plan/src/compliance_report.rs crates/voom-plan/src/compliance_report_test.rs crates/voom-plan/fixtures/plans
git commit -m "feat: generate compliance reports from plans"
```

## Task 4: Golden Compliance Fixtures

**Files:**
- Modify: `crates/voom-plan/src/fixtures.rs`
- Modify: `crates/voom-plan/src/fixtures_test.rs`
- Create: `crates/voom-plan/fixtures/reports/container_metadata_compliant.json`
- Create: `crates/voom-plan/fixtures/reports/container_metadata_noncompliant.json`
- Create: `crates/voom-plan/fixtures/reports/container_metadata_blocked.json`
- Create: `crates/voom-plan/fixtures/reports/container_metadata_mixed.json`

- [ ] **Step 1: Add failing fixture tests**

Add to `crates/voom-plan/src/fixtures_test.rs`:

```rust
#[test]
fn golden_compliance_reports_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
        "container_metadata_blocked",
        "container_metadata_mixed",
    ] {
        let value = load_golden_compliance_report(name).unwrap();
        serde_json::from_value::<crate::ComplianceReport>(value).unwrap();
    }
}
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-plan golden_compliance_reports_deserialize_through_public_type`

Expected: compile failure naming missing `load_golden_compliance_report`.

- [ ] **Step 3: Add fixture loader and fixtures**

Add `load_golden_compliance_report(name: &str)` to `fixtures.rs`, mirroring `load_golden_plan`, with `include_str!("../fixtures/reports/<name>.json")`.

Generate each JSON fixture from tests by calling `generate_plan` on policy/input fixtures or hand-built mixed/blocked plans, then copy the reviewed deterministic output into the four fixture files. Fixture JSON must include `report_id`, `report_hash`, `summary`, `checks`, `diagnostics`, and `provenance`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-plan golden_compliance`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/fixtures.rs crates/voom-plan/src/fixtures_test.rs crates/voom-plan/fixtures/reports
git commit -m "test: add golden compliance report fixtures"
```

## Task 5: Issue Dedupe Migration And Repository

**Files:**
- Create: `migrations/0008_issue_dedupe_key.sql`
- Modify: `crates/voom-events/src/kind.rs`
- Modify: `crates/voom-events/src/kind_test.rs`
- Modify: `crates/voom-events/src/payload.rs`
- Modify: `crates/voom-events/src/payload_test.rs`
- Create: `crates/voom-store/src/repo/issues.rs`
- Create: `crates/voom-store/src/repo/issues_test.rs`
- Modify: `crates/voom-store/src/repo/mod.rs`
- Modify: `crates/voom-store/src/schema_test.rs`
- Modify: `crates/voom-store/tests/migration_inventory.rs`

- [ ] **Step 1: Write failing migration/repository tests**

Create `crates/voom-store/src/repo/issues_test.rs` with tests:

```rust
issue_dedupe_key_column_is_nullable_and_unique_when_present
upsert_policy_issue_creates_then_updates_same_dedupe_key
resolve_matching_policy_issue_resolves_only_exact_dedupe_key
list_open_policy_issues_by_document_and_input_prefix_for_no_longer_emitted_resolution
```

Use direct SQL for the first test and `SqliteIssueRepo` for the latter two.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-store issues_test --all-features`

Expected: migration inventory failure and missing issue repo compile errors.

- [ ] **Step 3: Add issue lifecycle event tests**

Add to `crates/voom-events/src/kind_test.rs` expected round-trip coverage for:

```rust
EventKind::IssueOpened
EventKind::IssueUpdated
EventKind::IssueResolved
```

with wire strings:

```text
issue.opened
issue.updated
issue.resolved
```

Add to `crates/voom-events/src/payload_test.rs` one round-trip test per event variant. Each payload must carry:

```rust
pub struct IssueLifecyclePayload {
    pub issue_id: voom_core::IssueId,
    pub kind: String,
    pub status: String,
    pub dedupe_key: Option<String>,
    pub policy_version_id: Option<voom_core::PolicyVersionId>,
    pub report_id: Option<String>,
}
```

- [ ] **Step 4: Add issue lifecycle events**

In `EventKind`, add `IssueOpened`, `IssueUpdated`, and `IssueResolved`; update `as_str`, `from_str`, and all exhaustive tests.

In `payload.rs`, add `IssueLifecyclePayload` plus `Event::IssueOpened`, `Event::IssueUpdated`, and `Event::IssueResolved`; update `Event::kind`.

- [ ] **Step 5: Add migration**

Create `migrations/0008_issue_dedupe_key.sql`:

```sql
ALTER TABLE issues ADD COLUMN dedupe_key TEXT;

CREATE UNIQUE INDEX issues_dedupe_key_unique
    ON issues (dedupe_key)
    WHERE dedupe_key IS NOT NULL;
```

Update migration inventory/schema expectations to include version 8.

- [ ] **Step 6: Implement narrow issue repo**

Create `crates/voom-store/src/repo/issues.rs` with:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueDraft {
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub title: String,
    pub body: String,
    pub priority_reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueStatus {
    Open,
    Planned,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueRow {
    pub id: voom_core::IssueId,
    pub dedupe_key: String,
    pub status: PolicyIssueStatus,
    pub epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyIssueMutationKind {
    Created,
    Updated,
    Resolved,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyIssueMutation {
    pub kind: PolicyIssueMutationKind,
    pub row: PolicyIssueRow,
}

#[async_trait::async_trait]
pub trait IssueRepo: super::Repository {
    async fn upsert_policy_noncompliant_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        draft: PolicyIssueDraft,
        now: time::OffsetDateTime,
    ) -> Result<PolicyIssueMutation, voom_core::VoomError>;

    async fn resolve_policy_noncompliant_by_dedupe_key_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        dedupe_key: &str,
        title: &str,
        body: &str,
        now: time::OffsetDateTime,
    ) -> Result<Option<PolicyIssueMutation>, voom_core::VoomError>;

    async fn list_live_policy_noncompliant_by_dedupe_prefix_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        dedupe_prefix: &str,
    ) -> Result<Vec<PolicyIssueRow>, voom_core::VoomError>;
}
```

Implement `SqliteIssueRepo` with:

- `INSERT INTO issues (...) VALUES (...)`;
- inserts set `kind = 'policy_noncompliant'`, `severity = 'medium'`, `priority = 'normal'`, `priority_source = 'policy'`, `suppressed_until = NULL`, and `resolved_at = NULL` for open/planned rows;
- on unique conflict, `SELECT id, status, epoch, title, body FROM issues WHERE dedupe_key = ?`;
- return `Unchanged` without updating when status, title, body, and priority reason already match;
- `UPDATE issues SET status = ?, title = ?, body = ?, updated_at = ?, resolved_at = NULL, epoch = epoch + 1 WHERE id = ?`;
- resolve path updates only `kind = 'policy_noncompliant' AND dedupe_key = ? AND status IN ('open','planned')`.
- live-prefix listing uses `kind = 'policy_noncompliant' AND status IN ('open','planned') AND dedupe_key LIKE ? ESCAPE '\'`, with a caller-supplied escaped prefix ending in `%`.

Export repo types in `repo/mod.rs`.

- [ ] **Step 7: Verify and commit**

Run:

```bash
cargo test -p voom-events issue --all-features
cargo test -p voom-store issues_test --all-features
```

Expected: PASS.

Commit:

```bash
git add migrations/0008_issue_dedupe_key.sql crates/voom-events/src/kind.rs crates/voom-events/src/kind_test.rs crates/voom-events/src/payload.rs crates/voom-events/src/payload_test.rs crates/voom-store/src/repo/issues.rs crates/voom-store/src/repo/issues_test.rs crates/voom-store/src/repo/mod.rs crates/voom-store/src/schema_test.rs crates/voom-store/tests/migration_inventory.rs
git commit -m "feat: add policy issue dedupe repository"
```

## Task 6: Control-Plane Report And Apply Use Cases

**Files:**
- Modify: `crates/voom-control-plane/src/lib.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Create: `crates/voom-control-plane/src/cases/compliance.rs`
- Create: `crates/voom-control-plane/src/cases/compliance_test.rs`

- [ ] **Step 1: Write failing control-plane tests**

Create tests in `compliance_test.rs`:

```rust
compliance_report_is_read_only
compliance_report_rejects_stale_policy_version
compliance_apply_creates_planned_issue_for_noncompliant_check
compliance_apply_creates_open_issue_for_blocked_insufficient_facts
compliance_apply_is_idempotent_for_repeated_report
compliance_apply_resolves_matching_issue_after_compliance
compliance_apply_resolves_matching_issue_when_new_policy_no_longer_emits_check
compliance_apply_does_not_create_issue_for_unsupported_operation
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane compliance_ --all-features`

Expected: compile failure naming missing compliance use cases and issue repo field.

- [ ] **Step 3: Add use-case data types**

In `cases/compliance.rs`, add:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceReportData {
    pub plan: voom_plan::ExecutionPlan,
    pub report: voom_plan::ComplianceReport,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IssueApplicationSummary {
    pub created_count: u32,
    pub updated_count: u32,
    pub resolved_count: u32,
    pub skipped_count: u32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceApplyData {
    pub report: voom_plan::ComplianceReport,
    pub issues: IssueApplicationSummary,
}
```

- [ ] **Step 4: Verify current accepted version before planning**

Add helper:

```rust
async fn load_current_accepted_policy_and_input(
    cp: &crate::ControlPlane,
    policy_version_id: PolicyVersionId,
    input_set_id: PolicyInputSetId,
) -> Result<(voom_store::repo::policies::PolicyVersion, voom_policy::PolicyInputSetDraft), VoomError>
```

It must:

- `get_version(policy_version_id)` and return `NotFound` when absent;
- load the owning `policy_documents` row or equivalent summary;
- compare `document.current_accepted_version_id == Some(policy_version_id)`;
- return `VoomError::PolicyValidationError(format!("policy version {policy_version_id} is not the current accepted version"))` when stale;
- load input set and return `NotFound` when absent;
- deserialize `compiled_json` instead of recompiling source.

- [ ] **Step 5: Implement report/apply methods**

Add `ControlPlane` methods:

```rust
pub async fn generate_compliance_report(
    &self,
    policy_version_id: PolicyVersionId,
    input_set_id: PolicyInputSetId,
) -> Result<ComplianceReportData, VoomError>

pub async fn apply_compliance_report(
    &self,
    policy_version_id: PolicyVersionId,
    input_set_id: PolicyInputSetId,
) -> Result<ComplianceApplyData, VoomError>
```

`generate_compliance_report` calls the Sprint 5 planner, then `voom_plan::generate_compliance_report`.

`apply_compliance_report` begins one transaction, upserts actionable checks, resolves exact matching compliant checks, resolves previously live checks no longer emitted by the current accepted version, commits, and returns counts. Dedupe key preimage:

```text
policy_document_id + input_set_id + target_ref + compliance_kind + operation_kind
```

Use this searchable key format so no-longer-emitted checks can be found without target-wide scans:

```text
policy_noncompliant:v1:policy_document_id=<id>:input_set_id=<id>:check=<blake3 canonical JSON of target_ref + compliance_kind + operation_kind>
```

The prefix used for listing is:

```text
policy_noncompliant:v1:policy_document_id=<id>:input_set_id=<id>:
```

Use deterministic titles/bodies:

```text
Policy compliance: container for <target>
Policy version <id> requires {"container":"mkv"}; observed {"container":"mp4"}; status planned.
```

For every `PolicyIssueMutationKind::Created`, append `Event::IssueOpened` in the same transaction. For `Updated`, append `Event::IssueUpdated`. For `Resolved`, append `Event::IssueResolved`. For `Unchanged`, do not append an event and count the check as skipped.

Resolution sequence inside the same transaction:

1. Build the set of dedupe keys emitted by all actionable or compliant checks in the current report.
2. Upsert noncompliant and blocked actionable checks.
3. Resolve emitted compliant checks by exact dedupe key.
4. List live policy issues by the policy-document/input-set prefix above.
5. Resolve only listed live issues whose complete dedupe key is absent from the current emitted-key set.
6. Never resolve issues from a different policy document, input set, target, compliance kind, or operation kind.

- [ ] **Step 6: Verify and commit**

Run: `cargo test -p voom-control-plane compliance_ --all-features`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/lib.rs crates/voom-control-plane/src/cases/mod.rs crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/compliance_test.rs
git commit -m "feat: add compliance report and issue application use cases"
```

## Task 7: Policy Execution Bridge

**Files:**
- Create: `crates/voom-control-plane/src/workflow/policy_bridge.rs`
- Create: `crates/voom-control-plane/src/workflow/policy_bridge_test.rs`
- Modify: `crates/voom-control-plane/src/workflow/mod.rs`

- [ ] **Step 1: Write failing bridge tests**

Create `policy_bridge_test.rs`:

```rust
bridge_maps_only_planned_set_container_to_remux
bridge_returns_empty_summary_without_job_for_no_executable_nodes
bridge_rejects_planned_unsupported_operation_before_job_creation
```

Assert:

```rust
assert_eq!(workflow.nodes.len(), 1);
assert_eq!(workflow.nodes[0].id(), "policy-node_<source node id>");
assert_eq!(workflow.nodes[0].operation(), voom_worker_protocol::OperationKind::Remux);
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane policy_bridge --all-features`

Expected: compile failure naming missing bridge module.

- [ ] **Step 3: Implement bridge**

Create:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyExecutionPlan {
    pub workflow: Option<WorkflowPlan>,
    pub summary: PolicyExecutionSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PolicyExecutionSummary {
    pub plan_id: String,
    pub report_id: String,
    pub job_id: Option<voom_core::JobId>,
    pub submitted_node_count: u32,
    pub skipped_no_op_count: u32,
    pub blocked_count: u32,
    pub dispatch_count: u64,
    pub failure_count: u64,
    pub per_operation: std::collections::BTreeMap<String, u64>,
}

pub fn workflow_plan_from_compliance(
    plan: &voom_plan::ExecutionPlan,
    report: &voom_plan::ComplianceReport,
) -> Result<PolicyExecutionPlan, voom_core::VoomError>
```

Rules:

- `NodeStatus::Planned` and `operation_kind == "set_container"` maps to `OperationKind::Remux`.
- workflow id is `policy-<report_id>`.
- workflow node id is `policy-node_<node_id>`.
- no-op and blocked nodes are counted but not submitted.
- planned unsupported operation returns `VoomError::PolicyExecution("unsupported execution operation <kind>")`.
- empty executable set returns `workflow: None`.
- minimal `WorkflowPlan` has no dependencies, `FanOutPolicy { max_files: 1 }`, `ConcurrencyPolicy { max_in_flight_dispatches: 1 }`, and small deterministic timing.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-control-plane policy_bridge --all-features`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/workflow/mod.rs crates/voom-control-plane/src/workflow/policy_bridge.rs crates/voom-control-plane/src/workflow/policy_bridge_test.rs
git commit -m "feat: bridge compliance plans to synthetic workflow"
```

## Task 8: Execute Use Case

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance.rs`
- Modify: `crates/voom-control-plane/src/cases/compliance_test.rs`
- Create: `crates/voom-control-plane/tests/compliance_execute.rs`

- [ ] **Step 1: Write failing execute tests**

Add unit tests:

```rust
compliance_execute_no_executable_work_creates_no_job
compliance_execute_reports_issues_applied_when_workflow_submission_fails
```

Create integration test `compliance_execute.rs`:

```rust
compliance_execute_runs_set_container_as_remux_through_workflow_executor
```

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-control-plane compliance_execute --all-features`

Expected: compile failure naming missing execute method.

- [ ] **Step 3: Add execute data type and method**

In `cases/compliance.rs`, add:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct ComplianceExecuteData {
    pub report: voom_plan::ComplianceReport,
    pub issues: IssueApplicationSummary,
    pub execution: crate::workflow::policy_bridge::PolicyExecutionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_diagnostic: Option<voom_plan::ComplianceDiagnostic>,
}

#[derive(Debug)]
pub struct ComplianceExecuteError {
    pub source: VoomError,
    pub partial: Option<ComplianceExecuteData>,
}
```

Add:

```rust
pub async fn execute_compliance_policy(
    &self,
    policy_version_id: PolicyVersionId,
    input_set_id: PolicyInputSetId,
) -> Result<ComplianceExecuteData, ComplianceExecuteError>
```

Implementation sequence:

1. Generate plan/report.
2. Apply issues and commit them.
3. Build bridge workflow.
4. If bridge returns no workflow, return zero-submitted execution summary.
5. For failures before issue application completes, return `Err(ComplianceExecuteError { source, partial: None })`.
6. If bridge fails, return `Err(ComplianceExecuteError { source: VoomError::PolicyExecution(...), partial: Some(partial) })` where `partial` includes the report, completed issue application summary, a zero-job execution summary, and an `execution_diagnostic` with `UnsupportedExecutionOperation`. This path is covered through the bridge/helper boundary because the Sprint 5 durable planner currently emits unsupported operations as blocked, not planned.
7. If workflow exists, construct `WorkflowExecutor` with existing synthetic runtime registry pattern and call `submit_and_run`.
8. If workflow submission or execution fails after issue application, return `ComplianceExecuteError { source, partial: Some(partial) }` with the completed issue summary and the failing workflow summary copied into `partial.execution`.
9. Copy `job_id`, `dispatch_count`, `failure_count`, and per-operation success counts into `PolicyExecutionSummary`.

- [ ] **Step 4: Verify and commit**

Run: `cargo test -p voom-control-plane compliance_execute --all-features`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/src/cases/compliance.rs crates/voom-control-plane/src/cases/compliance_test.rs crates/voom-control-plane/tests/compliance_execute.rs
git commit -m "feat: execute supported compliance work"
```

## Task 9: CLI Compliance Commands

**Files:**
- Modify: `crates/voom-cli/src/envelope.rs`
- Modify: `crates/voom-cli/src/envelope_test.rs`
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/compliance.rs`
- Create: `crates/voom-cli/src/commands/compliance_test.rs`
- Create: `crates/voom-cli/tests/compliance_envelope.rs`
- Create: `crates/voom-cli/tests/snapshots/compliance_envelope__*.snap`

- [ ] **Step 1: Write failing command tests**

Create command tests for fixture-free argument parsing:

```rust
compliance_report_command_requires_policy_version_and_input_set
```

Create integration snapshots:

```rust
report_outputs_compliance_report_envelope
apply_outputs_report_and_issue_summary
execute_outputs_report_and_execution_summary
report_missing_input_set_uses_not_found
report_stale_policy_version_uses_policy_validation_error
execute_unsupported_operation_uses_policy_execution_error
```

`execute_unsupported_operation_uses_policy_execution_error` is a command/envelope snapshot built from a synthetic `ComplianceExecuteError` and the command's error-emission helper. Do not try to seed a durable policy fixture for this case: Sprint 5 currently emits unsupported operations as blocked nodes, so a repository-backed `voom compliance execute` invocation cannot produce a planned unsupported node without a test-only planner bypass.

- [ ] **Step 2: Run focused failure**

Run: `cargo test -p voom-cli compliance_envelope --all-features`

Expected: compile failure naming missing CLI command.

- [ ] **Step 3: Add data-bearing error envelope helper**

Add to `crates/voom-cli/src/envelope.rs`:

```rust
pub fn emit_err_with_data<T: Serialize>(
    command: &'static str,
    data: T,
    code: &'static str,
    message: String,
    hint: Option<String>,
    local: Option<Local>,
) -> io::Result<()> {
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        command,
        status: Status::Error,
        data: Some(data),
        local,
        warnings: Vec::new(),
        error: Some(ErrorBody { code, message, hint }),
    };
    write_json(&env)
}
```

Add an envelope unit test proving `status = "error"` can still include `data.report`, `data.issues`, and `data.execution_diagnostic`.

- [ ] **Step 4: Add CLI shape**

In `cli.rs`, add:

```rust
#[command(subcommand)]
Compliance(ComplianceCommand),
```

and:

```rust
#[derive(Subcommand, Debug)]
pub enum ComplianceCommand {
    Report {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
    Apply {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
    Execute {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
}
```

- [ ] **Step 5: Add command handlers**

Create `commands/compliance.rs` with:

```rust
pub async fn report(database_url: &str, local: Local, policy_version_id: u64, input_set_id: u64) -> io::Result<i32>
pub async fn apply(database_url: &str, local: Local, policy_version_id: u64, input_set_id: u64) -> io::Result<i32>
pub async fn execute(database_url: &str, local: Local, policy_version_id: u64, input_set_id: u64) -> io::Result<i32>
```

Each handler:

- opens `ControlPlane::open(database_url)`;
- calls the matching control-plane use case;
- emits `emit_ok("compliance", data, Some(local), Vec::new())`;
- emits `emit_err("compliance", err.code(), err.to_string(), None, Some(local))` and returns `2` on ordinary errors.

For `execute_compliance_policy` returning `ComplianceExecuteError`, emit:

```rust
if let Some(partial) = err.partial {
    emit_err_with_data(
        "compliance",
        partial,
        err.source.code(),
        err.source.to_string(),
        None,
        Some(local),
    )
} else {
    emit_err(
        "compliance",
        err.source.code(),
        err.source.to_string(),
        None,
        Some(local),
    )
}
```

and return exit code `2`. This preserves the stable `POLICY_EXECUTION_ERROR` code while still returning the report and completed issue application summary required by the Sprint 6 design.

Wire dispatch in `main.rs` using the same config/local pattern as `plan show`.

- [ ] **Step 6: Review snapshots and commit**

Run: `cargo test -p voom-cli compliance_envelope --all-features`

Expected: new insta snapshots.

Run: `cargo insta review`

Accept only intentional Sprint 6 snapshots.

Commit:

```bash
git add crates/voom-cli/src/envelope.rs crates/voom-cli/src/envelope_test.rs crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/compliance.rs crates/voom-cli/src/commands/compliance_test.rs crates/voom-cli/tests/compliance_envelope.rs crates/voom-cli/tests/snapshots
git commit -m "feat: add compliance CLI commands"
```

## Task 10: Mutation Boundaries And Full Verification

**Files:**
- Modify: `crates/voom-control-plane/src/cases/compliance_test.rs`
- Modify: `crates/voom-cli/tests/compliance_envelope.rs`
- Modify: `docs/superpowers/specs/2026-05-23-voom-sprint-6-design.md` only if implementation discoveries require closeout notes.

- [ ] **Step 1: Add explicit mutation-count tests**

In `compliance_test.rs`, add table-count helpers for:

```rust
["issues", "events", "jobs", "tickets", "leases", "artifacts"]
```

Add tests:

```rust
report_mutates_no_durable_work_or_issue_tables
apply_mutates_only_issues_and_issue_events
execute_mutates_issues_issue_events_and_workflow_tables_only
```

- [ ] **Step 2: Run targeted suites**

Run:

```bash
cargo test -p voom-plan compliance --all-features
cargo test -p voom-store issues_test --all-features
cargo test -p voom-control-plane compliance --all-features
cargo test -p voom-control-plane --test compliance_execute --all-features
cargo test -p voom-cli compliance_envelope --all-features
```

Expected: all PASS.

- [ ] **Step 3: Run layout and formatting**

Run:

```bash
just fmt
just check-test-layout
```

Expected: `fmt` changes only intended files; layout passes.

- [ ] **Step 4: Run full CI**

Run: `just ci`

Expected: PASS, with no skipped required checks.

- [ ] **Step 5: Commit verification hardening**

Commit:

```bash
git add crates/voom-control-plane/src/cases/compliance_test.rs crates/voom-cli/tests/compliance_envelope.rs docs/superpowers/specs/2026-05-23-voom-sprint-6-design.md
git commit -m "test: verify compliance mutation boundaries"
```

## Self-Review

- Spec coverage: Tasks cover pure reports, status mapping, ids/hashes, fixtures, durable issue dedupe, current accepted version validation, apply/resolve idempotence, execution bridge, workflow execution, CLI envelopes, mutation boundaries, and `just ci`.
- No source-only Sprint 6 compliance CLI is planned, matching the explicit deferral.
- Durable execution-plan storage is not introduced.
- The bridge maps only `set_container -> Remux`; no broad Sprint 2 default workflow is reused.
- The plan keeps `voom-plan` pure and moves all persistence/composition into store/control-plane.
- Implementation risk to watch: the durable CLI cannot naturally produce a planned unsupported node today because Sprint 5 emits unsupported operations as blocked; the plan therefore tests that stable `POLICY_EXECUTION_ERROR` envelope through a command/helper seam and keeps repository-backed execution tests on producible policy/input states.
