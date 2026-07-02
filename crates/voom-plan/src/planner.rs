use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPolicy, CompiledRule,
    CompiledValue, DiagnosticSeverity, MediaSnapshotInput, PolicyDiagnostic, PolicyInputSetDraft,
    RuleMatchMode,
};

use crate::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, ExecutionPlan, InputIdentity,
    NodeStatus, PlanNode, PlanOperationKind, PlanProvenance, PlanSummary, PlanningContext,
    PlanningDiagnostic, PlanningDiagnosticCode, PlanningRequest, PolicyIdentity, ResourceEstimates,
    SafetyHints, SchedulingHints, TargetRef, edge_id, node_id, plan_hash, plan_id,
};

pub mod audio;
pub mod remux;
pub mod transcode_video;

pub use transcode_video::video_stream_field;

#[derive(Debug)]
pub struct PlanGenerationError {
    pub diagnostics: Vec<PlanningDiagnostic>,
}

impl PlanGenerationError {
    #[must_use]
    pub fn into_voom_error(self) -> voom_core::VoomError {
        let message = self.diagnostics.first().map_or_else(
            || "plan generation failed".to_owned(),
            |d| d.message.clone(),
        );
        voom_core::VoomError::PlanGeneration(message)
    }
}

pub fn generate_plan(request: PlanningRequest) -> Result<ExecutionPlan, PlanGenerationError> {
    let PlanningRequest {
        policy,
        input,
        context,
    } = request;
    validate_input(&input)?;

    let mut builder = PlanBuilder::new(&policy, &input, &context);
    builder.expand();
    builder.finish()
}

/// Plans exactly one named phase against the supplied planning input.
///
/// Sprint 16's multi-phase coordinator drives the executor one phase at a time
/// (`docs/superpowers/specs/2026-05-29-voom-sprint-16-design.md` §5,
/// `docs/adr/0005-plan-phase-entry-point.md`): it projects each file's current
/// snapshot into `request.input` and re-invokes the planner per phase so
/// `run_if`/`skip_if` re-evaluate against the artifact the prior phase produced.
///
/// Shares the single planning code path with [`generate_plan`]; the resulting
/// plan is deterministic from `(compiled policy, phase, snapshot)` and carries
/// only the named phase's nodes (and therefore no inter-phase edges — ordering
/// is the coordinator's barrier, not encoded in a per-phase plan).
///
/// # Errors
///
/// Returns [`PlanGenerationError`] when the input set is empty or when
/// `phase_name` is not declared in `phase_order` (a coordinator bug — the bound
/// is the declared phase count, and a phase outside it can never be planned). An
/// operation that cannot be planned against the refreshed snapshot (for example
/// a selector that now matches nothing) is **not** an error: it yields a
/// `Blocked` node plus a planning diagnostic the coordinator turns into a
/// blocked issue.
pub fn plan_phase(
    request: PlanningRequest,
    phase_name: &str,
) -> Result<ExecutionPlan, PlanGenerationError> {
    let PlanningRequest {
        policy,
        input,
        context,
    } = request;
    validate_input(&input)?;

    if !policy.phase_order.iter().any(|name| name == phase_name) {
        return Err(invalid_phase_request(
            phase_name,
            format!("phase {phase_name} is not declared in phase_order"),
        ));
    }
    // A name bounded by phase_order but absent from `phases` is an internally
    // inconsistent policy. Fail loud here rather than letting the shared
    // expansion emit a node-less plan a coordinator could misread as a
    // legitimately skipped phase (the symmetric structural error to the check
    // above; see docs/adr/0005-plan-phase-entry-point.md).
    if !policy.phases.iter().any(|phase| phase.name == phase_name) {
        return Err(invalid_phase_request(
            phase_name,
            format!("phase {phase_name} is listed in phase_order but is missing"),
        ));
    }

    let mut builder = PlanBuilder::new(&policy, &input, &context);
    builder.expand_declared_phase(phase_name);
    builder.finish()
}

fn validate_input(input: &PolicyInputSetDraft) -> Result<(), PlanGenerationError> {
    if input.media_snapshots.is_empty() {
        return Err(PlanGenerationError {
            diagnostics: vec![PlanningDiagnostic::error(
                PlanningDiagnosticCode::EmptyInputSet,
                "planner input set has no media snapshots",
            )],
        });
    }

    Ok(())
}

struct PlanBuilder<'a> {
    policy: &'a CompiledPolicy,
    input: &'a PolicyInputSetDraft,
    context: &'a PlanningContext,
    nodes: Vec<PlanNode>,
    warnings: Vec<String>,
    diagnostics: Vec<PlanningDiagnostic>,
    fatal: Option<PlanGenerationError>,
}

