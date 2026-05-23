# VOOM Sprint 5 Plan DAG Dry-Run Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Sprint 5 pure execution-plan projection and plan-only CLI inspection commands described in `docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md`.

**Architecture:** `voom-plan` owns deterministic planning types, ids, hashes, diagnostics, and phase-dependency DAG generation from `CompiledPolicy` plus policy inputs. `voom-control-plane` composes source-only and durable planning without creating execution state. `voom-cli` exposes `voom plan dry-run` and `voom plan show` through the existing single-envelope stdout contract.

**Tech Stack:** Rust 2024, serde/serde_json deterministic projections, blake3 hashing, sqlx SQLite read paths, tokio integration tests, insta CLI snapshots, existing sibling unit-test layout, `just ci`.

---

## Success Criteria

- `voom-plan` has no dependency on `voom-store`, `voom-control-plane`, `voom-cli`, scheduler, artifact, or worker crates.
- Source-only planning works without `VOOM_DATABASE_URL`, without an initialized database, and without creating database files or directories.
- Durable planning reads accepted `policy_versions.compiled_json` and policy input rows, verifies stored identity, and does not insert/update/delete jobs, tickets, leases, events, issues, artifacts, policy versions, or input sets.
- `SetContainer { container: "mkv" }` emits one stable node per media snapshot: `planned` for known non-mkv, `no_op` for already mkv, `blocked` for unknown container.
- Track and tag operations emit deterministic blocked nodes with stable diagnostics; they are never silently skipped.
- Plan JSON is deterministic: stable `plan_id`, `plan_hash`, `node_id`, `edge_id`, sorted operation counts, and golden fixture output.
- CLI success and error paths emit exactly one JSON envelope on stdout.

## File Map

- Modify: `Cargo.toml`: add `voom-plan` to `[workspace.dependencies]`.
- Modify: `crates/voom-core/src/error.rs`, `error_test.rs`: add `PLAN_GENERATION_ERROR`.
- Modify: `crates/voom-api/src/lib.rs`, `crates/voom-cli/src/commands/health.rs`: include `PLAN_GENERATION_ERROR` in exhaustive public error-code mappings.
- Modify: `crates/voom-plan/Cargo.toml`, `src/lib.rs`: replace the reserved module with public planning modules and deps.
- Create: `crates/voom-plan/src/model.rs`, `model_test.rs`: serializable plan model, node/edge/status/scheduling/provenance types.
- Create: `crates/voom-plan/src/diagnostic.rs`, `diagnostic_test.rs`: planning diagnostics and stable codes.
- Create: `crates/voom-plan/src/hash.rs`, `hash_test.rs`: canonical JSON, `plan_hash`, `plan_id`, node ids, edge ids.
- Create: `crates/voom-plan/src/planner.rs`, `planner_test.rs`: pure planning request validation and operation expansion.
- Create: `crates/voom-plan/src/fixtures.rs`, `fixtures_test.rs`: golden fixture loading and schema round-trip tests.
- Create: `crates/voom-plan/fixtures/plans/container_metadata_compliant.json`.
- Create: `crates/voom-plan/fixtures/plans/container_metadata_noncompliant.json`.
- Modify: `crates/voom-control-plane/Cargo.toml`: depend on `voom-plan`.
- Create: `crates/voom-control-plane/src/cases/plans.rs`, `plans_test.rs`: source-only and durable planning use cases.
- Modify: `crates/voom-control-plane/src/cases/mod.rs`, `src/lib.rs`: expose plan case and retain repo access.
- Modify: `crates/voom-cli/Cargo.toml`: depend on `voom-policy` and `voom-plan` if needed by command data types.
- Modify: `crates/voom-cli/src/cli.rs`, `src/main.rs`, `src/commands/mod.rs`: add `plan` subcommands and dispatch.
- Create: `crates/voom-cli/src/commands/plan.rs`, `plan_test.rs`: command payloads and fixture parsing.
- Create: `crates/voom-cli/tests/plan_envelope.rs`: source-only and durable CLI snapshots/errors.
- Create: `crates/voom-cli/tests/snapshots/plan_envelope__*.snap`: reviewed JSON snapshots.
- Modify: `docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md`: add closeout notes only for intentional implementation discoveries.

## Task 1: Error Code Contract

**Files:**
- Modify: `crates/voom-core/src/error.rs`
- Modify: `crates/voom-core/src/error_test.rs`
- Modify: `crates/voom-api/src/lib.rs`
- Modify: `crates/voom-cli/src/commands/health.rs`

- [ ] **Step 1: Write failing error-code test**

Add to `crates/voom-core/src/error_test.rs`:

```rust
#[test]
fn plan_generation_error_has_stable_public_code() {
    let err = VoomError::PlanGeneration("planner rejected empty input set".to_owned());
    assert_eq!(err.code(), "PLAN_GENERATION_ERROR");
    assert_eq!(err.error_code(), ErrorCode::PlanGenerationError);
}
```

- [ ] **Step 2: Run focused test and verify failure**

Run: `cargo test -p voom-core plan_generation_error_has_stable_public_code`

Expected: compile failure naming missing `PlanGeneration` and `PlanGenerationError`.

- [ ] **Step 3: Add the typed code and error variant**

In `ErrorCode`, add after `PolicyValidationError`:

```rust
/// A compiled policy and policy input set could not be converted into an execution-plan projection.
PlanGenerationError,
```

In `ErrorCode::as_str`, add:

```rust
Self::PlanGenerationError => "PLAN_GENERATION_ERROR",
```

In `VoomError`, add after `PolicyValidationError`:

```rust
#[error("plan generation error: {0}")]
PlanGeneration(String),
```

In `VoomError::error_code`, add:

```rust
Self::PlanGeneration(_) => ErrorCode::PlanGenerationError,
```

In `every_error_code_has_a_wire_string`, add:

```rust
ErrorCode::PlanGenerationError,
```

- [ ] **Step 4: Update exhaustive ErrorCode matches**

The new `ErrorCode::PlanGenerationError` variant must be added to every existing exhaustive `ErrorCode` match before this task commits, otherwise later workspace builds fail outside `voom-core`.

Add it to:

- `crates/voom-api/src/lib.rs` API error mapping, grouped with policy validation and other request-domain errors;
- `crates/voom-cli/src/commands/health.rs` `voom_error_hint`, returning `None`.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p voom-core plan_generation_error_has_stable_public_code`

Expected: PASS.

Commit:

```bash
git add crates/voom-core/src/error.rs crates/voom-core/src/error_test.rs crates/voom-api/src/lib.rs crates/voom-cli/src/commands/health.rs
git commit -m "feat: add plan generation error code"
```

## Task 2: Planner Crate Skeleton And Public Model

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/voom-plan/Cargo.toml`
- Modify: `crates/voom-plan/src/lib.rs`
- Create: `crates/voom-plan/src/model.rs`
- Create: `crates/voom-plan/src/model_test.rs`
- Create: `crates/voom-plan/src/diagnostic.rs`
- Create: `crates/voom-plan/src/diagnostic_test.rs`

- [ ] **Step 1: Write failing model tests**

Create `crates/voom-plan/src/model_test.rs`:

