#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        reason = "planner tests assert deterministic JSON fixtures directly"
    )
)]
//! Pure Sprint 5 execution-plan projection.

pub mod compliance_model;
pub mod diagnostic;
pub mod fixtures;
pub mod hash;
pub mod model;
pub mod planner;

pub use compliance_model::{
    CheckStatus, ComplianceCheck, ComplianceDiagnostic, ComplianceDiagnosticCode,
    ComplianceDiagnosticSeverity, ComplianceInputIdentity, CompliancePolicyIdentity,
    ComplianceProvenance, ComplianceReport, ComplianceSummary, ExecutionEligibility,
    IssueActionHint, ReportStatus,
};
pub use diagnostic::{PlanningDiagnostic, PlanningDiagnosticCode, PlanningDiagnosticSeverity};
pub use fixtures::{GoldenPlanFixtureError, load_golden_plan};
pub use hash::{edge_id, node_id, plan_hash, plan_id};
pub use model::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, Estimate, ExecutionPlan,
    InputIdentity, NodeStatus, PlanNode, PlanProvenance, PlanSummary, PlanningContext,
    PlanningRequest, PolicyIdentity, ResourceEstimates, SafetyHints, SchedulingHints, TargetRef,
};
pub use planner::{PlanGenerationError, generate_plan};
