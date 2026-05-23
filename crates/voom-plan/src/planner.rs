use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPolicy, CompiledRule,
    CompiledValue, DiagnosticSeverity, MediaSnapshotInput, PolicyDiagnostic, PolicyInputSetDraft,
    RuleMatchMode,
};

use crate::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, ExecutionPlan, InputIdentity,
    NodeStatus, PlanNode, PlanProvenance, PlanSummary, PlanningContext, PlanningDiagnostic,
    PlanningDiagnosticCode, PlanningRequest, PolicyIdentity, ResourceEstimates, SafetyHints,
    SchedulingHints, TargetRef, edge_id, node_id, plan_hash, plan_id,
};

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

        let phases_by_name: BTreeMap<&str, _> = self
            .policy
            .phases
            .iter()
            .map(|phase| (phase.name.as_str(), phase))
            .collect();

        for phase_name in &self.policy.phase_order {
            let Some(phase) = phases_by_name.get(phase_name.as_str()).copied() else {
                self.diagnostics.push(
                    PlanningDiagnostic::error(
                        PlanningDiagnosticCode::InvalidPlanningRequest,
                        format!("phase {phase_name} is listed in phase_order but is missing"),
                    )
                    .with_phase(phase_name),
                );
                continue;
            };

            self.expand_phase(
                &phase.name,
                phase.run_if.as_ref(),
                phase.skip_if.as_ref(),
                &phase.operations,
            );
        }
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
                for operation in operations {
                    self.expand_operation_for_all_snapshots(phase_name, operation);
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

    fn expand_operation_for_all_snapshots(
        &mut self,
        phase_name: &str,
        operation: &CompiledOperation,
    ) {
        for snapshot in &self.input.media_snapshots {
            self.expand_operation_for_snapshot(phase_name, snapshot, operation);
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
        for operation in operations {
            self.expand_operation_for_snapshot(phase_name, snapshot, operation);
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
                self.expand_set_container_for_snapshot(phase_name, snapshot, container);
            }
            CompiledOperation::Conditional {
                condition,
                operations,
            } => match evaluate_condition(condition, snapshot) {
                ConditionEval::Matched => {
                    self.expand_operations_for_snapshot(phase_name, snapshot, operations);
                }
                ConditionEval::NotMatched => {}
                ConditionEval::Unknown => self.expand_blocked_insufficient_facts_for_operations(
                    phase_name, snapshot, operations,
                ),
            },
            CompiledOperation::Rules { mode, rules } => {
                self.expand_rules_for_snapshot(phase_name, snapshot, *mode, rules);
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

    fn expand_rules_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        mode: RuleMatchMode,
        rules: &[CompiledRule],
    ) {
        match mode {
            RuleMatchMode::First => {
                for rule in rules {
                    match rule_condition_matches(rule, snapshot) {
                        ConditionEval::Matched => {
                            self.expand_operations_for_snapshot(
                                phase_name,
                                snapshot,
                                &rule.operations,
                            );
                            break;
                        }
                        ConditionEval::NotMatched => {}
                        ConditionEval::Unknown => {
                            self.expand_blocked_insufficient_facts_for_operations(
                                phase_name,
                                snapshot,
                                &rule.operations,
                            );
                            break;
                        }
                    }
                }
            }
            RuleMatchMode::All => {
                for rule in rules {
                    match rule_condition_matches(rule, snapshot) {
                        ConditionEval::Matched => {
                            self.expand_operations_for_snapshot(
                                phase_name,
                                snapshot,
                                &rule.operations,
                            );
                        }
                        ConditionEval::NotMatched => {}
                        ConditionEval::Unknown => {
                            self.expand_blocked_insufficient_facts_for_operations(
                                phase_name,
                                snapshot,
                                &rule.operations,
                            );
                        }
                    }
                }
            }
        }
    }

    fn expand_set_container_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        container: &str,
    ) {
        let ordinal = checked_ordinal(self.nodes.len());
        let (status, status_reason, capability) = match snapshot.container.as_deref() {
            Some(current) if current == container => (
                NodeStatus::NoOp,
                format!("container is already {container}"),
                None,
            ),
            Some(current) => (
                NodeStatus::Planned,
                format!("container {current} will be changed to {container}"),
                Some("remux_container".to_owned()),
            ),
            None => {
                let message = "snapshot container is unknown";
                self.diagnostics.push(
                    PlanningDiagnostic::error(
                        PlanningDiagnosticCode::InsufficientSnapshotFacts,
                        message,
                    )
                    .with_phase(phase_name)
                    .with_operation_kind("set_container")
                    .with_target(snapshot.target.clone()),
                );
                (NodeStatus::Blocked, message.to_owned(), None)
            }
        };

        self.nodes.push(make_node(
            phase_name,
            ordinal,
            snapshot,
            "set_container",
            json!({ "container": container }),
            status,
            status_reason,
            capability,
        ));
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
                .with_operation_kind(operation_kind)
                .with_target(snapshot.target.clone()),
        );
        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            operation_kind,
            operation_payload(operation),
            NodeStatus::Blocked,
            message.to_owned(),
            None,
        ));
    }

    fn expand_blocked_unsupported_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operation_kind: &str,
        payload: serde_json::Value,
        message: &str,
    ) {
        self.diagnostics.push(
            PlanningDiagnostic::error(
                PlanningDiagnosticCode::UnsupportedOperationForSprint5,
                message,
            )
            .with_phase(phase_name)
            .with_operation_kind(operation_kind)
            .with_target(snapshot.target.clone()),
        );
        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            operation_kind,
            payload,
            NodeStatus::Blocked,
            message.to_owned(),
            None,
        ));
    }

    fn finish(self) -> Result<ExecutionPlan, PlanGenerationError> {
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
    operation_kind: &str,
    operation_payload: serde_json::Value,
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
            .entry(node.operation_kind.clone())
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
