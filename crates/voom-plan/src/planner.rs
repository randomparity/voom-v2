use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPolicy, CompiledRule,
    CompiledValue, DefaultStrategy, DiagnosticSeverity, MediaSnapshotInput, PolicyDiagnostic,
    PolicyInputSetDraft, RuleMatchMode, TrackFilter, TrackTarget,
};

use crate::{
    ArtifactExpectations, CapabilityHints, DependencyKind, Edge, ExecutionPlan, InputIdentity,
    NodeStatus, PlanNode, PlanProvenance, PlanSummary, PlanningContext, PlanningDiagnostic,
    PlanningDiagnosticCode, PlanningRequest, PolicyIdentity, ResourceEstimates, SafetyHints,
    SchedulingHints, TargetRef, edge_id, node_id, plan_hash, plan_id,
    remux::{RemuxPlanningBlock, SnapshotStreamFact, evaluate_filter, stream_facts},
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
                    if remux_candidate_kind(operation).is_some() {
                        match remux_candidate_support(operation) {
                            RemuxCandidateSupport::Supported => {
                                remux_source_index.get_or_insert(source_index);
                                remux_operations.push(operation);
                            }
                            RemuxCandidateSupport::Unsupported(message) => {
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
                    self.expand_blocked_remux_shape_for_snapshot(
                        phase_name,
                        snapshot,
                        operation_kind,
                        operation_payload(operation),
                        message,
                    );
                }
                PhaseItem::RemuxGroup { operations, .. } => {
                    self.expand_remux_group_for_snapshot(phase_name, snapshot, &operations);
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
                self.expand_set_container_for_snapshot(phase_name, snapshot, container);
            }
            CompiledOperation::TranscodeVideo {
                target_codec,
                container,
                profile,
            } => self.expand_transcode_video_for_snapshot(
                phase_name,
                snapshot,
                target_codec,
                container,
                profile,
            ),
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

    fn expand_remux_group_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operations: &[&CompiledOperation],
    ) {
        let payload = remux_payload(snapshot, operations);
        let observed_state = snapshot
            .container
            .as_ref()
            .map(|container| json!({ "container": container }));
        let target_container = payload
            .get("container")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("mkv");
        let (status, status_reason, capability) =
            match remux_group_shape(snapshot, operations, target_container) {
                RemuxGroupShape::NoOp => (
                    NodeStatus::NoOp,
                    format!(
                        "container is already {target_container} and track selection is unchanged"
                    ),
                    None,
                ),
                RemuxGroupShape::ContainerChange { current } => (
                    NodeStatus::Planned,
                    format!("container {current} will be changed to {target_container}"),
                    Some("remux_container".to_owned()),
                ),
                RemuxGroupShape::TrackSelectionChange => (
                    NodeStatus::Planned,
                    "track selection will be changed".to_owned(),
                    Some("remux".to_owned()),
                ),
                RemuxGroupShape::InsufficientFacts(message) => {
                    self.push_remux_diagnostic(
                        PlanningDiagnosticCode::InsufficientSnapshotFacts,
                        phase_name,
                        snapshot,
                        message,
                    );
                    (NodeStatus::Blocked, message.to_owned(), None)
                }
                RemuxGroupShape::UnsupportedShape(message) => {
                    self.push_remux_diagnostic(
                        PlanningDiagnosticCode::UnsupportedMediaShape,
                        phase_name,
                        snapshot,
                        message,
                    );
                    (NodeStatus::Blocked, message.to_owned(), None)
                }
            };

        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            "remux",
            payload,
            observed_state,
            status,
            status_reason,
            capability,
        ));
    }

    fn push_remux_diagnostic(
        &mut self,
        code: PlanningDiagnosticCode,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        message: &str,
    ) {
        self.diagnostics.push(
            PlanningDiagnostic::error(code, message)
                .with_phase(phase_name)
                .with_operation_kind("remux")
                .with_target(snapshot.target.clone()),
        );
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
            snapshot
                .container
                .as_ref()
                .map(|container| json!({ "container": container })),
            status,
            status_reason,
            capability,
        ));
    }

    fn expand_transcode_video_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        target_codec: &str,
        container: &str,
        profile: &str,
    ) {
        let payload = json!({
            "type": "transcode_video",
            "target_codec": target_codec,
            "container": container,
            "profile": profile,
        });
        let observed_state = transcode_video_observed_state(snapshot);
        let (status, status_reason, capability) = match transcode_video_shape(snapshot, container) {
            TranscodeVideoShape::Compliant => (
                NodeStatus::NoOp,
                format!("video is already {target_codec} in {container}"),
                None,
            ),
            TranscodeVideoShape::NeedsTranscode => (
                NodeStatus::Planned,
                format!("video will be transcoded to {target_codec} in {container}"),
                Some("transcode_video".to_owned()),
            ),
            TranscodeVideoShape::InsufficientFacts(message) => {
                self.push_transcode_video_diagnostic(
                    PlanningDiagnosticCode::InsufficientSnapshotFacts,
                    phase_name,
                    snapshot,
                    message,
                );
                (NodeStatus::Blocked, message.to_owned(), None)
            }
            TranscodeVideoShape::UnsupportedShape(message) => {
                self.push_transcode_video_diagnostic(
                    PlanningDiagnosticCode::UnsupportedMediaShape,
                    phase_name,
                    snapshot,
                    message,
                );
                (NodeStatus::Blocked, message.to_owned(), None)
            }
        };

        self.nodes.push(make_node(
            phase_name,
            checked_ordinal(self.nodes.len()),
            snapshot,
            "transcode_video",
            payload,
            observed_state,
            status,
            status_reason,
            capability,
        ));
    }

    fn push_transcode_video_diagnostic(
        &mut self,
        code: PlanningDiagnosticCode,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        message: &str,
    ) {
        self.diagnostics.push(
            PlanningDiagnostic::error(code, message)
                .with_phase(phase_name)
                .with_operation_kind("transcode_video")
                .with_target(snapshot.target.clone()),
        );
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
            None,
            NodeStatus::Blocked,
            message.to_owned(),
            None,
        ));
    }

    fn expand_blocked_remux_shape_for_snapshot(
        &mut self,
        phase_name: &str,
        snapshot: &MediaSnapshotInput,
        operation_kind: &str,
        payload: serde_json::Value,
        message: &str,
    ) {
        self.diagnostics.push(
            PlanningDiagnostic::error(PlanningDiagnosticCode::UnsupportedMediaShape, message)
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
            None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TranscodeVideoShape<'a> {
    Compliant,
    NeedsTranscode,
    InsufficientFacts(&'a str),
    UnsupportedShape(&'a str),
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

enum RemuxCandidateSupport {
    Supported,
    Unsupported(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemuxGroupShape {
    NoOp,
    ContainerChange { current: String },
    TrackSelectionChange,
    InsufficientFacts(&'static str),
    UnsupportedShape(&'static str),
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

fn remux_candidate_kind(operation: &CompiledOperation) -> Option<&'static str> {
    match operation {
        CompiledOperation::SetContainer { .. } => Some("set_container"),
        CompiledOperation::KeepTracks { .. } => Some("keep_tracks"),
        CompiledOperation::RemoveTracks { .. } => Some("remove_tracks"),
        CompiledOperation::ReorderTracks { .. } => Some("reorder_tracks"),
        CompiledOperation::SetDefaults { .. } => Some("set_defaults"),
        _ => None,
    }
}

fn remux_candidate_support(operation: &CompiledOperation) -> RemuxCandidateSupport {
    match operation {
        CompiledOperation::SetContainer { container } if container.eq_ignore_ascii_case("mkv") => {
            RemuxCandidateSupport::Supported
        }
        CompiledOperation::SetContainer { .. } => {
            RemuxCandidateSupport::Unsupported("only mkv remux containers are supported")
        }
        CompiledOperation::KeepTracks { target, filter }
        | CompiledOperation::RemoveTracks { target, filter } => {
            if *target == TrackTarget::Video {
                return RemuxCandidateSupport::Unsupported(
                    "video track selection is not supported by remux planning",
                );
            }
            if filter.as_ref().is_some_and(filter_has_unsupported_shape) {
                RemuxCandidateSupport::Unsupported(
                    "track filter is not supported by remux planning",
                )
            } else {
                RemuxCandidateSupport::Supported
            }
        }
        CompiledOperation::SetDefaults {
            strategy: DefaultStrategy::Best,
            ..
        } => RemuxCandidateSupport::Unsupported(
            "default strategy best is not supported by remux planning",
        ),
        CompiledOperation::ReorderTracks { targets } => {
            if duplicate_track_targets(targets) {
                RemuxCandidateSupport::Unsupported("track order contains duplicate target groups")
            } else {
                RemuxCandidateSupport::Supported
            }
        }
        CompiledOperation::SetDefaults { .. } => RemuxCandidateSupport::Supported,
        _ => RemuxCandidateSupport::Unsupported("operation is not supported by remux planning"),
    }
}

fn duplicate_track_targets(targets: &[TrackTarget]) -> bool {
    let mut seen = Vec::new();
    for target in targets {
        if seen.contains(target) {
            return true;
        }
        seen.push(*target);
    }
    false
}

fn filter_has_unsupported_shape(filter: &TrackFilter) -> bool {
    match filter {
        TrackFilter::Commentary | TrackFilter::TitleMatches { .. } => true,
        TrackFilter::Not { inner } => filter_has_unsupported_shape(inner),
        TrackFilter::And { filters } | TrackFilter::Or { filters } => {
            filters.iter().any(filter_has_unsupported_shape)
        }
        TrackFilter::LanguageIn { .. }
        | TrackFilter::CodecIn { .. }
        | TrackFilter::Channels { .. }
        | TrackFilter::Forced
        | TrackFilter::Default
        | TrackFilter::Font
        | TrackFilter::TitleContains { .. } => false,
    }
}

fn remux_payload(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> serde_json::Value {
    let container = operations
        .iter()
        .find_map(|operation| match operation {
            CompiledOperation::SetContainer { container } => Some(container.as_str()),
            _ => None,
        })
        .unwrap_or("mkv");
    let track_actions = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::KeepTracks { target, filter } => Some(track_action_payload(
                "keep_tracks",
                *target,
                filter.as_ref(),
            )),
            CompiledOperation::RemoveTracks { target, filter } => Some(track_action_payload(
                "remove_tracks",
                *target,
                filter.as_ref(),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    let reorder_operations = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::ReorderTracks { targets } => Some(targets),
            _ => None,
        })
        .collect::<Vec<_>>();
    let track_order = match reorder_operations.as_slice() {
        [targets] => targets
            .iter()
            .map(|target| track_target_payload(*target))
            .collect::<Vec<serde_json::Value>>(),
        _ => default_track_order(),
    };
    let defaults = operations
        .iter()
        .filter_map(|operation| match operation {
            CompiledOperation::SetDefaults { target, strategy } => Some(json!({
                "target": track_target_payload(*target),
                "strategy": default_strategy_payload(*strategy),
            })),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut payload = json!({
        "type": "remux",
        "container": container,
        "track_actions": track_actions,
        "track_order": track_order,
        "defaults": defaults,
    });
    if let Some(id) = snapshot.existing_media_snapshot_id
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("source_media_snapshot_id".to_owned(), json!(id.0));
    }
    payload
}

fn track_action_payload(
    action_type: &str,
    target: TrackTarget,
    filter: Option<&TrackFilter>,
) -> serde_json::Value {
    let mut action = serde_json::Map::new();
    action.insert("type".to_owned(), json!(action_type));
    action.insert("target".to_owned(), track_target_payload(target));
    if let Some(filter) = filter {
        action.insert(
            "filter".to_owned(),
            serde_json::to_value(filter).unwrap_or_else(|_| json!({})),
        );
    }
    serde_json::Value::Object(action)
}

fn default_track_order() -> Vec<serde_json::Value> {
    vec![json!("video"), json!("audio"), json!("subtitle")]
}

fn track_target_payload(target: TrackTarget) -> serde_json::Value {
    serde_json::to_value(target).unwrap_or_else(|_| json!("unknown"))
}

fn default_strategy_payload(strategy: DefaultStrategy) -> serde_json::Value {
    serde_json::to_value(strategy).unwrap_or_else(|_| json!("unknown"))
}

fn remux_group_shape(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
    target_container: &str,
) -> RemuxGroupShape {
    let track_selection_changed = match evaluate_remux_track_operations(snapshot, operations) {
        Ok(changed) => changed,
        Err(block) => return remux_block_shape(block),
    };

    let Some(current_container) = snapshot.container.as_deref() else {
        return RemuxGroupShape::InsufficientFacts("snapshot container is unknown");
    };
    if current_container.eq_ignore_ascii_case(target_container) && !track_selection_changed {
        RemuxGroupShape::NoOp
    } else if current_container.eq_ignore_ascii_case(target_container) {
        RemuxGroupShape::TrackSelectionChange
    } else {
        RemuxGroupShape::ContainerChange {
            current: current_container.to_owned(),
        }
    }
}

fn evaluate_remux_track_operations(
    snapshot: &MediaSnapshotInput,
    operations: &[&CompiledOperation],
) -> Result<bool, RemuxPlanningBlock> {
    let has_track_operation = operations
        .iter()
        .any(|operation| !matches!(operation, CompiledOperation::SetContainer { .. }));
    let has_stream_facts = has_remux_stream_fact_shape(snapshot);
    if !has_track_operation && !has_stream_facts {
        return Ok(false);
    }

    let facts = stream_facts(snapshot)?;
    if !facts.iter().any(|stream| stream.kind == TrackTarget::Video) {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    if !has_track_operation {
        return Ok(false);
    }

    let mut changed = false;
    let mut seen_reorder = false;
    for operation in operations {
        match operation {
            CompiledOperation::KeepTracks { target, filter } => {
                changed |= keep_tracks_changes(&facts, *target, filter.as_ref())?;
            }
            CompiledOperation::RemoveTracks { target, filter } => {
                changed |= remove_tracks_changes(&facts, *target, filter.as_ref())?;
            }
            CompiledOperation::SetDefaults { target, strategy } => {
                if !facts.iter().any(|stream| stream.kind == *target)
                    && !matches!(strategy, DefaultStrategy::None | DefaultStrategy::Preserve)
                {
                    return Err(RemuxPlanningBlock::InsufficientSnapshotFacts);
                }
                changed |= set_defaults_changes(&facts, *target, *strategy);
            }
            CompiledOperation::ReorderTracks { targets } => {
                if seen_reorder || targets.is_empty() || duplicate_track_targets(targets) {
                    return Err(RemuxPlanningBlock::UnsupportedMediaShape);
                }
                seen_reorder = true;
                changed |= reorder_tracks_changes(&facts, targets)?;
            }
            CompiledOperation::SetContainer { .. } => {}
            _ => return Err(RemuxPlanningBlock::UnsupportedMediaShape),
        }
    }

    Ok(changed)
}

fn keep_tracks_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    filter: Option<&TrackFilter>,
) -> Result<bool, RemuxPlanningBlock> {
    let Some(filter) = filter else {
        return Ok(false);
    };
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        if !evaluate_filter(filter, stream)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn remove_tracks_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    filter: Option<&TrackFilter>,
) -> Result<bool, RemuxPlanningBlock> {
    let Some(filter) = filter else {
        return Ok(facts.iter().any(|stream| stream.kind == target));
    };
    for stream in facts.iter().filter(|stream| stream.kind == target) {
        if evaluate_filter(filter, stream)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn set_defaults_changes(
    facts: &[SnapshotStreamFact],
    target: TrackTarget,
    strategy: DefaultStrategy,
) -> bool {
    let target_streams = facts
        .iter()
        .filter(|stream| stream.kind == target)
        .collect::<Vec<_>>();
    match strategy {
        DefaultStrategy::First => target_streams
            .iter()
            .min_by_key(|stream| stream.provider_stream_index)
            .is_none_or(|first| {
                !first.is_default
                    || target_streams.iter().any(|stream| {
                        stream.provider_stream_index != first.provider_stream_index
                            && stream.is_default
                    })
            }),
        DefaultStrategy::None => target_streams.iter().any(|stream| stream.is_default),
        DefaultStrategy::Preserve => false,
        DefaultStrategy::Best => true,
    }
}

fn reorder_tracks_changes(
    facts: &[SnapshotStreamFact],
    targets: &[TrackTarget],
) -> Result<bool, RemuxPlanningBlock> {
    if targets.contains(&TrackTarget::Attachment) {
        return Err(RemuxPlanningBlock::UnsupportedMediaShape);
    }
    let mut streams = facts
        .iter()
        .filter(|stream| stream.kind != TrackTarget::Attachment)
        .collect::<Vec<_>>();
    streams.sort_by_key(|stream| stream.provider_stream_index);
    let mut current_order = Vec::new();
    for stream in streams {
        if current_order.last().copied() != Some(stream.kind) {
            current_order.push(stream.kind);
        }
    }
    let requested_present_order = targets
        .iter()
        .copied()
        .filter(|target| current_order.contains(target))
        .collect::<Vec<_>>();
    Ok(current_order != requested_present_order)
}

fn has_remux_stream_fact_shape(snapshot: &MediaSnapshotInput) -> bool {
    let Some(streams) = snapshot
        .stream_summary
        .get("streams")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    streams.iter().all(|stream| {
        stream.as_object().is_some_and(|stream| {
            stream.contains_key("id") && stream.contains_key("index") && stream.contains_key("kind")
        })
    })
}

fn remux_block_shape(block: RemuxPlanningBlock) -> RemuxGroupShape {
    match block {
        RemuxPlanningBlock::InsufficientSnapshotFacts => RemuxGroupShape::InsufficientFacts(
            "snapshot stream facts are insufficient for remux planning",
        ),
        RemuxPlanningBlock::UnsupportedMediaShape => {
            RemuxGroupShape::UnsupportedShape("media shape is not supported by remux planning")
        }
    }
}

fn transcode_video_shape<'a>(
    snapshot: &MediaSnapshotInput,
    target_container: &str,
) -> TranscodeVideoShape<'a> {
    let Some(video_stream_count) = video_stream_count(snapshot) else {
        return TranscodeVideoShape::InsufficientFacts("snapshot video stream count is unknown");
    };
    if video_stream_count != 1 {
        return TranscodeVideoShape::UnsupportedShape(
            "transcode_video requires exactly one video stream",
        );
    }

    let Some(container) = snapshot.container.as_deref() else {
        return TranscodeVideoShape::InsufficientFacts("snapshot container is unknown");
    };
    let Some(video_codec) = snapshot.video_codec.as_deref() else {
        return TranscodeVideoShape::InsufficientFacts("snapshot video codec is unknown");
    };

    if container.eq_ignore_ascii_case(target_container)
        && voom_worker_protocol::is_supported_transcode_video_codec(video_codec)
    {
        TranscodeVideoShape::Compliant
    } else {
        TranscodeVideoShape::NeedsTranscode
    }
}

fn transcode_video_observed_state(snapshot: &MediaSnapshotInput) -> Option<serde_json::Value> {
    let mut observed = serde_json::Map::new();
    if let Some(container) = &snapshot.container {
        observed.insert("container".to_owned(), json!(container));
    }
    if let Some(video_codec) = &snapshot.video_codec {
        observed.insert("video_codec".to_owned(), json!(video_codec));
    }
    if let Some(video_stream_count) = video_stream_count(snapshot) {
        observed.insert("video_stream_count".to_owned(), json!(video_stream_count));
    }
    if observed.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(observed))
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
    operation_kind: &str,
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
        node_id: node_id(phase_name, ordinal, operation_kind, &target_key),
        phase_name: phase_name.to_owned(),
        ordinal,
        target: snapshot.target.clone(),
        operation_kind: operation_kind.to_owned(),
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
        CompiledOperation::TranscodeVideo { .. } => "transcode_video",
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