#[derive(Debug)]
struct OperationPlan {
    operation_kind: PlanOperationKind,
    operation_payload: serde_json::Value,
    observed_state: Option<serde_json::Value>,
    status: NodeStatus,
    status_reason: String,
    capability: Option<String>,
    resource_estimates: ResourceEstimates,
    diagnostics: Vec<PlanningDiagnostic>,
}

impl OperationPlan {
    fn new(
        operation_kind: PlanOperationKind,
        operation_payload: serde_json::Value,
        observed_state: Option<serde_json::Value>,
        status: NodeStatus,
        status_reason: String,
        capability: Option<String>,
    ) -> Self {
        Self {
            operation_kind,
            operation_payload,
            observed_state,
            status,
            status_reason,
            capability,
            resource_estimates: ResourceEstimates::default(),
            diagnostics: Vec::new(),
        }
    }

    fn with_diagnostic(mut self, diagnostic: PlanningDiagnostic) -> Self {
        self.diagnostics.push(diagnostic);
        self
    }

    fn with_resource_estimates(mut self, resource_estimates: ResourceEstimates) -> Self {
        self.resource_estimates = resource_estimates;
        self
    }
}

impl<'a> PlanBuilder<'a> {
    fn new(
        policy: &'a CompiledPolicy,
        input: &'a PolicyInputSetDraft,
        context: &'a PlanningContext,
    ) -> Self {
        Self {
            policy,
            input,
            context,
            nodes: Vec::new(),
            warnings: policy_warnings(policy),
            diagnostics: Vec::new(),
            fatal: None,
        }
    }

    fn expand(&mut self) {
        if self.policy.phase_order.is_empty() {
            let message = "compiled policy has no phases";
            self.warnings.push(message.to_owned());
            self.diagnostics.push(PlanningDiagnostic::warning(
                PlanningDiagnosticCode::EmptyPolicyPhases,
                message,
            ));
            return;
        }

        for phase_name in &self.policy.phase_order {
            self.expand_declared_phase(phase_name);
        }
    }

    /// Expands a single phase that the caller has already confirmed is declared
    /// in `phase_order`. Shared by the whole-policy [`Self::expand`] loop and the
    /// per-phase [`plan_phase`] entry point so there is one planning code path.
    fn expand_declared_phase(&mut self, phase_name: &str) {
        let Some(phase) = self
            .policy
            .phases
            .iter()
            .find(|phase| phase.name == phase_name)
        else {
            self.diagnostics.push(
                PlanningDiagnostic::error(
                    PlanningDiagnosticCode::InvalidPlanningRequest,
                    format!("phase {phase_name} is listed in phase_order but is missing"),
                )
                .with_phase(phase_name),
            );
            return;
        };

        self.expand_phase(
            &phase.name,
            phase.run_if.as_ref(),
            phase.skip_if.as_ref(),
            &phase.operations,
        );
    }

    fn expand_phase(
        &mut self,
        phase_name: &str,
        run_if: Option<&CompiledCondition>,
        skip_if: Option<&CompiledCondition>,
        operations: &[CompiledOperation],
    ) {
        match (run_if, skip_if) {
            (None, None) => {
                for snapshot in &self.input.media_snapshots {
                    self.expand_operations_for_snapshot(phase_name, snapshot, operations);
                }
            }
            _ => {
                for snapshot in &self.input.media_snapshots {
                    let should_run = run_if.map_or(ConditionEval::Matched, |condition| {
                        evaluate_condition(condition, snapshot)
                    });
                    let should_skip = skip_if.map_or(ConditionEval::NotMatched, |condition| {
                        evaluate_condition(condition, snapshot)
                    });
                    match (should_run, should_skip) {
                        (ConditionEval::Matched, ConditionEval::NotMatched) => {
                            self.expand_operations_for_snapshot(phase_name, snapshot, operations);
                        }
                        (ConditionEval::NotMatched, _) | (_, ConditionEval::Matched) => {}
                        (ConditionEval::Unknown, _) | (_, ConditionEval::Unknown) => {
                            self.expand_blocked_insufficient_facts_for_operations(
                                phase_name, snapshot, operations,
                            );
                        }
                    }
                }
            }
        }
    }

    fn expand_blocked_insufficient_facts_for_operations(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operations: &[CompiledOperation],
    ) {
        for operation in operations {
            self.expand_blocked_insufficient_facts_for_snapshot(phase_name, snapshot, operation);
        }
    }