```rust
use super::*;

#[test]
fn execution_plan_serializes_public_shape() {
    let plan = ExecutionPlan {
        schema_version: 1,
        plan_id: "plan_test".to_owned(),
        plan_hash: "blake3:test".to_owned(),
        policy: PolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "abc".to_owned(),
            document_id: None,
            version_id: None,
        },
        input: InputIdentity {
            slug: Some("synthetic-compliant-baseline".to_owned()),
            source_label: Some("synthetic_compliant_baseline".to_owned()),
            input_set_id: None,
            fixture_labels: vec!["synthetic_compliant_baseline".to_owned()],
        },
        generated_at: None,
        summary: PlanSummary::default(),
        nodes: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    };

    let json = serde_json::to_value(&plan).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["plan_id"], "plan_test");
    assert_eq!(json["plan_hash"], "blake3:test");
    assert_eq!(json["policy"]["slug"], "container-metadata");
    assert_eq!(json["nodes"], serde_json::json!([]));
    assert_eq!(json["edges"], serde_json::json!([]));
}

#[test]
fn default_scheduling_hints_are_descriptive_placeholders() {
    let hints = SchedulingHints::default();
    assert_eq!(hints.priority_class, "normal");
    assert_eq!(hints.estimated_cpu_class, "unknown");
    assert_eq!(hints.estimated_gpu_class, "none");
    assert_eq!(hints.estimated_disk_bytes, Estimate::Unknown);
    assert_eq!(hints.estimated_network_bytes, Estimate::Unknown);
    assert_eq!(hints.expected_duration, Estimate::Unknown);
}
```

Create `crates/voom-plan/src/diagnostic_test.rs`:

```rust
use super::*;

#[test]
fn planning_diagnostic_serializes_stable_code() {
    let diagnostic = PlanningDiagnostic::error(
        PlanningDiagnosticCode::UnsupportedOperationForSprint5,
        "track planning is outside Sprint 5",
    )
    .with_phase("normalize")
    .with_operation_kind("keep_tracks");

    let json = serde_json::to_value(&diagnostic).unwrap();
    assert_eq!(json["severity"], "error");
    assert_eq!(json["code"], "unsupported_operation_for_sprint5");
    assert_eq!(json["phase_name"], "normalize");
    assert_eq!(json["operation_kind"], "keep_tracks");
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p voom-plan model
cargo test -p voom-plan diagnostic
```

Expected: compile failure because model and diagnostic modules are missing.

- [ ] **Step 3: Add dependencies and exports**

In root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
voom-plan = { version = "0.1.0-dev", path = "crates/voom-plan" }
```

In `crates/voom-plan/Cargo.toml`, add:

```toml
[dependencies]
blake3 = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
time = { workspace = true, features = ["serde", "formatting"] }
voom-core = { workspace = true }
voom-policy = { workspace = true }
```

Replace `crates/voom-plan/src/lib.rs` with:

```rust
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "planner tests assert deterministic JSON fixtures directly"
    )
)]
//! Pure Sprint 5 execution-plan projection.

pub mod diagnostic;
pub mod fixtures;
pub mod hash;
pub mod model;
pub mod planner;

pub use diagnostic::{PlanningDiagnostic, PlanningDiagnosticCode, PlanningDiagnosticSeverity};
pub use model::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, Estimate, ExecutionPlan,
    InputIdentity, NodeStatus, PlanNode, PlanProvenance, PlanSummary, PlanningContext,
    PlanningRequest, PolicyIdentity, ResourceEstimates, SafetyHints, SchedulingHints, TargetRef,
};
pub use planner::{PlanGenerationError, generate_plan};
```

- [ ] **Step 4: Implement model types**

Create `model.rs` with serde-friendly structs using `BTreeMap` for deterministic maps. Keep field names exactly as specified in Sprint 5:

```rust
use std::collections::BTreeMap;

