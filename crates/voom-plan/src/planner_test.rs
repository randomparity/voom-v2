use std::collections::BTreeMap;

use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy,
    CompiledValue, DiagnosticCode, DiagnosticStage, MediaSnapshotInput, PolicyDiagnostic,
    PolicyInputSetDraft, PolicyInputSourceKind, SourceLocation, SourceSpan, TargetKind, TargetRef,
    TrackTarget,
};

use crate::{DependencyKind, NodeStatus, PlanningContext, PlanningRequest, generate_plan};

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
    assert_eq!(
        plan.nodes[0].status_reason,
        "container mp4 will be changed to mkv"
    );
    assert_eq!(
        plan.nodes[0]
            .capability_hints
            .operation_capability
            .as_deref(),
        Some("remux_container")
    );
    assert_eq!(
        plan.nodes[0].operation_payload,
        serde_json::json!({"container": "mkv"})
    );
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
    assert_eq!(plan.nodes[0].status_reason, "container is already mkv");
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
    assert_eq!(plan.nodes[0].status_reason, "snapshot container is unknown");
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
    assert_eq!(plan.diagnostics[0].message, "snapshot container is unknown");
}

#[test]
fn policy_warnings_are_visible_in_plan_output() {
    let mut compiled = policy(CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    });
    compiled.warnings.push(PolicyDiagnostic::warning(
        DiagnosticCode::MetadataRequiresToolsDeferred,
        DiagnosticStage::Validate,
        SourceSpan::new(7, 21),
        SourceLocation { line: 1, column: 8 },
        "metadata requires_tools is deferred",
    ));

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(
        plan.warnings,
        vec![
            "policy:metadata_requires_tools_deferred:metadata requires_tools is deferred"
                .to_owned()
        ]
    );
}

#[test]
fn plan_id_includes_schema_version_identity() {
    let request = |schema_version| PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(Some("mp4")),
        context: PlanningContext {
            schema_version,
            ..PlanningContext::default()
        },
    };

    let schema_one = generate_plan(request(1)).unwrap();
    let schema_two = generate_plan(request(2)).unwrap();

    assert_ne!(schema_one.plan_id, schema_two.plan_id);
    assert_ne!(schema_one.plan_hash, schema_two.plan_hash);
}

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
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
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
    assert_eq!(
        plan.edges[0].dependency_kind,
        DependencyKind::PhaseDependsOn
    );
    assert_eq!(plan.edges[0].from_node_id, plan.nodes[0].node_id);
    assert_eq!(plan.edges[0].to_node_id, plan.nodes[1].node_id);
}

#[test]
fn container_name_condition_selects_resolved_branch() {
    let plan_for = |container| {
        generate_plan(PlanningRequest {
            policy: policy(CompiledOperation::Conditional {
                condition: CompiledCondition::FieldComparison {
                    path: vec!["container".to_owned(), "name".to_owned()],
                    op: ComparisonOp::Eq,
                    value: CompiledValue::String {
                        value: "mp4".to_owned(),
                    },
                },
                operations: vec![CompiledOperation::SetContainer {
                    container: "mkv".to_owned(),
                }],
            }),
            input: input(Some(container)),
            context: PlanningContext::default(),
        })
        .unwrap()
    };

    let matching = plan_for("mp4");
    assert_eq!(matching.nodes.len(), 1);
    assert_eq!(matching.nodes[0].status, NodeStatus::Planned);

    let non_matching = plan_for("mkv");
    assert!(non_matching.nodes.is_empty());
    assert!(non_matching.diagnostics.is_empty());
}