    fn expand_operations_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operations: &[CompiledOperation],
    ) {
        let operations = snapshot_operations(snapshot, operations);
        let mut remux_operations = Vec::new();
        let mut remux_source_index = None;
        let mut items = Vec::new();

        for (source_index, operation) in operations.into_iter().enumerate() {
            match operation {
                SnapshotOperation::Operation(operation) => {
                    if remux::candidate_kind(operation).is_some() {
                        match remux::candidate_support(operation) {
                            remux::CandidateSupport::Supported => {
                                remux_source_index.get_or_insert(source_index);
                                remux_operations.push(operation);
                            }
                            remux::CandidateSupport::Unsupported(message) => {
                                items.push(PhaseItem::BlockedUnsupportedRemux {
                                    source_index,
                                    operation,
                                    message,
                                });
                            }
                        }
                    } else {
                        items.push(PhaseItem::Operation {
                            source_index,
                            operation,
                        });
                    }
                }
                SnapshotOperation::BlockedInsufficient(operation) => {
                    items.push(PhaseItem::BlockedInsufficient {
                        source_index,
                        operation,
                    });
                }
            }
        }

        if let Some(source_index) = remux_source_index {
            items.push(PhaseItem::RemuxGroup {
                source_index,
                operations: remux_operations,
            });
        }

        items.sort_by_key(PhaseItem::source_index);

        for item in items {
            match item {
                PhaseItem::Operation { operation, .. } => {
                    self.expand_operation_for_snapshot(phase_name, snapshot, operation);
                }
                PhaseItem::BlockedInsufficient { operation, .. } => {
                    self.expand_blocked_insufficient_facts_for_snapshot(
                        phase_name, snapshot, operation,
                    );
                }
                PhaseItem::BlockedUnsupportedRemux {
                    operation, message, ..
                } => {
                    let operation_kind = operation_kind(operation);
                    self.push_operation_plan(
                        phase_name,
                        snapshot,
                        remux::plan_blocked_candidate(
                            phase_name,
                            snapshot,
                            operation_kind,
                            operation_payload(operation),
                            message,
                        ),
                    );
                }
                PhaseItem::RemuxGroup { operations, .. } => {
                    self.push_operation_plan(
                        phase_name,
                        snapshot,
                        remux::plan_group(phase_name, snapshot, &operations),
                    );
                }
            }
        }
    }

    fn expand_operation_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operation: &CompiledOperation,
    ) {
        match operation {
            CompiledOperation::SetContainer { container } => {
                self.push_operation_plan(
                    phase_name,
                    snapshot,
                    remux::plan_set_container(phase_name, snapshot, container),
                );
            }
            CompiledOperation::TranscodeVideo {
                container,
                resolved_profile,
                ..
            } => {
                let Some(resolved) = resolved_profile.as_ref() else {
                    // Resolution invariant (pinned Phase 5↔6 contract): the control
                    // plane fills `resolved_profile` in-memory before planning. A
                    // `None` here means resolution was skipped — a hard internal
                    // error, never a silent no-op.
                    self.record_fatal_diagnostic(transcode_video::missing_resolution_diagnostic(
                        phase_name, snapshot,
                    ));
                    return;
                };
                match transcode_video::plan(phase_name, snapshot, resolved, container) {
                    Ok(plan) => self.push_operation_plan(phase_name, snapshot, plan),
                    Err(error) => self.record_fatal(error),
                }
            }
            CompiledOperation::TranscodeAudio {
                target_codec,
                container,
                filter,
            } => self.push_operation_plan(
                phase_name,
                snapshot,
                audio::plan_transcode(
                    phase_name,
                    snapshot,
                    target_codec,
                    container,
                    filter.as_ref(),
                ),
            ),
            CompiledOperation::ExtractAudio {
                target_codec,
                container,
                filter,
            } => self.push_operation_plan(
                phase_name,
                snapshot,
                audio::plan_extract(
                    phase_name,
                    snapshot,
                    target_codec,
                    container,
                    filter.as_ref(),
                ),
            ),
            CompiledOperation::SynthesizeAudio {
                target_codec,
                container,
                target_channels,
                filter,
            } => self.push_operation_plan(
                phase_name,
                snapshot,
                audio::plan_synthesize(
                    phase_name,
                    snapshot,
                    target_codec,
                    container,
                    *target_channels,
                    filter.as_ref(),
                ),
            ),
            CompiledOperation::VerifyArtifact => {
                self.push_operation_plan(phase_name, snapshot, plan_verify_artifact(snapshot));
            }
            unsupported => {
                let operation_kind = operation_kind(unsupported);
                self.expand_blocked_unsupported_for_snapshot(
                    phase_name,
                    snapshot,
                    operation_kind,
                    operation_payload(unsupported),
                    "operation is not supported by Sprint 5 planner",
                );
            }
        }
    }

    fn push_operation_plan(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        mut plan: OperationPlan,
    ) {
        let mut node = make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            plan.operation_kind,
            plan.operation_payload,
            plan.observed_state,
            plan.status,
            plan.status_reason,
            plan.capability,
        );
        node.resource_estimates = plan.resource_estimates;
        self.diagnostics.append(&mut plan.diagnostics);
        self.nodes.push(node);
    }

    fn record_fatal_diagnostic(&mut self, diagnostic: PlanningDiagnostic) {
        match &mut self.fatal {
            Some(fatal) => fatal.diagnostics.push(diagnostic),
            None => {
                self.fatal = Some(PlanGenerationError {
                    diagnostics: vec![diagnostic],
                });
            }
        }
    }

    fn record_fatal(&mut self, mut error: PlanGenerationError) {
        match &mut self.fatal {
            Some(fatal) => fatal.diagnostics.append(&mut error.diagnostics),
            None => self.fatal = Some(error),
        }
    }

    fn expand_blocked_insufficient_facts_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operation: &CompiledOperation,
    ) {
        let operation_kind = operation_kind(operation);
        let message = "snapshot facts are insufficient to evaluate condition";
        self.diagnostics.push(
            PlanningDiagnostic::error(PlanningDiagnosticCode::InsufficientSnapshotFacts, message)
                .with_phase(phase_name)
                .with_operation_kind(operation_kind.as_str())
                .with_target(snapshot.target.clone()),
        );
        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            operation_kind,
            operation_payload(operation),
            None,
            NodeStatus::Blocked,
            message.to_owned(),
            None,
        ));
    }

    fn expand_blocked_unsupported_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operation_kind: PlanOperationKind,
        payload: serde_json::Value,
        message: &str,
    ) {
        self.diagnostics.push(
            PlanningDiagnostic::error(
                PlanningDiagnosticCode::UnsupportedOperationForSprint5,
                message,
            )
            .with_phase(phase_name)
            .with_operation_kind(operation_kind.as_str())
            .with_target(snapshot.target.clone()),
        );
        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            operation_kind,
            payload,
            None,
            NodeStatus::Blocked,
            message.to_owned(),
            None,
        ));
    }

    fn finish(mut self) -> Result<ExecutionPlan, PlanGenerationError> {
        if let Some(fatal) = self.fatal.take() {
            return Err(fatal);
        }
        let policy = PolicyIdentity {
            slug: self.policy.slug.clone(),
            source_hash: self.policy.source_hash.clone(),
            document_id: self.context.policy_document_id,
            version_id: self.context.policy_version_id,
        };
        let input = InputIdentity {
            slug: Some(self.input.slug.clone()),
            source_label: self.context.input_source_label.clone(),
            input_set_id: self.context.policy_input_set_id,
            fixture_labels: self.input.fixture_labels.clone(),
        };
        let summary = summarize_nodes(&self.nodes);
        let edges = build_phase_edges(self.policy, &self.nodes);
        let provenance = PlanProvenance::default();
        let plan_id = self.plan_id(&policy, &input, &summary, &edges, &provenance)?;

        let mut plan = ExecutionPlan {
            schema_version: self.context.schema_version,
            plan_id,
            plan_hash: String::new(),
            policy,
            input,
            generated_at: self.context.generated_at,
            summary,
            nodes: self.nodes,
            edges,
            warnings: self.warnings,
            diagnostics: self.diagnostics,
            provenance,
        };
        plan.plan_hash = plan_hash(&plan).map_err(|error| serialization_error(&error))?;
        Ok(plan)
    }

    fn plan_id(
        &self,
        policy: &PolicyIdentity,
        input: &InputIdentity,
        summary: &PlanSummary,
        edges: &[Edge],
        provenance: &PlanProvenance,
    ) -> Result<String, PlanGenerationError> {
        let preimage = json!({
            "schema_version": self.context.schema_version,
            "policy": policy,
            "input": input,
            "summary": summary,
            "nodes": self.nodes,
            "edges": edges,
            "warnings": self.warnings,
            "diagnostics": self.diagnostics,
            "provenance": provenance,
        });
        plan_id(&preimage).map_err(|error| serialization_error(&error))
    }
}