use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanningRequest {
    pub policy: voom_policy::CompiledPolicy,
    pub input: voom_policy::PolicyInputSetDraft,
    pub context: PlanningContext,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanningContext {
    pub schema_version: u32,
    pub policy_document_id: Option<voom_core::PolicyDocumentId>,
    pub policy_version_id: Option<voom_core::PolicyVersionId>,
    pub policy_input_set_id: Option<voom_core::PolicyInputSetId>,
    pub input_source_label: Option<String>,
    pub generated_at: Option<OffsetDateTime>,
    pub feature_flags: BTreeMap<String, bool>,
}

impl Default for PlanningContext {
    fn default() -> Self {
        Self {
            schema_version: 1,
            policy_document_id: None,
            policy_version_id: None,
            policy_input_set_id: None,
            input_source_label: None,
            generated_at: None,
            feature_flags: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionPlan {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan_hash: String,
    pub policy: PolicyIdentity,
    pub input: InputIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<OffsetDateTime>,
    pub summary: PlanSummary,
    pub nodes: Vec<PlanNode>,
    pub edges: Vec<Edge>,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<crate::PlanningDiagnostic>,
    pub provenance: PlanProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PolicyIdentity {
    pub slug: String,
    pub source_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<voom_core::PolicyDocumentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_id: Option<voom_core::PolicyVersionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_set_id: Option<voom_core::PolicyInputSetId>,
    pub fixture_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct PlanSummary {
    pub total_node_count: u32,
    pub executable_node_count: u32,
    pub no_op_node_count: u32,
    pub blocked_node_count: u32,
    pub target_count: u32,
    pub operation_counts_by_kind: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanNode {
    pub node_id: String,
    pub phase_name: String,
    pub ordinal: u32,
    pub target: TargetRef,
    pub operation_kind: String,
    pub operation_payload: serde_json::Value,
    pub status: NodeStatus,
    pub status_reason: String,
    pub capability_hints: CapabilityHints,
    pub scheduling_hints: SchedulingHints,
    pub resource_estimates: ResourceEstimates,
    pub artifact_expectations: ArtifactExpectations,
    pub safety_hints: SafetyHints,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Planned,
    NoOp,
    Blocked,
}

pub type TargetRef = voom_policy::TargetRef;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    pub edge_id: String,
    pub from_node_id: String,
    pub to_node_id: String,
    pub dependency_kind: DependencyKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    PhaseDependsOn,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct CapabilityHints {
    pub operation_capability: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchedulingHints {
    pub priority_class: String,
    pub estimated_cpu_class: String,
    pub estimated_gpu_class: String,
    pub estimated_disk_bytes: Estimate,
    pub estimated_network_bytes: Estimate,
    pub expected_duration: Estimate,
    pub concurrency_key: Option<String>,
}

impl Default for SchedulingHints {
    fn default() -> Self {
        Self {
            priority_class: "normal".to_owned(),
            estimated_cpu_class: "unknown".to_owned(),
            estimated_gpu_class: "none".to_owned(),
            estimated_disk_bytes: Estimate::Unknown,
            estimated_network_bytes: Estimate::Unknown,
            expected_duration: Estimate::Unknown,
            concurrency_key: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estimate {
    Unknown,
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ResourceEstimates {
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ArtifactExpectations {
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct SafetyHints {
    pub requires_approval: bool,
    pub destructive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanProvenance {
    pub planner: String,
    pub format: String,
}

impl Default for PlanProvenance {
    fn default() -> Self {
        Self {
            planner: "voom-plan".to_owned(),
            format: "sprint5-v1".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "model_test.rs"]
mod tests;
```

Create `diagnostic.rs` with the required code set:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanningDiagnosticCode {
    MissingPolicyInputTarget,
    UnsupportedOperationForSprint5,
    InsufficientSnapshotFacts,
    AmbiguousTargetSelection,
    EmptyPolicyPhases,
    EmptyInputSet,
    InvalidPlanningRequest,
    DeterministicSerializationFailure,
}

impl PlanningDiagnosticCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingPolicyInputTarget => "missing_policy_input_target",
            Self::UnsupportedOperationForSprint5 => "unsupported_operation_for_sprint5",
            Self::InsufficientSnapshotFacts => "insufficient_snapshot_facts",
            Self::AmbiguousTargetSelection => "ambiguous_target_selection",
            Self::EmptyPolicyPhases => "empty_policy_phases",
            Self::EmptyInputSet => "empty_input_set",
            Self::InvalidPlanningRequest => "invalid_planning_request",
            Self::DeterministicSerializationFailure => "deterministic_serialization_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanningDiagnostic {
    pub severity: PlanningDiagnosticSeverity,
    pub code: PlanningDiagnosticCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<voom_policy::TargetRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl PlanningDiagnostic {
    #[must_use]
    pub fn error(code: PlanningDiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity: PlanningDiagnosticSeverity::Error,
            code,
            message: message.into(),
            target: None,
            phase_name: None,
            operation_kind: None,
            suggestion: None,
        }
    }

    #[must_use]
    pub fn warning(code: PlanningDiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity: PlanningDiagnosticSeverity::Warning,
            code,
            message: message.into(),
            target: None,
            phase_name: None,
            operation_kind: None,
            suggestion: None,
        }
    }

    #[must_use]
    pub fn with_target(mut self, target: voom_policy::TargetRef) -> Self {
        self.target = Some(target);
        self
    }

    #[must_use]
    pub fn with_phase(mut self, phase_name: impl Into<String>) -> Self {
        self.phase_name = Some(phase_name.into());
        self
    }

    #[must_use]
    pub fn with_operation_kind(mut self, operation_kind: impl Into<String>) -> Self {
        self.operation_kind = Some(operation_kind.into());
        self
    }
}

#[cfg(test)]
#[path = "diagnostic_test.rs"]
mod tests;
```

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-plan model
cargo test -p voom-plan diagnostic
```

Expected: PASS.

Commit:

```bash
git add Cargo.toml crates/voom-plan/Cargo.toml crates/voom-plan/src/lib.rs crates/voom-plan/src/model.rs crates/voom-plan/src/model_test.rs crates/voom-plan/src/diagnostic.rs crates/voom-plan/src/diagnostic_test.rs
git commit -m "feat: define sprint 5 plan model"
```

## Task 3: Deterministic Hashing And Ids

**Files:**
- Create: `crates/voom-plan/src/hash.rs`
- Create: `crates/voom-plan/src/hash_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write failing hash tests**

Create `hash_test.rs`:

```rust
use crate::model::{ExecutionPlan, InputIdentity, PlanProvenance, PlanSummary, PolicyIdentity};

use super::*;

fn empty_plan(generated_at: bool) -> ExecutionPlan {
    ExecutionPlan {
        schema_version: 1,
        plan_id: String::new(),
        plan_hash: String::new(),
        policy: PolicyIdentity {
            slug: "container-metadata".to_owned(),
            source_hash: "source-hash".to_owned(),
            document_id: None,
            version_id: None,
        },
        input: InputIdentity {
            slug: Some("synthetic-compliant-baseline".to_owned()),
            source_label: Some("synthetic_compliant_baseline".to_owned()),
            input_set_id: None,
            fixture_labels: vec!["synthetic_compliant_baseline".to_owned()],
        },
        generated_at: generated_at.then(|| {
            time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
        }),
        summary: PlanSummary::default(),
        nodes: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
        diagnostics: Vec::new(),
        provenance: PlanProvenance::default(),
    }
}

#[test]
fn plan_hash_ignores_plan_hash_plan_id_and_generated_at() {
    let mut left = empty_plan(false);
    let mut right = empty_plan(true);
    left.plan_id = "plan_old".to_owned();
    left.plan_hash = "blake3:old".to_owned();
    right.plan_id = "plan_new".to_owned();
    right.plan_hash = "blake3:new".to_owned();

    assert_eq!(plan_hash(&left).unwrap(), plan_hash(&right).unwrap());
}

#[test]
fn node_and_edge_ids_are_stable_from_components() {
    assert_eq!(
        node_id("normalize", 0, "set_container", "synthetic:media_variant:variant-1"),
        node_id("normalize", 0, "set_container", "synthetic:media_variant:variant-1")
    );
    assert!(node_id("normalize", 0, "set_container", "target").starts_with("node_"));
    assert!(edge_id("node_a", "node_b", "phase_depends_on").starts_with("edge_"));
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p voom-plan hash`

Expected: compile failure naming missing `hash` module functions.

- [ ] **Step 3: Implement hashing helpers**

Create `hash.rs`:

```rust
use serde_json::Value;

use crate::ExecutionPlan;

pub fn plan_hash(plan: &ExecutionPlan) -> Result<String, serde_json::Error> {
    let mut value = serde_json::to_value(plan)?;
    strip_volatile_plan_fields(&mut value);
    Ok(format!("blake3:{}", blake3::hash(canonical_json(&value)?.as_bytes()).to_hex()))
}

pub fn plan_id(preimage: &Value) -> Result<String, serde_json::Error> {
    let hash = blake3::hash(canonical_json(preimage)?.as_bytes()).to_hex().to_string();
    Ok(format!("plan_{}", &hash[..16]))
}

#[must_use]
pub fn node_id(
    phase_name: &str,
    ordinal: u32,
    operation_kind: &str,
    target_key: &str,
) -> String {
    stable_prefixed_id(
        "node",
        &format!("{phase_name}\n{ordinal}\n{operation_kind}\n{target_key}"),
    )
}

#[must_use]
pub fn edge_id(from_node_id: &str, to_node_id: &str, dependency_kind: &str) -> String {
    stable_prefixed_id("edge", &format!("{from_node_id}\n{to_node_id}\n{dependency_kind}"))
}

pub fn canonical_json(value: &Value) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

fn stable_prefixed_id(prefix: &str, preimage: &str) -> String {
    let hash = blake3::hash(preimage.as_bytes()).to_hex().to_string();
    format!("{prefix}_{}", &hash[..16])
}

fn strip_volatile_plan_fields(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("plan_id");
        map.remove("plan_hash");
        map.remove("generated_at");
    }
}

#[cfg(test)]
#[path = "hash_test.rs"]
mod tests;
```

- [ ] **Step 4: Export helpers and commit**

Add to `lib.rs`:

```rust
pub use hash::{edge_id, node_id, plan_hash, plan_id};
```

Run: `cargo test -p voom-plan hash`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/hash.rs crates/voom-plan/src/hash_test.rs crates/voom-plan/src/lib.rs
git commit -m "feat: add deterministic plan hashes and ids"
```

## Task 4: Pure Planner For Container Operations

**Files:**
- Create: `crates/voom-plan/src/planner.rs`
- Create: `crates/voom-plan/src/planner_test.rs`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write failing planner tests**

Create `planner_test.rs` with tests that build a one-phase compiled policy and one input snapshot:

```rust
use std::collections::BTreeMap;

use voom_policy::{
    CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy, MediaSnapshotInput,
    PolicyInputSetDraft, PolicyInputSourceKind, TargetKind, TargetRef, TrackTarget,
};

use crate::{NodeStatus, PlanningContext, PlanningRequest, generate_plan};

fn policy(operation: CompiledOperation) -> CompiledPolicy {
    CompiledPolicy {
        policy_name: "container metadata".to_owned(),
        slug: "container-metadata".to_owned(),
        source_hash: "source-hash".to_owned(),
        schema_version: 2,
        metadata: BTreeMap::new(),
        config: BTreeMap::new(),
        phases: vec![CompiledPhase {
            name: "normalize".to_owned(),
            depends_on: Vec::new(),
            run_if: None,
            skip_if: None,
            on_error: None,
            operations: vec![operation],
        }],
        phase_order: vec!["normalize".to_owned()],
        warnings: Vec::new(),
        provenance: voom_policy::PolicyProvenance::default(),
    }
}

fn input(container: Option<&str>) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: "synthetic-input".to_owned(),
        display_name: "Synthetic Input".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Fixture,
        created_at: time::OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap(),
        description: None,
        fixture_labels: vec!["synthetic_input".to_owned()],
        synthetic_targets: vec![voom_policy::PolicySyntheticTarget {
            synthetic_key: "variant-1".to_owned(),
            target_kind: TargetKind::MediaVariant,
            display_name: None,
        }],
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 0,
            target: TargetRef::Synthetic {
                key: "variant-1".to_owned(),
                kind: TargetKind::MediaVariant,
            },
            container: container.map(str::to_owned),
            stream_summary: serde_json::json!({"streams": []}),
            video_codec: None,
            width: None,
            height: None,
            hdr: None,
            bitrate: None,
            duration_millis: None,
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            health_flags: Vec::new(),
            existing_media_snapshot_id: None,
        }],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

#[test]
fn set_container_plans_non_mkv_snapshot() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.summary.executable_node_count, 1);
    assert_eq!(plan.summary.operation_counts_by_kind["set_container"], 1);
}

#[test]
fn set_container_no_ops_already_mkv_snapshot() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(Some("mkv")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(plan.summary.no_op_node_count, 1);
}

#[test]
fn set_container_blocks_unknown_container_snapshot() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(None),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(plan.diagnostics[0].code.as_str(), "insufficient_snapshot_facts");
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p voom-plan set_container_`

Expected: compile failure because `generate_plan` is missing.

- [ ] **Step 3: Implement planner core**

Create `planner.rs` with:

```rust
use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use voom_policy::{CompiledOperation, CompiledPolicy, MediaSnapshotInput, PolicyInputSetDraft};

use crate::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, ExecutionPlan, InputIdentity,
    NodeStatus, PlanNode, PlanProvenance, PlanSummary, PlanningContext, PlanningDiagnostic,
    PlanningDiagnosticCode, PlanningRequest, PolicyIdentity, ResourceEstimates, SafetyHints,
    SchedulingHints, edge_id, node_id, plan_hash, plan_id,
};

#[derive(Debug)]
pub struct PlanGenerationError {
    pub diagnostics: Vec<PlanningDiagnostic>,
}

impl PlanGenerationError {
    #[must_use]
    pub fn into_voom_error(self) -> voom_core::VoomError {
        let message = self
            .diagnostics
            .first()
            .map_or_else(|| "plan generation failed".to_owned(), |d| d.message.clone());
        voom_core::VoomError::PlanGeneration(message)
    }
}

pub fn generate_plan(request: PlanningRequest) -> Result<ExecutionPlan, PlanGenerationError> {
    validate_request(&request)?;
    let mut builder = PlanBuilder::new(&request.policy, &request.input, &request.context);
    builder.expand();
    builder.finish()
}
```

Implement `PlanBuilder` in the same file with these concrete rules:

- iterate phases in `policy.phase_order`; if a named phase is missing from `policy.phases`, add an `InvalidPlanningRequest` diagnostic and skip that missing phase;
- for each `CompiledOperation::SetContainer { container }`, emit one node for each `input.media_snapshots`;
- planned node reason: `container mp4 will be changed to mkv`;
- no-op node reason: `container is already mkv`;
- blocked node reason and diagnostic message: `snapshot container is unknown`;
- capability hint for planned set-container: `Some("remux_container".to_owned())`;
- concurrency key: stable target key from `TargetRef`;
- operation payload: `{"container":"mkv"}`;
- unsupported track and tag operations emit blocked nodes per media snapshot with `unsupported_operation_for_sprint5`;
- empty input set fails with `PlanGenerationError` containing `EmptyInputSet`;
- empty policy phases returns a plan with no nodes and an `empty_policy_phases` warning diagnostic, because the design requires diagnostics but does not require a hard failure.

Use a helper to build nodes:

```rust
fn make_node(
    phase_name: &str,
    ordinal: u32,
    snapshot: &MediaSnapshotInput,
    operation_kind: &str,
    operation_payload: serde_json::Value,
    status: NodeStatus,
    status_reason: String,
    capability: Option<String>,
) -> PlanNode {
    let target_key = target_key(&snapshot.target);
    let mut scheduling_hints = SchedulingHints::default();
    scheduling_hints.concurrency_key = Some(target_key.clone());
    PlanNode {
        node_id: node_id(phase_name, ordinal, operation_kind, &target_key),
        phase_name: phase_name.to_owned(),
        ordinal,
        target: snapshot.target.clone(),
        operation_kind: operation_kind.to_owned(),
        operation_payload,
        status,
        status_reason,
        capability_hints: CapabilityHints {
            operation_capability: capability,
        },
        scheduling_hints,
        resource_estimates: ResourceEstimates::default(),
        artifact_expectations: ArtifactExpectations::default(),
        safety_hints: SafetyHints::default(),
    }
}
```

After nodes are built, compute `summary`, `edges`, `plan_hash`, and `plan_id`:

- `summary.target_count` is the count of distinct node target keys.
- `operation_counts_by_kind` increments every emitted node, including blocked and no-op nodes.
- dependency edges connect every node in a dependency phase to every node in the dependent phase when the dependent phase names that dependency in `depends_on`.
- `plan_id` uses a JSON preimage containing policy identity, input identity, summary, nodes, edges, warnings, diagnostics, and provenance before `plan_hash` is set.
- `plan_hash` is computed after the public plan is assembled.

- [ ] **Step 4: Run focused tests and commit**

Run: `cargo test -p voom-plan set_container_`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs crates/voom-plan/src/lib.rs
git commit -m "feat: plan container operations"
```

## Task 5: Unsupported Operations, Conditions, And Phase Edges

**Files:**
- Modify: `crates/voom-plan/src/planner.rs`
- Modify: `crates/voom-plan/src/planner_test.rs`

- [ ] **Step 1: Add failing tests for loud unsupported behavior**

Add tests to `planner_test.rs`:

```rust
#[test]
fn unresolved_condition_emits_blocked_node_for_nested_operation() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Conditional {
            condition: CompiledCondition::Predicate {
                name: "external_host_state".to_owned(),
            },
            operations: vec![CompiledOperation::SetContainer {
                container: "mkv".to_owned(),
            }],
        }),
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.nodes[0].operation_kind, "set_container");
    assert_eq!(plan.diagnostics[0].code.as_str(), "insufficient_snapshot_facts");
}

#[test]
fn track_operations_emit_blocked_nodes_instead_of_disappearing() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::KeepTracks {
            target: TrackTarget::Audio,
            filter: None,
        }),
        input: input(Some("mkv")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.nodes[0].operation_kind, "keep_tracks");
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(plan.diagnostics.len(), 1);
}

#[test]
fn tag_operations_emit_blocked_nodes_instead_of_disappearing() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::ClearTags),
        input: input(Some("mkv")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.nodes[0].operation_kind, "clear_tags");
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(plan.diagnostics.len(), 1);
}

#[test]
fn phase_depends_on_creates_stable_edges() {
    let mut compiled = policy(CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    });
    compiled.phases.push(CompiledPhase {
        name: "verify".to_owned(),
        depends_on: vec!["normalize".to_owned()],
        run_if: None,
        skip_if: None,
        on_error: None,
        operations: vec![CompiledOperation::ClearTags],
    });
    compiled.phase_order = vec!["normalize".to_owned(), "verify".to_owned()];

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.edges.len(), 1);
    assert_eq!(plan.edges[0].dependency_kind, crate::DependencyKind::PhaseDependsOn);
    assert_eq!(plan.edges[0].from_node_id, plan.nodes[0].node_id);
    assert_eq!(plan.edges[0].to_node_id, plan.nodes[1].node_id);
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p voom-plan tag_operations_emit_blocked_nodes_instead_of_disappearing
cargo test -p voom-plan track_operations_emit_blocked_nodes_instead_of_disappearing
cargo test -p voom-plan unresolved_condition_emits_blocked_node_for_nested_operation
cargo test -p voom-plan phase_depends_on_creates_stable_edges
```

Expected: one or both tests fail until unsupported operations and edge creation are implemented.

- [ ] **Step 3: Implement operation kind mapping**

Add a deterministic mapper in `planner.rs`:

```rust
fn operation_kind(operation: &CompiledOperation) -> &'static str {
    match operation {
        CompiledOperation::SetContainer { .. } => "set_container",
        CompiledOperation::KeepTracks { .. } => "keep_tracks",
        CompiledOperation::RemoveTracks { .. } => "remove_tracks",
        CompiledOperation::ReorderTracks { .. } => "reorder_tracks",
        CompiledOperation::SetDefaults { .. } => "set_defaults",
        CompiledOperation::ClearTrackActions { .. } => "clear_track_actions",
        CompiledOperation::ClearTags => "clear_tags",
        CompiledOperation::SetTag { .. } => "set_tag",
        CompiledOperation::DeleteTag { .. } => "delete_tag",
        CompiledOperation::Conditional { .. } => "conditional",
        CompiledOperation::Rules { .. } => "rules",
    }
}
```

For unsupported leaf operations, emit blocked nodes per snapshot with payload from `serde_json::to_value(operation)`, status reason `operation is not supported by Sprint 5 planner`, and an `UnsupportedOperationForSprint5` diagnostic containing phase, target, and operation kind.

- [ ] **Step 4: Implement deterministic condition traversal**

Add `evaluate_condition` returning `Option<bool>`:

```rust
fn evaluate_condition(
    condition: &voom_policy::CompiledCondition,
    snapshot: &MediaSnapshotInput,
) -> Option<bool> {
    match condition {
        voom_policy::CompiledCondition::FieldComparison { path, op, value } => {
            evaluate_field_comparison(path, *op, value, snapshot)
        }
        voom_policy::CompiledCondition::FieldExists { path } => snapshot_field(path, snapshot).map(|_| true),
        voom_policy::CompiledCondition::Not { inner } => evaluate_condition(inner, snapshot).map(|v| !v),
        voom_policy::CompiledCondition::And { conditions } => {
            let mut saw_unknown = false;
            for condition in conditions {
                match evaluate_condition(condition, snapshot) {
                    Some(false) => return Some(false),
                    Some(true) => {}
                    None => saw_unknown = true,
                }
            }
            (!saw_unknown).then_some(true)
        }
        voom_policy::CompiledCondition::Or { conditions } => {
            let mut saw_unknown = false;
            for condition in conditions {
                match evaluate_condition(condition, snapshot) {
                    Some(true) => return Some(true),
                    Some(false) => {}
                    None => saw_unknown = true,
                }
            }
            (!saw_unknown).then_some(false)
        }
        voom_policy::CompiledCondition::Exists { .. }
        | voom_policy::CompiledCondition::Count { .. }
        | voom_policy::CompiledCondition::Predicate { .. } => None,
    }
}
```

Support `container`, `video_codec`, `width`, `height`, `hdr`, `bitrate`, and `duration_millis` field paths. For unknown conditions, operations under that branch emit blocked nodes with `InsufficientSnapshotFacts`.

- [ ] **Step 5: Run and commit**

Run:

```bash
cargo test -p voom-plan tag_operations_emit_blocked_nodes_instead_of_disappearing
cargo test -p voom-plan track_operations_emit_blocked_nodes_instead_of_disappearing
cargo test -p voom-plan unresolved_condition_emits_blocked_node_for_nested_operation
cargo test -p voom-plan phase_depends_on_creates_stable_edges
```

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/planner.rs crates/voom-plan/src/planner_test.rs
git commit -m "feat: preserve blocked planning diagnostics"
```

## Task 6: Golden Plan Fixtures

**Files:**
- Create: `crates/voom-plan/src/fixtures.rs`
- Create: `crates/voom-plan/src/fixtures_test.rs`
- Create: `crates/voom-plan/fixtures/plans/container_metadata_compliant.json`
- Create: `crates/voom-plan/fixtures/plans/container_metadata_noncompliant.json`
- Modify: `crates/voom-plan/src/lib.rs`

- [ ] **Step 1: Write fixture tests before creating golden files**

Create `fixtures_test.rs`:

```rust
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};

use crate::{ExecutionPlan, PlanningContext, PlanningRequest, generate_plan};

#[test]
fn compliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_compliant_baseline".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_compliant").unwrap()
    );
}

#[test]
fn noncompliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_noncompliant_transcode_needed".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_noncompliant").unwrap()
    );
}

#[test]
fn golden_plans_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
    ] {
        let value = load_golden_plan(name).unwrap();
        serde_json::from_value::<ExecutionPlan>(value).unwrap();
    }
}
```

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p voom-plan fixture`

Expected: failure because fixture loader and golden files are absent.

- [ ] **Step 3: Add fixture loader**

Create `fixtures.rs`:

```rust
pub fn load_golden_plan(name: &str) -> Result<serde_json::Value, serde_json::Error> {
    let source = match name {
        "container_metadata_compliant" => {
            include_str!("../fixtures/plans/container_metadata_compliant.json")
        }
        "container_metadata_noncompliant" => {
            include_str!("../fixtures/plans/container_metadata_noncompliant.json")
        }
        _ => "null",
    };
    serde_json::from_str(source)
}

#[cfg(test)]
#[path = "fixtures_test.rs"]
mod tests;
```

Export `load_golden_plan` from `lib.rs`:

```rust
pub use fixtures::load_golden_plan;
```

- [ ] **Step 4: Generate and review golden JSON**

Temporarily print the generated JSON from the tests with `eprintln!("{}", serde_json::to_string_pretty(&plan).unwrap());`, run:

```bash
cargo test -p voom-plan compliant_container_fixture_matches_golden_plan -- --nocapture
cargo test -p voom-plan noncompliant_container_fixture_matches_golden_plan -- --nocapture
```

Create the two golden JSON files from the generated pretty output, then remove the temporary `eprintln!` calls. The compliant fixture must include a no-op `set_container` node and blocked tag nodes from `clear_tags`, `set_tag`, and `delete_tag`. The noncompliant fixture must include a planned `set_container` node and the same deterministic blocked tag nodes.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p voom-plan fixture`

Expected: PASS.

Commit:

```bash
git add crates/voom-plan/src/fixtures.rs crates/voom-plan/src/fixtures_test.rs crates/voom-plan/src/lib.rs crates/voom-plan/fixtures/plans/container_metadata_compliant.json crates/voom-plan/fixtures/plans/container_metadata_noncompliant.json
git commit -m "test: add golden plan fixtures"
```

## Task 7: Control-Plane Planning Use Cases

**Files:**
- Modify: `crates/voom-control-plane/Cargo.toml`
- Create: `crates/voom-control-plane/src/cases/plans.rs`
- Create: `crates/voom-control-plane/src/cases/plans_test.rs`
- Modify: `crates/voom-control-plane/src/cases/mod.rs`
- Modify: `crates/voom-control-plane/src/lib.rs`

- [ ] **Step 1: Write failing control-plane tests**

Create `plans_test.rs`:

```rust
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};

use super::*;
use crate::cases::cp;

#[test]
fn plan_policy_source_with_input_draft_does_not_need_database() {
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let plan = plan_policy_source_with_input(
        &source,
        input,
        Some("synthetic_noncompliant_transcode_needed"),
    )
    .unwrap();

    assert_eq!(plan.policy.slug, "container-metadata");
    assert_eq!(plan.input.source_label.as_deref(), Some("synthetic_noncompliant_transcode_needed"));
    assert!(plan.nodes.iter().any(|node| node.status == voom_plan::NodeStatus::Planned));
}

#[tokio::test]
async fn durable_planning_reads_compiled_policy_without_creating_execution_state() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp.create_policy_document("container-metadata", &source).await.unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap())
        .await
        .unwrap();

    let before_jobs = count_rows(&cp, "jobs").await;
    let before_events = count_rows(&cp, "events").await;
    let before_tickets = count_rows(&cp, "tickets").await;

    let plan = cp
        .plan_accepted_policy_version_with_input_set(created_policy.version.id, input.id)
        .await
        .unwrap();

    assert_eq!(plan.policy.version_id, Some(created_policy.version.id));
    assert_eq!(plan.input.input_set_id, Some(input.id));
    assert_eq!(before_jobs, count_rows(&cp, "jobs").await);
    assert_eq!(before_events, count_rows(&cp, "events").await);
    assert_eq!(before_tickets, count_rows(&cp, "tickets").await);
}

