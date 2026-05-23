use std::collections::BTreeMap;

use voom_policy::{
    CompiledOperation, CompiledPhase, CompiledPolicy, MediaSnapshotInput, PolicyInputSetDraft,
    PolicyInputSourceKind, TargetKind, TargetRef,
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