enum SnapshotOperation<'a> {
    Operation(&'a CompiledOperation),
    BlockedInsufficient(&'a CompiledOperation),
}

enum PhaseItem<'a> {
    Operation {
        source_index: usize,
        operation: &'a CompiledOperation,
    },
    BlockedInsufficient {
        source_index: usize,
        operation: &'a CompiledOperation,
    },
    BlockedUnsupportedRemux {
        source_index: usize,
        operation: &'a CompiledOperation,
        message: &'static str,
    },
    RemuxGroup {
        source_index: usize,
        operations: Vec<&'a CompiledOperation>,
    },
}

impl PhaseItem<'_> {
    const fn source_index(&self) -> usize {
        match self {
            Self::Operation { source_index, .. }
            | Self::BlockedInsufficient { source_index, .. }
            | Self::BlockedUnsupportedRemux { source_index, .. }
            | Self::RemuxGroup { source_index, .. } => *source_index,
        }
    }
}

fn snapshot_operations<'a>(
    snapshot: &MediaSnapshotInput,
    operations: &'a [CompiledOperation],
) -> Vec<SnapshotOperation<'a>> {
    let mut flattened = Vec::new();
    append_snapshot_operations(snapshot, operations, &mut flattened);
    flattened
}

fn append_snapshot_operations<'a>(
    snapshot: &MediaSnapshotInput,
    operations: &'a [CompiledOperation],
    flattened: &mut Vec<SnapshotOperation<'a>>,
) {
    for operation in operations {
        match operation {
            CompiledOperation::Conditional {
                condition,
                operations,
            } => match evaluate_condition(condition, snapshot) {
                ConditionEval::Matched => {
                    append_snapshot_operations(snapshot, operations, flattened);
                }
                ConditionEval::NotMatched => {}
                ConditionEval::Unknown => {
                    append_blocked_insufficient_operations(operations, flattened);
                }
            },
            CompiledOperation::Rules { mode, rules } => {
                append_rule_operations(snapshot, *mode, rules, flattened);
            }
            operation => flattened.push(SnapshotOperation::Operation(operation)),
        }
    }
}