async fn count_rows(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}
```

If `pool_for_test()` does not exist, add a `#[cfg(any(test, feature = "test-support"))]` accessor on `ControlPlane` returning `&SqlitePool`, matching the repo-accessor pattern in `lib.rs`.

- [ ] **Step 2: Run tests and verify failure**

Run: `cargo test -p voom-control-plane plan_`

Expected: compile failure because plan use cases and `voom-plan` dependency are missing.

- [ ] **Step 3: Add dependency and module**

In `crates/voom-control-plane/Cargo.toml`:

```toml
voom-plan = { workspace = true }
```

In `cases/mod.rs`:

```rust
pub mod plans;
```

In `lib.rs`, expose the source-only functions for CLI use without requiring a `ControlPlane` database handle:

```rust
pub use cases::plans::{plan_compiled_policy_with_input, plan_policy_source_with_input};
```

- [ ] **Step 4: Implement source-only and durable use cases**

Create `plans.rs`:

```rust
use voom_core::{PolicyInputSetId, PolicyVersionId, VoomError};
use voom_store::repo::{policies::PolicyRepo, policy_inputs::PolicyInputRepo};

use crate::ControlPlane;

pub fn plan_compiled_policy_with_input(
    policy: voom_policy::CompiledPolicy,
    input: voom_policy::PolicyInputSetDraft,
    mut context: voom_plan::PlanningContext,
) -> Result<voom_plan::ExecutionPlan, VoomError> {
    context.schema_version = 1;
    voom_plan::generate_plan(voom_plan::PlanningRequest {
        policy,
        input,
        context,
    })
    .map_err(voom_plan::PlanGenerationError::into_voom_error)
}

pub fn plan_policy_source_with_input(
    source: &str,
    input: voom_policy::PolicyInputSetDraft,
    input_source_label: Option<&str>,
) -> Result<voom_plan::ExecutionPlan, VoomError> {
    let compiled = voom_policy::compile_policy(source).map_err(|err| err.error)?.policy;
    plan_compiled_policy_with_input(
        compiled,
        input,
        voom_plan::PlanningContext {
            input_source_label: input_source_label.map(str::to_owned),
            ..voom_plan::PlanningContext::default()
        },
    )
}

impl ControlPlane {
    pub async fn plan_accepted_policy_version_with_input_set(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
    ) -> Result<voom_plan::ExecutionPlan, VoomError> {
        let version = self
            .policies
            .get_version(policy_version_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("policy version {policy_version_id} not found")))?;
        let policy: voom_policy::CompiledPolicy =
            serde_json::from_value(version.compiled_json.clone()).map_err(|e| {
                VoomError::PlanGeneration(format!("stored compiled policy JSON is invalid: {e}"))
            })?;
        if policy.source_hash != version.source_hash || policy.schema_version != version.schema_version {
            return Err(VoomError::PlanGeneration(format!(
                "stored compiled policy identity mismatch for policy version {policy_version_id}"
            )));
        }
        let input = self
            .policy_inputs
            .get_input_set(input_set_id)
            .await?
            .ok_or_else(|| VoomError::NotFound(format!("policy input set {input_set_id} not found")))?;
        let draft = input_set_to_draft(input);
        plan_compiled_policy_with_input(
            policy,
            draft,
            voom_plan::PlanningContext {
                policy_document_id: Some(version.policy_document_id),
                policy_version_id: Some(version.id),
                policy_input_set_id: Some(input_set_id),
                ..voom_plan::PlanningContext::default()
            },
        )
    }
}
```

Implement `input_set_to_draft` and target conversion in the same file by mapping every repository child row back to the matching `voom_policy::*Input` type. For `PolicyInputTargetRef::Synthetic`, drop the repository synthetic id and keep `{ key, kind }`; for real target refs, copy the typed id.

- [ ] **Step 5: Run and commit**

Run: `cargo test -p voom-control-plane plan_`

Expected: PASS.

Commit:

```bash
git add crates/voom-control-plane/Cargo.toml crates/voom-control-plane/src/cases/mod.rs crates/voom-control-plane/src/cases/plans.rs crates/voom-control-plane/src/cases/plans_test.rs crates/voom-control-plane/src/lib.rs
git commit -m "feat: add read-only planning use cases"
```

## Task 8: CLI Plan Commands

**Files:**
- Modify: `crates/voom-cli/Cargo.toml`
- Modify: `crates/voom-cli/src/cli.rs`
- Modify: `crates/voom-cli/src/main.rs`
- Modify: `crates/voom-cli/src/commands/mod.rs`
- Create: `crates/voom-cli/src/commands/plan.rs`
- Create: `crates/voom-cli/src/commands/plan_test.rs`

- [ ] **Step 1: Write command payload tests**

Create `plan_test.rs`:

```rust
use super::*;

#[test]
fn fixture_name_parser_accepts_public_labels() {
    assert_eq!(
        fixture_name("synthetic_compliant_baseline").unwrap(),
        voom_policy::FixtureName::SyntheticCompliantBaseline
    );
    assert_eq!(
        fixture_name("synthetic_noncompliant_transcode_needed").unwrap(),
        voom_policy::FixtureName::SyntheticNoncompliantTranscodeNeeded
    );
}

#[test]
fn plan_data_wraps_plan_under_plan_key() {
    let data = PlanData {
        plan: voom_plan::ExecutionPlan {
            schema_version: 1,
            plan_id: "plan_test".to_owned(),
            plan_hash: "blake3:test".to_owned(),
            policy: voom_plan::PolicyIdentity {
                slug: "p".to_owned(),
                source_hash: "h".to_owned(),
                document_id: None,
                version_id: None,
            },
            input: voom_plan::InputIdentity {
                slug: None,
                source_label: None,
                input_set_id: None,
                fixture_labels: Vec::new(),
            },
            generated_at: None,
            summary: voom_plan::PlanSummary::default(),
            nodes: Vec::new(),
            edges: Vec::new(),
            warnings: Vec::new(),
            diagnostics: Vec::new(),
            provenance: voom_plan::PlanProvenance::default(),
        },
    };

    let json = serde_json::to_value(data).unwrap();
    assert_eq!(json["plan"]["plan_id"], "plan_test");
}
```