fn append_rule_operations<'a>(
    snapshot: &MediaSnapshotInput,
    mode: RuleMatchMode,
    rules: &'a [CompiledRule],
    flattened: &mut Vec<SnapshotOperation<'a>>,
) {
    match mode {
        RuleMatchMode::First => {
            for rule in rules {
                match rule_condition_matches(rule, snapshot) {
                    ConditionEval::Matched => {
                        append_snapshot_operations(snapshot, &rule.operations, flattened);
                        break;
                    }
                    ConditionEval::NotMatched => {}
                    ConditionEval::Unknown => {
                        append_blocked_insufficient_operations(&rule.operations, flattened);
                        break;
                    }
                }
            }
        }
        RuleMatchMode::All => {
            for rule in rules {
                match rule_condition_matches(rule, snapshot) {
                    ConditionEval::Matched => {
                        append_snapshot_operations(snapshot, &rule.operations, flattened);
                    }
                    ConditionEval::NotMatched => {}
                    ConditionEval::Unknown => {
                        append_blocked_insufficient_operations(&rule.operations, flattened);
                    }
                }
            }
        }
    }
}

fn append_blocked_insufficient_operations<'a>(
    operations: &'a [CompiledOperation],
    flattened: &mut Vec<SnapshotOperation<'a>>,
) {
    for operation in operations {
        flattened.push(SnapshotOperation::BlockedInsufficient(operation));
    }
}

fn video_stream_count(snapshot: &MediaSnapshotInput) -> Option<u64> {
    snapshot
        .stream_summary
        .get("video_stream_count")
        .and_then(serde_json::Value::as_u64)
}

fn policy_warnings(policy: &CompiledPolicy) -> Vec<String> {
    policy
        .warnings
        .iter()
        .filter(|warning| warning.severity == DiagnosticSeverity::Warning)
        .map(policy_warning)
        .collect()
}

fn policy_warning(warning: &PolicyDiagnostic) -> String {
    format!("policy:{}:{}", warning.code, warning.message)
}

fn rule_condition_matches(rule: &CompiledRule, snapshot: &MediaSnapshotInput) -> ConditionEval {
    rule.condition
        .as_ref()
        .map_or(ConditionEval::Matched, |condition| {
            evaluate_condition(condition, snapshot)
        })
}

fn operation_payload(operation: &CompiledOperation) -> serde_json::Value {
    let operation_kind = operation_kind(operation);
    serde_json::to_value(operation).unwrap_or_else(|_| {
        json!({
            "type": operation_kind,
        })
    })
}