- [ ] **Step 2: Run tests and verify failure**

Run:

```bash
cargo test -p voom-cli fixture_name_parser_accepts_public_labels
cargo test -p voom-cli plan_data_wraps_plan_under_plan_key
```

Expected: compile failure because the plan command module is missing.

- [ ] **Step 3: Add CLI dependencies and args**

In `crates/voom-cli/Cargo.toml` dependencies, add:

```toml
voom-plan = { workspace = true }
voom-policy = { workspace = true }
```

In `cli.rs`, add a nested plan command:

```rust
#[derive(Subcommand, Debug)]
pub enum Command {
    Version,
    Health,
    Init,
    #[command(subcommand)]
    Plan(PlanCommand),
}

#[derive(Subcommand, Debug)]
pub enum PlanCommand {
    DryRun {
        #[arg(long)]
        policy_file: std::path::PathBuf,
        #[arg(long)]
        input_fixture: String,
    },
    Show {
        #[arg(long)]
        policy_version_id: u64,
        #[arg(long)]
        input_set_id: u64,
    },
}
```

- [ ] **Step 4: Implement command module**

Create `commands/plan.rs`:

```rust
use std::{io, path::Path};

use serde::Serialize;
use voom_core::{ErrorCode, VoomError};

use crate::envelope::{Local, emit_err, emit_ok};

#[derive(Debug, Serialize)]
pub struct PlanData {
    pub plan: voom_plan::ExecutionPlan,
}

pub async fn dry_run(policy_file: &Path, input_fixture: &str) -> io::Result<i32> {
    let source = match tokio::fs::read_to_string(policy_file).await {
        Ok(source) => source,
        Err(err) => {
            emit_err(
                "plan",
                ErrorCode::ConfigInvalid.as_str(),
                format!("policy file {}: {err}", policy_file.display()),
                None,
                None,
            )?;
            return Ok(2);
        }
    };
    let fixture = match fixture_name(input_fixture) {
        Ok(fixture) => fixture,
        Err(message) => {
            emit_err("plan", ErrorCode::ConfigInvalid.as_str(), message.to_owned(), None, None)?;
            return Ok(2);
        }
    };
    let input = match voom_policy::load_fixture(fixture) {
        Ok(input) => input,
        Err(err) => {
            emit_err("plan", ErrorCode::Internal.as_str(), err.to_string(), None, None)?;
            return Ok(2);
        }
    };
    let plan = match voom_control_plane::plan_policy_source_with_input(
        &source,
        input,
        Some(input_fixture),
    ) {
        Ok(plan) => plan,
        Err(err) => {
            emit_voom_error(&err, None)?;
            return Ok(2);
        }
    };
    emit_ok("plan", PlanData { plan }, None, Vec::new())?;
    Ok(0)
}

pub async fn show(
    database_url: &str,
    local: Local,
    policy_version_id: u64,
    input_set_id: u64,
) -> io::Result<i32> {
    let cp = match voom_control_plane::ControlPlane::open(database_url).await {
        Ok(cp) => cp,
        Err(err) => {
            emit_voom_error(&err, Some(local))?;
            return Ok(2);
        }
    };
    let plan = match cp
        .plan_accepted_policy_version_with_input_set(
            voom_core::PolicyVersionId(policy_version_id),
            voom_core::PolicyInputSetId(input_set_id),
        )
        .await
    {
        Ok(plan) => plan,
        Err(err) => {
            emit_voom_error(&err, Some(local))?;
            return Ok(2);
        }
    };
    emit_ok("plan", PlanData { plan }, Some(local), Vec::new())?;
    Ok(0)
}

fn emit_voom_error(err: &VoomError, local: Option<Local>) -> io::Result<()> {
    emit_err("plan", err.code(), err.to_string(), None, local)
}

pub fn fixture_name(value: &str) -> Result<voom_policy::FixtureName, &'static str> {
    match value {
        "synthetic_compliant_baseline" => Ok(voom_policy::FixtureName::SyntheticCompliantBaseline),
        "synthetic_noncompliant_transcode_needed" => {
            Ok(voom_policy::FixtureName::SyntheticNoncompliantTranscodeNeeded)
        }
        _ => Err("unknown input fixture"),
    }
}

#[cfg(test)]
#[path = "plan_test.rs"]
mod tests;
```

Plan command handlers must emit their own `command: "plan"` success and error envelopes for expected policy, planning, config, not-found, and database failures. Do not let expected `VoomError`s bubble to `main`'s fallback error handler, because that path emits the generic `command: "internal"` envelope.

- [ ] **Step 5: Wire dispatch**

In `commands/mod.rs`:

```rust
pub mod plan;
```

In `main.rs`, import `PlanCommand` and dispatch:

```rust
Command::Plan(plan_command) => match plan_command {
    PlanCommand::DryRun {
        policy_file,
        input_fixture,
    } => Ok(Exit::from_run_code(plan::dry_run(&policy_file, &input_fixture).await?)),
    PlanCommand::Show {
        policy_version_id,
        input_set_id,
    } => {
        let cfg = match resolve_cfg(&cli) {
            Ok(cfg) => cfg,
            Err(err) => {
                voom_cli::envelope::emit_err("plan", err.code(), err.to_string(), None, None)?;
                return Ok(Exit::Failure);
            }
        };
        let local = Local {
            db_url: cfg.database_url.clone(),
            config_path: cfg.config_path.display().to_string(),
        };
        Ok(Exit::from_run_code(
            plan::show(&cfg.database_url, local, policy_version_id, input_set_id).await?,
        ))
    }
}
```