fn evaluate_condition(
    condition: &CompiledCondition,
    snapshot: &MediaSnapshotInput,
) -> ConditionEval {
    match condition {
        CompiledCondition::FieldComparison { path, op, value } => {
            evaluate_field_comparison(path, *op, value, snapshot)
        }
        CompiledCondition::FieldExists { path } => {
            ConditionEval::from_bool(snapshot_field(path, snapshot).is_some())
        }
        CompiledCondition::Not { inner } => evaluate_condition(inner, snapshot).negate(),
        CompiledCondition::And { conditions } => {
            let mut saw_unknown = false;
            for condition in conditions {
                match evaluate_condition(condition, snapshot) {
                    ConditionEval::NotMatched => return ConditionEval::NotMatched,
                    ConditionEval::Matched => {}
                    ConditionEval::Unknown => saw_unknown = true,
                }
            }
            if saw_unknown {
                ConditionEval::Unknown
            } else {
                ConditionEval::Matched
            }
        }
        CompiledCondition::Or { conditions } => {
            let mut saw_unknown = false;
            for condition in conditions {
                match evaluate_condition(condition, snapshot) {
                    ConditionEval::Matched => return ConditionEval::Matched,
                    ConditionEval::NotMatched => {}
                    ConditionEval::Unknown => saw_unknown = true,
                }
            }
            if saw_unknown {
                ConditionEval::Unknown
            } else {
                ConditionEval::NotMatched
            }
        }
        CompiledCondition::Exists { .. }
        | CompiledCondition::Count { .. }
        | CompiledCondition::Predicate { .. } => ConditionEval::Unknown,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConditionEval {
    Matched,
    NotMatched,
    Unknown,
}

impl ConditionEval {
    const fn from_bool(value: bool) -> Self {
        if value {
            Self::Matched
        } else {
            Self::NotMatched
        }
    }

    const fn negate(self) -> Self {
        match self {
            Self::Matched => Self::NotMatched,
            Self::NotMatched => Self::Matched,
            Self::Unknown => Self::Unknown,
        }
    }
}

fn evaluate_field_comparison(
    path: &[String],
    op: ComparisonOp,
    value: &CompiledValue,
    snapshot: &MediaSnapshotInput,
) -> ConditionEval {
    let Some(left) = snapshot_field(path, snapshot) else {
        return ConditionEval::Unknown;
    };
    let Some(right) = condition_value(value, snapshot) else {
        return ConditionEval::Unknown;
    };
    compare_snapshot_values(&left, op, &right)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotFieldValue<'a> {
    String(&'a str),
    Number(u64),
    Boolean(bool),
}

fn snapshot_field<'a>(
    path: &[String],
    snapshot: &'a MediaSnapshotInput,
) -> Option<SnapshotFieldValue<'a>> {
    let canonical = canonical_field_path(path)?;
    match canonical {
        "container" => snapshot
            .container
            .as_deref()
            .map(SnapshotFieldValue::String),
        "video_codec" => snapshot
            .video_codec
            .as_deref()
            .map(SnapshotFieldValue::String),
        "width" => snapshot
            .width
            .map(|value| SnapshotFieldValue::Number(u64::from(value))),
        "height" => snapshot
            .height
            .map(|value| SnapshotFieldValue::Number(u64::from(value))),
        "hdr" => snapshot.hdr.as_deref().map(SnapshotFieldValue::String),
        "bitrate" => snapshot.bitrate.map(SnapshotFieldValue::Number),
        "duration_millis" => snapshot.duration_millis.map(SnapshotFieldValue::Number),
        _ => None,
    }
}

fn canonical_field_path(path: &[String]) -> Option<&'static str> {
    match path {
        [field] => match field.as_str() {
            "container" => Some("container"),
            "video_codec" => Some("video_codec"),
            "width" => Some("width"),
            "height" => Some("height"),
            "hdr" => Some("hdr"),
            "bitrate" => Some("bitrate"),
            "duration_millis" => Some("duration_millis"),
            _ => None,
        },
        [scope, field] if scope == "video" => match field.as_str() {
            "codec" => Some("video_codec"),
            "width" => Some("width"),
            "height" => Some("height"),
            "hdr" => Some("hdr"),
            "bitrate" => Some("bitrate"),
            "duration_millis" => Some("duration_millis"),
            _ => None,
        },
        [scope, field] if scope == "media" => match field.as_str() {
            "container" => Some("container"),
            "duration_millis" => Some("duration_millis"),
            _ => None,
        },
        [scope, field] if scope == "container" => match field.as_str() {
            "name" | "value" => Some("container"),
            _ => None,
        },
        _ => None,
    }
}

fn condition_value<'a>(
    value: &'a CompiledValue,
    snapshot: &'a MediaSnapshotInput,
) -> Option<SnapshotFieldValue<'a>> {
    match value {
        CompiledValue::String { value } => Some(SnapshotFieldValue::String(value)),
        CompiledValue::Number { value } => {
            value.parse::<u64>().ok().map(SnapshotFieldValue::Number)
        }
        CompiledValue::Boolean { value } => Some(SnapshotFieldValue::Boolean(*value)),
        CompiledValue::FieldPath { path } => snapshot_field(path, snapshot),
        CompiledValue::List { .. } => None,
    }
}

fn compare_snapshot_values(
    left: &SnapshotFieldValue<'_>,
    op: ComparisonOp,
    right: &SnapshotFieldValue<'_>,
) -> ConditionEval {
    match (left, right) {
        (SnapshotFieldValue::String(left), SnapshotFieldValue::String(right)) => {
            compare_strings(left, op, right)
        }
        (SnapshotFieldValue::Number(left), SnapshotFieldValue::Number(right)) => {
            compare_numbers(*left, op, *right)
        }
        (SnapshotFieldValue::Boolean(left), SnapshotFieldValue::Boolean(right)) => {
            compare_booleans(*left, op, *right)
        }
        _ => match op {
            ComparisonOp::Eq => ConditionEval::NotMatched,
            ComparisonOp::Ne => ConditionEval::Matched,
            ComparisonOp::Lt
            | ComparisonOp::Lte
            | ComparisonOp::Gt
            | ComparisonOp::Gte
            | ComparisonOp::Contains
            | ComparisonOp::Matches => ConditionEval::Unknown,
        },
    }
}