The top-level `main` fallback still maps unexpected `VoomError`s through `VoomError::error_code()`, but no expected plan command failure should rely on that fallback. Expected plan failures must be emitted in `commands/plan.rs` so the envelope command remains `plan`.

- [ ] **Step 6: Run and commit**

Run:

```bash
cargo test -p voom-cli fixture_name_parser_accepts_public_labels
cargo test -p voom-cli plan_data_wraps_plan_under_plan_key
```

Expected: PASS.

Commit:

```bash
git add crates/voom-cli/Cargo.toml crates/voom-cli/src/cli.rs crates/voom-cli/src/main.rs crates/voom-cli/src/commands/mod.rs crates/voom-cli/src/commands/plan.rs crates/voom-cli/src/commands/plan_test.rs
git commit -m "feat: add plan CLI commands"
```

## Task 9: CLI Integration Snapshots And Read-Only Proofs

**Files:**
- Create: `crates/voom-cli/tests/plan_envelope.rs`
- Create: `crates/voom-cli/tests/snapshots/plan_envelope__dry_run_noncompliant.snap`
- Create: `crates/voom-cli/tests/snapshots/plan_envelope__show_noncompliant.snap`
- Create: `crates/voom-cli/tests/snapshots/plan_envelope__parse_error.snap`
- Create: `crates/voom-cli/tests/snapshots/plan_envelope__missing_input_set.snap`

- [ ] **Step 1: Write CLI integration tests**

Create `plan_envelope.rs`:

```rust
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::{NamedTempFile, tempdir};
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::test_support::sqlite_url_for;

#[test]
fn dry_run_noncompliant_succeeds_without_database() {
    let dir = tempdir().unwrap();
    let policy_path = dir.path().join("container-metadata.voom");
    std::fs::write(
        &policy_path,
        load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap(),
    )
    .unwrap();
    let db_path = dir.path().join("must-not-exist.sqlite");

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .env_remove("VOOM_DATABASE_URL")
        .env("VOOM_LOG_FORMAT", "json")
        .args([
            "--database-url",
            &format!("sqlite://{}", db_path.display()),
            "plan",
            "dry-run",
            "--policy-file",
            policy_path.to_str().unwrap(),
            "--input-fixture",
            "synthetic_noncompliant_transcode_needed",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert!(!db_path.exists(), "source-only dry-run must not create database files");
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["plan"]["input"]["source_label"], "synthetic_noncompliant_transcode_needed");
    insta::assert_json_snapshot!("dry_run_noncompliant", json);
}

#[tokio::test]
async fn show_noncompliant_reads_durable_policy_and_input() {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = voom_control_plane::ControlPlane::open_with_pool(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
    )
    .await
    .unwrap();
    let created = cp
        .create_policy_document(
            "container-metadata",
            &load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap(),
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap())
        .await
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &url,
            "plan",
            "show",
            "--policy-version-id",
            &created.version.id.0.to_string(),
            "--input-set-id",
            &input.id.0.to_string(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["data"]["plan"]["policy"]["version_id"], created.version.id.0);
    assert_eq!(json["data"]["plan"]["input"]["input_set_id"], input.id.0);
    insta::assert_json_snapshot!("show_noncompliant", json);
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}
```

Add parse-error and missing-input-set tests in the same file:

- parse error writes an invalid policy file, runs `plan dry-run`, expects exit `2` and `error.code == "POLICY_PARSE_ERROR"`;
- missing input set initializes an empty DB, runs `plan show --policy-version-id <real> --input-set-id 999999`, expects exit `2` and `error.code == "NOT_FOUND"`.

- [ ] **Step 2: Run tests and verify snapshots are new**

Run: `cargo test -p voom-cli --test plan_envelope`

Expected: test failures or new insta snapshots until snapshots are accepted.

- [ ] **Step 3: Review snapshots deliberately**

Run: `cargo insta review`

Accept only snapshots whose stdout contains one envelope, command `plan`, stable ids/hashes, and no host-specific path in source-only dry-run output.

- [ ] **Step 4: Run and commit**

Run: `cargo test -p voom-cli --test plan_envelope`

Expected: PASS.

Commit:

```bash
git add crates/voom-cli/tests/plan_envelope.rs crates/voom-cli/tests/snapshots/plan_envelope__*.snap
git commit -m "test: cover plan CLI envelopes"
```

## Task 10: Closeout Verification

**Files:**
- Review: `docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md`
- Review: all changed files

- [ ] **Step 1: Run focused crate tests**

Run:

```bash
cargo test -p voom-plan
cargo test -p voom-control-plane plan_
cargo test -p voom-cli --test plan_envelope
```

Expected: all pass with no skipped tests.

- [ ] **Step 2: Run layout and snapshot checks**

Run:

```bash
just check-test-layout
cargo insta test -p voom-cli
```

Expected: sibling unit-test layout passes and snapshots are reviewed.

- [ ] **Step 3: Run full CI**

Run: `just ci`

Expected: `fmt-check`, `lint`, `check-test-layout`, `test`, `doc`, `deny`, and `audit` all pass. If `deny` or `audit` fails because of an upstream advisory unrelated to Sprint 5, capture the exact advisory and command output in the final handoff.

- [ ] **Step 4: Acceptance matrix audit**

Check each Sprint 5 requirement against the implementation:

- pure `voom-plan` dependency boundary;
- deterministic source-only plan JSON;
- durable planning from stored compiled JSON;
- no execution-state writes;
- blocked diagnostics for unsupported track/tag operations;
- CLI parse, validation, planning, and missing-row error envelopes;
- fixture golden deserialization.

If implementation discovers a real deferral, append a dated closeout note to the design doc with the exact deferred behavior and reason. Do not add aspirational notes.

- [ ] **Step 5: Commit closeout docs if changed**

If the design doc changed, run:

```bash
git add docs/superpowers/specs/2026-05-23-voom-sprint-5-design.md
git commit -m "docs: record sprint 5 closeout notes"
```

If the design doc did not change, no closeout commit is needed.