fn compare_strings(left: &str, op: ComparisonOp, right: &str) -> ConditionEval {
    match op {
        ComparisonOp::Eq => ConditionEval::from_bool(left == right),
        ComparisonOp::Ne => ConditionEval::from_bool(left != right),
        ComparisonOp::Contains => ConditionEval::from_bool(left.contains(right)),
        ComparisonOp::Lt
        | ComparisonOp::Lte
        | ComparisonOp::Gt
        | ComparisonOp::Gte
        | ComparisonOp::Matches => ConditionEval::Unknown,
    }
}

fn compare_numbers(left: u64, op: ComparisonOp, right: u64) -> ConditionEval {
    match op {
        ComparisonOp::Eq => ConditionEval::from_bool(left == right),
        ComparisonOp::Ne => ConditionEval::from_bool(left != right),
        ComparisonOp::Lt => ConditionEval::from_bool(left < right),
        ComparisonOp::Lte => ConditionEval::from_bool(left <= right),
        ComparisonOp::Gt => ConditionEval::from_bool(left > right),
        ComparisonOp::Gte => ConditionEval::from_bool(left >= right),
        ComparisonOp::Contains | ComparisonOp::Matches => ConditionEval::Unknown,
    }
}

fn compare_booleans(left: bool, op: ComparisonOp, right: bool) -> ConditionEval {
    match op {
        ComparisonOp::Eq => ConditionEval::from_bool(left == right),
        ComparisonOp::Ne => ConditionEval::from_bool(left != right),
        ComparisonOp::Lt
        | ComparisonOp::Lte
        | ComparisonOp::Gt
        | ComparisonOp::Gte
        | ComparisonOp::Contains
        | ComparisonOp::Matches => ConditionEval::Unknown,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Task 4 specifies this planner helper signature"
)]
fn make_node(
    phase_name: &str,
    ordinal: u32,
    snapshot: &MediaSnapshotInput,
    operation_kind: PlanOperationKind,
    operation_payload: serde_json::Value,
    observed_state: Option<serde_json::Value>,
    status: NodeStatus,
    status_reason: String,
    capability: Option<String>,
) -> PlanNode {
    let target_key = target_key(&snapshot.target);
    let scheduling_hints = SchedulingHints {
        concurrency_key: Some(target_key.clone()),
        ..SchedulingHints::default()
    };
    PlanNode {
        node_id: node_id(phase_name, ordinal, operation_kind.as_str(), &target_key),
        phase_name: phase_name.to_owned(),
        ordinal,
        target: snapshot.target.clone(),
        operation_kind,
        operation_payload,
        observed_state,
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

fn summarize_nodes(nodes: &[PlanNode]) -> PlanSummary {
    let mut summary = PlanSummary {
        total_node_count: checked_ordinal(nodes.len()),
        ..PlanSummary::default()
    };
    let mut target_keys = BTreeSet::new();

    for node in nodes {
        match node.status {
            NodeStatus::Planned => summary.executable_node_count += 1,
            NodeStatus::NoOp => summary.no_op_node_count += 1,
            NodeStatus::Blocked => summary.blocked_node_count += 1,
        }
        target_keys.insert(target_key(&node.target));
        *summary
            .operation_counts_by_kind
            .entry(node.operation_kind)
            .or_insert(0) += 1;
    }

    summary.target_count = checked_ordinal(target_keys.len());
    summary
}

fn build_phase_edges(policy: &CompiledPolicy, nodes: &[PlanNode]) -> Vec<Edge> {
    let phases_by_name: BTreeMap<&str, _> = policy
        .phases
        .iter()
        .map(|phase| (phase.name.as_str(), phase))
        .collect();
    let mut nodes_by_phase: BTreeMap<&str, Vec<&PlanNode>> = BTreeMap::new();

    for node in nodes {
        nodes_by_phase
            .entry(node.phase_name.as_str())
            .or_default()
            .push(node);
    }

    let mut edges = Vec::new();
    for phase_name in &policy.phase_order {
        let Some(phase) = phases_by_name.get(phase_name.as_str()) else {
            continue;
        };
        let Some(to_nodes) = nodes_by_phase.get(phase.name.as_str()) else {
            continue;
        };

        for dependency_name in &phase.depends_on {
            let Some(from_nodes) = nodes_by_phase.get(dependency_name.as_str()) else {
                continue;
            };
            for from_node in from_nodes {
                for to_node in to_nodes {
                    edges.push(Edge {
                        edge_id: edge_id(&from_node.node_id, &to_node.node_id, "phase_depends_on"),
                        from_node_id: from_node.node_id.clone(),
                        to_node_id: to_node.node_id.clone(),
                        dependency_kind: DependencyKind::PhaseDependsOn,
                    });
                }
            }
        }
    }

    edges
}

/// Plan the `verify artifact` operation. Verification targets the artifact a
/// prior phase produces, not the source snapshot's streams, so the node is
/// always planned (never a snapshot-shape no-op or block) and routes to the
/// `verify_artifact` worker capability. The concrete artifact facts the worker
/// checks against are assembled downstream from the committed artifact, not at
/// plan time; the payload only pins the operation and the source snapshot id
/// when the caller already knows it.
fn plan_verify_artifact(snapshot: &MediaSnapshotInput) -> OperationPlan {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "type".to_owned(),
        json!(PlanOperationKind::VerifyArtifact.as_str()),
    );
    if let Some(id) = snapshot.existing_media_snapshot_id {
        payload.insert("source_media_snapshot_id".to_owned(), json!(id.0));
    }
    OperationPlan::new(
        PlanOperationKind::VerifyArtifact,
        serde_json::Value::Object(payload),
        None,
        NodeStatus::Planned,
        "artifact will be verified against its expected facts".to_owned(),
        Some(PlanOperationKind::VerifyArtifact.as_str().to_owned()),
    )
}

fn operation_kind(operation: &CompiledOperation) -> PlanOperationKind {
    match operation {
        CompiledOperation::SetContainer { .. } => PlanOperationKind::SetContainer,
        CompiledOperation::KeepTracks { .. } => PlanOperationKind::KeepTracks,
        CompiledOperation::RemoveTracks { .. } => PlanOperationKind::RemoveTracks,
        CompiledOperation::ReorderTracks { .. } => PlanOperationKind::ReorderTracks,
        CompiledOperation::SetDefaults { .. } => PlanOperationKind::SetDefaults,
        CompiledOperation::ClearTrackActions { .. } => PlanOperationKind::ClearTrackActions,
        CompiledOperation::ClearTags => PlanOperationKind::ClearTags,
        CompiledOperation::SetTag { .. } => PlanOperationKind::SetTag,
        CompiledOperation::DeleteTag { .. } => PlanOperationKind::DeleteTag,
        CompiledOperation::TranscodeVideo { .. } => PlanOperationKind::TranscodeVideo,
        // Synthesis rides the transcode_audio operation kind end-to-end (ADR 0026,
        // Option B) so no new voom_core OperationKind or control-plane routing is
        // needed; the plan payload's `type` distinguishes the add-track mode.
        CompiledOperation::TranscodeAudio { .. } | CompiledOperation::SynthesizeAudio { .. } => {
            PlanOperationKind::TranscodeAudio
        }
        CompiledOperation::ExtractAudio { .. } => PlanOperationKind::ExtractAudio,
        CompiledOperation::VerifyArtifact => PlanOperationKind::VerifyArtifact,
        CompiledOperation::Conditional { .. } => PlanOperationKind::Conditional,
        CompiledOperation::Rules { .. } => PlanOperationKind::Rules,
    }
}

fn target_key(target: &TargetRef) -> String {
    match target {
        TargetRef::MediaWork { id } => format!("media_work:{id}"),
        TargetRef::MediaVariant { id } => format!("media_variant:{id}"),
        TargetRef::AssetBundle { id } => format!("asset_bundle:{id}"),
        TargetRef::FileAsset { id } => format!("file_asset:{id}"),
        TargetRef::FileVersion { id } => format!("file_version:{id}"),
        TargetRef::FileLocation { id } => format!("file_location:{id}"),
        TargetRef::Synthetic { key, kind } => format!("synthetic:{}:{key}", kind.as_str()),
    }
}

fn checked_ordinal(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn invalid_phase_request(phase_name: &str, message: String) -> PlanGenerationError {
    PlanGenerationError {
        diagnostics: vec![
            PlanningDiagnostic::error(PlanningDiagnosticCode::InvalidPlanningRequest, message)
                .with_phase(phase_name),
        ],
    }
}

fn serialization_error(error: &serde_json::Error) -> PlanGenerationError {
    PlanGenerationError {
        diagnostics: vec![PlanningDiagnostic::error(
            PlanningDiagnosticCode::DeterministicSerializationFailure,
            format!("planner deterministic serialization failed: {error}"),
        )],
    }
}

#[cfg(test)]
#[path = "planner_test.rs"]
mod tests;
