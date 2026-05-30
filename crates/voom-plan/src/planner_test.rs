use std::collections::BTreeMap;

use voom_core::MediaSnapshotId;
use voom_policy::{
    ComparisonOp, CompiledCondition, CompiledOperation, CompiledPhase, CompiledPolicy,
    CompiledRule, CompiledValue, DefaultStrategy, DiagnosticCode, DiagnosticStage,
    MediaSnapshotInput, PolicyDiagnostic, PolicyInputSetDraft, PolicyInputSourceKind,
    RuleMatchMode, SourceLocation, SourceSpan, TargetKind, TargetRef, TrackFilter, TrackTarget,
};

use crate::{
    DependencyKind, NodeStatus, PlanGenerationError, PlanningContext, PlanningDiagnosticCode,
    PlanningRequest, generate_plan, plan_phase,
};

fn policy(operation: CompiledOperation) -> CompiledPolicy {
    compiled_policy_with_ops(vec![operation])
}

fn compiled_policy_with_ops(operations: Vec<CompiledOperation>) -> CompiledPolicy {
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
            operations,
        }],
        phase_order: vec!["normalize".to_owned()],
        warnings: Vec::new(),
        provenance: voom_policy::PolicyProvenance::default(),
    }
}

fn compiled_policy_with_phases(phases: &[(&str, Vec<CompiledOperation>)]) -> CompiledPolicy {
    CompiledPolicy {
        policy_name: "container metadata".to_owned(),
        slug: "container-metadata".to_owned(),
        source_hash: "source-hash".to_owned(),
        schema_version: 2,
        metadata: BTreeMap::new(),
        config: BTreeMap::new(),
        phases: phases
            .iter()
            .enumerate()
            .map(|(index, (name, operations))| CompiledPhase {
                name: (*name).to_owned(),
                depends_on: index
                    .checked_sub(1)
                    .map(|previous| phases[previous].0.to_owned())
                    .into_iter()
                    .collect(),
                run_if: None,
                skip_if: None,
                on_error: None,
                operations: operations.clone(),
            })
            .collect(),
        phase_order: phases
            .iter()
            .map(|(name, _operations)| (*name).to_owned())
            .collect(),
        warnings: Vec::new(),
        provenance: voom_policy::PolicyProvenance::default(),
    }
}

fn input(container: Option<&str>) -> PolicyInputSetDraft {
    input_with_snapshot(snapshot_with(container, None, None))
}

fn input_with_snapshot(snapshot: MediaSnapshotInput) -> PolicyInputSetDraft {
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
        media_snapshots: vec![snapshot],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

fn snapshot_with(
    container: Option<&str>,
    video_codec: Option<&str>,
    video_stream_count: Option<u64>,
) -> MediaSnapshotInput {
    MediaSnapshotInput {
        ordinal: 0,
        target: TargetRef::Synthetic {
            key: "variant-1".to_owned(),
            kind: TargetKind::MediaVariant,
        },
        container: container.map(str::to_owned),
        stream_summary: video_stream_count.map_or_else(
            || serde_json::json!({}),
            |count| serde_json::json!({"video_stream_count": count}),
        ),
        video_codec: video_codec.map(str::to_owned),
        width: None,
        height: None,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: None,
    }
}

fn snapshot_with_streams(container: Option<&str>) -> MediaSnapshotInput {
    let mut snapshot = snapshot_with(container, None, Some(1));
    snapshot.stream_summary = serde_json::json!({
        "video_stream_count": 1,
        "streams": [
            {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"},
            {"id": "stream-1", "index": 1, "kind": "audio", "codec_name": "aac", "language": "eng"},
            {"id": "stream-2", "index": 2, "kind": "audio", "codec_name": "aac", "language": "und"},
            {"id": "stream-3", "index": 3, "kind": "subtitle", "codec_name": "subrip", "language": "spa"}
        ]
    });
    snapshot
}

fn snapshot_with_attachment_stream(container: Option<&str>) -> MediaSnapshotInput {
    let mut snapshot = snapshot_with_streams(container);
    let streams = snapshot
        .stream_summary
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap();
    streams.push(serde_json::json!({
        "id": "stream-4",
        "index": 4,
        "kind": "attachment",
        "codec_name": "mjpeg",
        "filename": "cover.jpg"
    }));
    snapshot
}

fn snapshot_mkv_with_audio_languages_and_defaults(
    languages_and_defaults: &[(&str, bool)],
) -> MediaSnapshotInput {
    let mut snapshot = snapshot_with(Some("mkv"), None, Some(1));
    let audio_streams = languages_and_defaults
        .iter()
        .enumerate()
        .map(|(offset, (language, is_default))| {
            let index = offset + 1;
            serde_json::json!({
                "id": format!("stream-{index}"),
                "index": index,
                "kind": "audio",
                "codec_name": "aac",
                "language": language,
                "disposition": {
                    "default": is_default
                }
            })
        })
        .collect::<Vec<_>>();
    let mut streams = vec![serde_json::json!({
        "id": "stream-0",
        "index": 0,
        "kind": "video",
        "codec_name": "h264"
    })];
    streams.extend(audio_streams);
    snapshot.stream_summary = serde_json::json!({
        "video_stream_count": 1,
        "streams": streams
    });
    snapshot
}

fn snapshot_mp4_with_audio_only_stream_facts() -> MediaSnapshotInput {
    let mut snapshot = snapshot_with(Some("mp4"), None, Some(0));
    snapshot.stream_summary = serde_json::json!({
        "video_stream_count": 0,
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "audio",
                "codec_name": "aac",
                "language": "eng"
            }
        ]
    });
    snapshot
}

fn snapshot_mp4_with_video_audio_subtitle() -> MediaSnapshotInput {
    snapshot_with_streams(Some("mp4"))
}

fn snapshot_mkv_with_video_audio_subtitle() -> MediaSnapshotInput {
    snapshot_with_streams(Some("mkv"))
}

fn request(policy: CompiledPolicy, snapshot: MediaSnapshotInput) -> PlanningRequest {
    PlanningRequest {
        policy,
        input: input_with_snapshot(snapshot),
        context: PlanningContext::default(),
    }
}

fn request_with_transcode(snapshot: MediaSnapshotInput) -> PlanningRequest {
    PlanningRequest {
        policy: policy(CompiledOperation::TranscodeVideo {
            target_codec: "hevc".to_owned(),
            container: "mkv".to_owned(),
            profile: voom_policy::VideoProfileRef::Named("default-hevc".to_owned()),
            resolved_profile: Some(voom_worker_protocol::TranscodeVideoProfile::default_hevc()),
        }),
        input: input_with_snapshot(snapshot),
        context: PlanningContext::default(),
    }
}

fn request_with_transcode_audio(snapshot: MediaSnapshotInput) -> PlanningRequest {
    PlanningRequest {
        policy: policy(CompiledOperation::TranscodeAudio {
            target_codec: "opus".to_owned(),
            container: "mkv".to_owned(),
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            }),
        }),
        input: input_with_snapshot(snapshot),
        context: PlanningContext::default(),
    }
}

fn request_with_extract_audio(snapshot: MediaSnapshotInput) -> PlanningRequest {
    PlanningRequest {
        policy: policy(CompiledOperation::ExtractAudio {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: Some(TrackFilter::Commentary),
        }),
        input: input_with_snapshot(snapshot),
        context: PlanningContext::default(),
    }
}

#[test]
fn groups_container_and_track_operations_into_one_remux_node() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        },
        CompiledOperation::KeepTracks {
            target: TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned(), "und".to_owned()],
            }),
        },
        CompiledOperation::SetDefaults {
            target: TrackTarget::Audio,
            strategy: DefaultStrategy::First,
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["type"], "remux");
    assert_eq!(plan.nodes[0].operation_payload["container"], "mkv");
    assert_eq!(
        plan.nodes[0].operation_payload["track_actions"],
        serde_json::json!([
            {
                "type": "keep_tracks",
                "target": "audio",
                "filter": {
                    "type": "language_in",
                    "values": ["eng", "und"]
                }
            }
        ])
    );
    assert_eq!(
        plan.nodes[0].operation_payload["track_order"],
        serde_json::json!(["video", "audio", "subtitle"])
    );
    assert_eq!(
        plan.nodes[0].operation_payload["defaults"],
        serde_json::json!([
            {
                "target": "audio",
                "strategy": "first"
            }
        ])
    );
}

#[test]
fn remux_payload_includes_existing_media_snapshot_id_when_available() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);
    let mut snapshot = snapshot_with_streams(Some("mp4"));
    snapshot.existing_media_snapshot_id = Some(MediaSnapshotId(99));

    let plan = generate_plan(request(policy, snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(
        plan.nodes[0].operation_payload["source_media_snapshot_id"],
        99
    );
}

#[test]
fn container_mkv_alone_is_no_op_when_snapshot_is_already_mkv() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn container_mkv_alone_plans_when_snapshot_container_is_not_mkv() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container mp4 will be changed to mkv"
    );
}

#[test]
fn container_mkv_alone_blocks_when_snapshot_container_is_unknown() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);
    let mut snapshot = snapshot_mp4_with_video_audio_subtitle();
    snapshot.container = None;

    let plan = generate_plan(request(policy, snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::InsufficientSnapshotFacts
    );
}

#[test]
fn container_mkv_alone_blocks_when_stream_summary_has_zero_video_and_no_streams_array() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);
    // mp4 container with a video_stream_count: 0 summary and NO streams array.
    // A SetContainer-only remux must still enforce video presence and block —
    // not silently bypass the check and emit a Planned remux for a video-less
    // asset.
    let snapshot = snapshot_with(Some("mp4"), Some("h264"), Some(0));

    let plan = generate_plan(request(policy, snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::UnsupportedMediaShape
    );
}

#[test]
fn intervening_non_remux_operation_does_not_split_same_phase_remux_group() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        },
        CompiledOperation::SetTag {
            key: "title".to_owned(),
            value: CompiledValue::String {
                value: "Movie".to_owned(),
            },
        },
        CompiledOperation::KeepTracks {
            target: TrackTarget::Audio,
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["eng".to_owned()],
            }),
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    let remux_nodes = plan
        .nodes
        .iter()
        .filter(|node| node.operation_kind == "remux")
        .collect::<Vec<_>>();
    assert_eq!(remux_nodes.len(), 1);
    assert_eq!(
        remux_nodes[0].operation_payload["track_actions"][0]["type"],
        "keep_tracks"
    );
    assert!(
        plan.nodes
            .iter()
            .any(|node| node.operation_kind == "set_tag" && node.status == NodeStatus::Blocked)
    );
}

#[test]
fn remux_operations_in_different_phases_remain_separate_nodes() {
    let policy = compiled_policy_with_phases(&[
        (
            "normalize",
            vec![CompiledOperation::SetContainer {
                container: "mkv".to_owned(),
            }],
        ),
        (
            "tracks",
            vec![CompiledOperation::KeepTracks {
                target: TrackTarget::Audio,
                filter: Some(TrackFilter::LanguageIn {
                    values: vec!["eng".to_owned()],
                }),
            }],
        ),
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    let remux_nodes = plan
        .nodes
        .iter()
        .filter(|node| node.operation_kind == "remux")
        .collect::<Vec<_>>();
    assert_eq!(remux_nodes.len(), 2);
    assert_eq!(remux_nodes[0].phase_name, "normalize");
    assert_eq!(remux_nodes[1].phase_name, "tracks");
    assert!(plan.edges.iter().any(|edge| {
        edge.from_node_id == remux_nodes[0].node_id && edge.to_node_id == remux_nodes[1].node_id
    }));
}

#[test]
fn defaults_best_blocks_instead_of_joining_executable_group() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        },
        CompiledOperation::SetDefaults {
            target: TrackTarget::Audio,
            strategy: DefaultStrategy::Best,
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_video_audio_subtitle())).unwrap();

    assert!(
        plan.nodes
            .iter()
            .any(|node| node.operation_kind == "remux" && node.status == NodeStatus::Planned)
    );
    assert!(plan.nodes.iter().any(
        |node| node.operation_kind == "set_defaults" && node.status == NodeStatus::Blocked
    ));
}

#[test]
fn attachment_target_track_selection_blocks_before_remux_planning() {
    for operation in [
        CompiledOperation::KeepTracks {
            target: TrackTarget::Attachment,
            filter: Some(TrackFilter::Font),
        },
        CompiledOperation::RemoveTracks {
            target: TrackTarget::Attachment,
            filter: Some(TrackFilter::Font),
        },
    ] {
        let plan = generate_plan(request(
            compiled_policy_with_ops(vec![operation]),
            snapshot_mp4_with_video_audio_subtitle(),
        ))
        .unwrap();

        assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
        assert_eq!(
            plan.diagnostics[0].code,
            PlanningDiagnosticCode::UnsupportedMediaShape
        );
        assert_eq!(
            plan.diagnostics[0].message,
            "attachment track selection is not supported by remux planning"
        );
    }
}

#[test]
fn container_remux_blocks_when_source_snapshot_has_attachment_streams() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_with_attachment_stream(Some("mp4")),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::UnsupportedMediaShape
    );
    assert_eq!(
        plan.diagnostics[0].message,
        "media shape is not supported by remux planning"
    );
}

#[test]
fn track_remux_keep_audio_language_selection_no_ops_when_output_matches_snapshot() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::KeepTracks {
        target: TrackTarget::Audio,
        filter: Some(TrackFilter::LanguageIn {
            values: vec!["eng".to_owned(), "spa".to_owned()],
        }),
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_mkv_with_audio_languages_and_defaults(&[("eng", false), ("spa", false)]),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_set_default_first_no_ops_when_first_audio_is_only_default() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetDefaults {
        target: TrackTarget::Audio,
        strategy: DefaultStrategy::First,
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_mkv_with_audio_languages_and_defaults(&[("eng", true), ("spa", false)]),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_reorder_no_ops_when_group_order_already_matches_snapshot() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::ReorderTracks {
        targets: vec![
            TrackTarget::Video,
            TrackTarget::Audio,
            TrackTarget::Subtitle,
        ],
    }]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_reorder_no_ops_when_absent_groups_are_in_canonical_order() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::ReorderTracks {
        targets: vec![
            TrackTarget::Video,
            TrackTarget::Audio,
            TrackTarget::Subtitle,
        ],
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_mkv_with_audio_languages_and_defaults(&[("eng", false), ("spa", false)]),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_preserve_defaults_no_ops_when_no_other_shape_change() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetDefaults {
        target: TrackTarget::Audio,
        strategy: DefaultStrategy::Preserve,
    }]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_defaults_none_no_ops_when_target_track_kind_is_absent() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetDefaults {
        target: TrackTarget::Subtitle,
        strategy: DefaultStrategy::None,
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_mkv_with_audio_languages_and_defaults(&[("eng", false), ("spa", false)]),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_defaults_preserve_no_ops_when_target_track_kind_is_absent() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetDefaults {
        target: TrackTarget::Subtitle,
        strategy: DefaultStrategy::Preserve,
    }]);

    let plan = generate_plan(request(
        policy,
        snapshot_mkv_with_audio_languages_and_defaults(&[("eng", false), ("spa", false)]),
    ))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
}

#[test]
fn track_remux_reorder_plans_when_group_order_differs_from_snapshot() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::ReorderTracks {
        targets: vec![
            TrackTarget::Audio,
            TrackTarget::Video,
            TrackTarget::Subtitle,
        ],
    }]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(
        plan.nodes[0].status_reason,
        "track selection will be changed"
    );
}

#[test]
fn track_remux_multiple_reorders_in_same_group_blocks_as_ambiguous() {
    let policy = compiled_policy_with_ops(vec![
        CompiledOperation::ReorderTracks {
            targets: vec![
                TrackTarget::Audio,
                TrackTarget::Video,
                TrackTarget::Subtitle,
            ],
        },
        CompiledOperation::ReorderTracks {
            targets: vec![
                TrackTarget::Video,
                TrackTarget::Audio,
                TrackTarget::Subtitle,
            ],
        },
    ]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::UnsupportedMediaShape
    );
}

#[test]
fn track_remux_reorder_with_duplicate_group_blocks_as_unsupported_shape() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::ReorderTracks {
        targets: vec![TrackTarget::Video, TrackTarget::Audio, TrackTarget::Audio],
    }]);

    let plan = generate_plan(request(policy, snapshot_mkv_with_video_audio_subtitle())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "reorder_tracks");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::UnsupportedMediaShape
    );
}

#[test]
fn track_remux_container_only_blocks_when_stream_facts_have_no_video() {
    let policy = compiled_policy_with_ops(vec![CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    }]);

    let plan = generate_plan(request(policy, snapshot_mp4_with_audio_only_stream_facts())).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::UnsupportedMediaShape
    );
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
    assert_eq!(plan.nodes[0].operation_kind, "remux");
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
        serde_json::json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio", "subtitle"],
            "defaults": []
        })
    );
    assert_eq!(plan.summary.executable_node_count, 1);
    assert_eq!(plan.summary.operation_counts_by_kind["remux"], 1);
}

#[test]
fn set_container_plan_nodes_carry_structured_observed_container_when_known() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();
    let node = plan
        .nodes
        .iter()
        .find(|node| node.operation_kind == "remux")
        .unwrap();

    assert_eq!(
        node.observed_state,
        Some(serde_json::json!({"container": "mp4"}))
    );
}

#[test]
fn set_container_plan_nodes_leave_observed_state_absent_when_unknown() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }),
        input: input(None),
        context: PlanningContext::default(),
    })
    .unwrap();
    let node = plan
        .nodes
        .iter()
        .find(|node| node.operation_kind == "remux")
        .unwrap();

    assert_eq!(node.status, NodeStatus::Blocked);
    assert_eq!(node.observed_state, None);
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
    assert_eq!(
        plan.nodes[0].status_reason,
        "container is already mkv and track selection is unchanged"
    );
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
fn transcode_video_plans_non_hevc_or_non_mkv_single_video_snapshot() {
    let plan = generate_plan(request_with_transcode(snapshot_with(
        Some("mp4"),
        Some("h264"),
        Some(1),
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["target_codec"], "hevc");
    assert_eq!(plan.nodes[0].operation_payload["container"], "mkv");
    assert_eq!(
        plan.nodes[0].observed_state,
        Some(serde_json::json!({
            "container": "mp4",
            "video_codec": "h264",
            "video_stream_count": 1
        }))
    );
}

#[test]
fn transcode_video_no_ops_hevc_mkv_single_video_snapshot() {
    let plan = generate_plan(request_with_transcode(snapshot_with(
        Some("mkv"),
        Some("hevc"),
        Some(1),
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
}

#[test]
fn transcode_video_no_ops_h265_mkv_single_video_snapshot() {
    let plan = generate_plan(request_with_transcode(snapshot_with(
        Some("mkv"),
        Some("h265"),
        Some(1),
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
}

#[test]
fn transcode_video_blocks_unknown_or_multi_video_snapshots() {
    assert_transcode_blocked(snapshot_with(None, Some("h264"), Some(1)));
    assert_transcode_blocked(snapshot_with(Some("mkv"), None, Some(1)));
    assert_transcode_blocked(snapshot_with(Some("mkv"), Some("h264"), None));
    assert_transcode_blocked(snapshot_with(Some("mkv"), Some("h264"), Some(0)));
    assert_transcode_blocked(snapshot_with(Some("mkv"), Some("h264"), Some(2)));
}

fn profile_hevc_1080p_mkv() -> voom_worker_protocol::TranscodeVideoProfile {
    let mut profile = voom_worker_protocol::TranscodeVideoProfile::default_hevc();
    profile.pixel_format = Some("yuv420p".to_owned());
    profile.max_width = Some(1920);
    profile.max_height = Some(1080);
    profile
}

fn profile_hevc_mp4() -> voom_worker_protocol::TranscodeVideoProfile {
    voom_worker_protocol::TranscodeVideoProfile::default_hevc()
}

fn profile_hevc_10bit() -> voom_worker_protocol::TranscodeVideoProfile {
    let mut profile = voom_worker_protocol::TranscodeVideoProfile::default_hevc();
    profile.codec_profile = Some("main10".to_owned());
    profile.pixel_format = Some("yuv420p10le".to_owned());
    profile
}

fn video_snapshot(
    container: &str,
    width: u32,
    height: u32,
    extra: &serde_json::Map<String, serde_json::Value>,
    non_video: &[serde_json::Value],
) -> MediaSnapshotInput {
    let mut snapshot = snapshot_with(Some(container), Some("hevc"), Some(1));
    snapshot.width = Some(width);
    snapshot.height = Some(height);
    let mut video = serde_json::Map::new();
    video.insert("id".to_owned(), serde_json::json!("stream-0"));
    video.insert("index".to_owned(), serde_json::json!(0));
    video.insert("kind".to_owned(), serde_json::json!("video"));
    video.insert("codec_name".to_owned(), serde_json::json!("hevc"));
    video.insert("width".to_owned(), serde_json::json!(width));
    video.insert("height".to_owned(), serde_json::json!(height));
    for (key, value) in extra {
        video.insert(key.clone(), value.clone());
    }
    let mut streams = vec![serde_json::Value::Object(video)];
    streams.extend(non_video.iter().cloned());
    snapshot.stream_summary = serde_json::json!({
        "video_stream_count": 1,
        "streams": streams,
    });
    snapshot
}

fn video_extra(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn source_hevc_720_mkv() -> MediaSnapshotInput {
    video_snapshot(
        "mkv",
        1280,
        720,
        &video_extra(&[("pixel_format", serde_json::json!("yuv420p"))]),
        &[],
    )
}

fn source_hevc_2160_mkv() -> MediaSnapshotInput {
    video_snapshot(
        "mkv",
        3840,
        2160,
        &video_extra(&[("pixel_format", serde_json::json!("yuv420p"))]),
        &[],
    )
}

fn source_hevc_8bit() -> MediaSnapshotInput {
    video_snapshot(
        "mkv",
        1280,
        720,
        &video_extra(&[
            ("pixel_format", serde_json::json!("yuv420p")),
            ("profile", serde_json::json!("Main")),
        ]),
        &[],
    )
}

fn source_without_pixel_format() -> MediaSnapshotInput {
    video_snapshot("mkv", 1280, 720, &video_extra(&[]), &[])
}

fn source_two_video_streams() -> MediaSnapshotInput {
    let mut snapshot = source_hevc_720_mkv();
    let streams = snapshot
        .stream_summary
        .get_mut("streams")
        .and_then(serde_json::Value::as_array_mut)
        .unwrap();
    streams.push(serde_json::json!({
        "id": "stream-1", "index": 1, "kind": "video", "codec_name": "hevc"
    }));
    snapshot.stream_summary["video_stream_count"] = serde_json::json!(2);
    snapshot
}

fn source_with_ass_subtitle() -> MediaSnapshotInput {
    video_snapshot(
        "mkv",
        1280,
        720,
        &video_extra(&[("pixel_format", serde_json::json!("yuv420p"))]),
        &[serde_json::json!({
            "id": "stream-1", "index": 1, "kind": "subtitle", "codec_name": "ass"
        })],
    )
}

fn source_stream_missing_codec() -> MediaSnapshotInput {
    video_snapshot(
        "mkv",
        1280,
        720,
        &video_extra(&[("pixel_format", serde_json::json!("yuv420p"))]),
        &[serde_json::json!({"id": "stream-1", "index": 1, "kind": "audio"})],
    )
}

fn plan_transcode_with_container(
    profile: voom_worker_protocol::TranscodeVideoProfile,
    snapshot: MediaSnapshotInput,
    container: &str,
) -> crate::ExecutionPlan {
    let policy = policy(CompiledOperation::TranscodeVideo {
        target_codec: profile.target_codec.clone(),
        container: container.to_owned(),
        profile: voom_policy::VideoProfileRef::Named(profile.name.clone()),
        resolved_profile: Some(profile),
    });
    generate_plan(request(policy, snapshot)).unwrap()
}

fn node_status(plan: &crate::ExecutionPlan) -> NodeStatus {
    plan.nodes[0].status.clone()
}

fn blocked_reason(plan: &crate::ExecutionPlan) -> &str {
    plan.nodes[0].status_reason.as_str()
}

fn resource_notes(plan: &crate::ExecutionPlan) -> Vec<String> {
    plan.nodes[0].resource_estimates.notes.clone()
}

#[test]
fn no_op_when_all_observable_constraints_satisfied() {
    let plan =
        plan_transcode_with_container(profile_hevc_1080p_mkv(), source_hevc_720_mkv(), "mkv");
    assert_eq!(node_status(&plan), NodeStatus::NoOp);
}

#[test]
fn planned_when_too_wide() {
    let plan =
        plan_transcode_with_container(profile_hevc_1080p_mkv(), source_hevc_2160_mkv(), "mkv");
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn planned_on_container_change() {
    let plan = plan_transcode_with_container(profile_hevc_mp4(), source_hevc_720_mkv(), "mp4");
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn planned_on_wrong_pixel_format_or_profile_level() {
    let plan = plan_transcode_with_container(profile_hevc_10bit(), source_hevc_8bit(), "mkv");
    assert_eq!(node_status(&plan), NodeStatus::Planned);
}

#[test]
fn blocked_insufficient_when_constrained_pixel_format_unknown() {
    let plan =
        plan_transcode_with_container(profile_hevc_10bit(), source_without_pixel_format(), "mkv");
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
}

#[test]
fn blocked_unsupported_when_not_exactly_one_video_stream() {
    let plan = plan_transcode_with_container(profile_hevc_mp4(), source_two_video_streams(), "mp4");
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert_eq!(plan.diagnostics[0].code.as_str(), "unsupported_media_shape");
}

#[test]
fn blocked_when_mp4_target_has_incompatible_subtitle() {
    let plan = plan_transcode_with_container(profile_hevc_mp4(), source_with_ass_subtitle(), "mp4");
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert!(blocked_reason(&plan).contains("ass"));
}

#[test]
fn blocked_insufficient_when_mp4_stream_inventory_underdescribed() {
    let plan =
        plan_transcode_with_container(profile_hevc_mp4(), source_stream_missing_codec(), "mp4");
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
}

#[test]
fn blocked_insufficient_when_mp4_target_and_streams_array_absent() {
    // A snapshot with video_stream_count but no "streams" array is under-described
    // for an mp4 target — the gate must block rather than pass through.
    let mut snapshot = snapshot_with(Some("mkv"), Some("h264"), Some(1));
    snapshot.stream_summary = serde_json::json!({ "video_stream_count": 1 });
    let plan = plan_transcode_with_container(profile_hevc_mp4(), snapshot, "mp4");
    assert_eq!(node_status(&plan), NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
}

#[test]
fn resource_notes_are_format_stable() {
    let plan =
        plan_transcode_with_container(profile_hevc_1080p_mkv(), source_hevc_2160_mkv(), "mkv");
    let notes = resource_notes(&plan);
    assert!(notes.contains(&"encoder=libx265".to_owned()));
    assert!(notes.contains(&"speed=medium".to_owned()));
    assert!(notes.contains(&"cpu_cost=medium".to_owned()));
    assert!(notes.contains(&"crf=23".to_owned()));
    assert!(notes.contains(&"downscale=3840x2160->1920x1080".to_owned()));
}

#[test]
fn downscale_note_emits_when_only_width_is_constrained() {
    let mut profile = voom_worker_protocol::TranscodeVideoProfile::default_hevc();
    profile.pixel_format = Some("yuv420p".to_owned());
    profile.max_width = Some(1920);
    let plan = plan_transcode_with_container(profile, source_hevc_2160_mkv(), "mkv");
    let notes = resource_notes(&plan);
    assert!(notes.contains(&"downscale=3840x2160->1920x2160".to_owned()));
}

#[test]
fn transcode_video_node_payload_carries_profile_and_resolved_profile() {
    let plan = plan_transcode_with_container(profile_hevc_mp4(), source_hevc_720_mkv(), "mp4");
    let payload = &plan.nodes[0].operation_payload;
    assert_eq!(payload["profile"], "default-hevc");
    assert!(payload["resolved_profile"].is_object());
    assert_eq!(payload["resolved_profile"]["encoder"], "libx265");
    assert_eq!(payload["resolved_profile"]["crf"], 23);
}

#[test]
fn transcode_video_blocks_when_profile_unresolved() {
    let policy = policy(CompiledOperation::TranscodeVideo {
        target_codec: "hevc".to_owned(),
        container: "mkv".to_owned(),
        profile: voom_policy::VideoProfileRef::Named("default-hevc".to_owned()),
        resolved_profile: None,
    });
    let result = generate_plan(request(policy, source_hevc_720_mkv()));
    let error = result.unwrap_err();
    assert_eq!(
        error.diagnostics[0].code,
        PlanningDiagnosticCode::InvalidPlanningRequest
    );
}

#[test]
fn unresolved_profile_reports_every_missing_snapshot() {
    let policy = policy(CompiledOperation::TranscodeVideo {
        target_codec: "hevc".to_owned(),
        container: "mkv".to_owned(),
        profile: voom_policy::VideoProfileRef::Named("default-hevc".to_owned()),
        resolved_profile: None,
    });
    let mut input = input_with_snapshot(source_hevc_720_mkv());
    input.media_snapshots.push(source_hevc_2160_mkv());

    let error = generate_plan(PlanningRequest {
        policy,
        input,
        context: PlanningContext::default(),
    })
    .unwrap_err();

    assert_eq!(error.diagnostics.len(), 2);
    assert!(
        error
            .diagnostics
            .iter()
            .all(|d| d.code == PlanningDiagnosticCode::InvalidPlanningRequest)
    );
}

#[test]
fn transcode_audio_plans_selected_aac_audio_to_opus() {
    let plan = generate_plan(request_with_transcode_audio(snapshot_with_audio_streams(
        Some("mp4"),
        &[
            audio_stream(1, "aac", "eng", Some(false)),
            audio_stream(2, "aac", "jpn", Some(false)),
        ],
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["type"], "transcode_audio");
    assert_eq!(plan.nodes[0].operation_payload["target_codec"], "opus");
    assert_eq!(plan.nodes[0].operation_payload["container"], "mkv");
    assert_eq!(
        plan.nodes[0].operation_payload["source_media_snapshot_id"],
        42
    );
    assert_eq!(
        plan.nodes[0]
            .capability_hints
            .operation_capability
            .as_deref(),
        Some("transcode_audio")
    );
}

#[test]
fn transcode_audio_no_ops_selected_opus_audio_in_mkv() {
    let plan = generate_plan(request_with_transcode_audio(snapshot_with_audio_streams(
        Some("mkv"),
        &[audio_stream(1, "opus", "eng", Some(false))],
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::NoOp);
}

#[test]
fn transcode_audio_blocks_when_selector_matches_zero_audio_streams() {
    let plan = generate_plan(request_with_transcode_audio(snapshot_with_audio_streams(
        Some("mkv"),
        &[audio_stream(1, "aac", "jpn", Some(false))],
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].operation_kind.as_deref(),
        Some("transcode_audio")
    );
}

#[test]
fn extract_audio_where_commentary_plans_for_exactly_one_known_commentary_stream() {
    let plan = generate_plan(request_with_extract_audio(snapshot_with_audio_streams(
        Some("mkv"),
        &[
            audio_stream(1, "aac", "eng", Some(false)),
            audio_stream(2, "aac", "eng", Some(true)),
        ],
    )))
    .unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "extract_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[0].operation_payload["type"], "extract_audio");
    assert_eq!(plan.nodes[0].operation_payload["target_codec"], "opus");
    assert_eq!(plan.nodes[0].operation_payload["container"], "ogg");
    assert_eq!(
        plan.nodes[0]
            .capability_hints
            .operation_capability
            .as_deref(),
        Some("extract_audio")
    );
}

#[test]
fn extract_audio_where_commentary_blocks_on_zero_multiple_or_unknown_commentary() {
    assert_extract_audio_blocked(snapshot_with_audio_streams(
        Some("mkv"),
        &[audio_stream(1, "aac", "eng", Some(false))],
    ));
    assert_extract_audio_blocked(snapshot_with_audio_streams(
        Some("mkv"),
        &[
            audio_stream(1, "aac", "eng", Some(true)),
            audio_stream(2, "aac", "eng", Some(true)),
        ],
    ));
    assert_extract_audio_blocked(snapshot_with_audio_streams(
        Some("mkv"),
        &[audio_stream(1, "aac", "eng", None)],
    ));
}

fn assert_extract_audio_blocked(snapshot: MediaSnapshotInput) {
    let plan = generate_plan(request_with_extract_audio(snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "extract_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.summary.blocked_node_count, 1);
}

fn assert_transcode_blocked(snapshot: MediaSnapshotInput) {
    let plan = generate_plan(request_with_transcode(snapshot)).unwrap();

    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(plan.diagnostics.len(), 1);
}

fn snapshot_with_audio_streams(
    container: Option<&str>,
    audio_streams: &[serde_json::Value],
) -> MediaSnapshotInput {
    let mut snapshot = snapshot_with(container, Some("h264"), Some(1));
    let mut streams = vec![serde_json::json!({
        "id": "stream-0",
        "index": 0,
        "kind": "video",
        "codec_name": "h264"
    })];
    streams.extend_from_slice(audio_streams);
    snapshot.stream_summary = serde_json::json!({
        "video_stream_count": 1,
        "streams": streams
    });
    snapshot.existing_media_snapshot_id = Some(MediaSnapshotId(42));
    snapshot
}

fn audio_stream(
    index: u32,
    codec: &str,
    language: &str,
    commentary: Option<bool>,
) -> serde_json::Value {
    let mut stream = serde_json::json!({
        "id": format!("stream-{index}"),
        "index": index,
        "kind": "audio",
        "codec_name": codec,
        "language": language,
        "title": format!("Audio {index}"),
        "channels": 2
    });
    if let Some(commentary) = commentary {
        stream["disposition"] = serde_json::json!({
            "default": false,
            "forced": false,
            "commentary": commentary
        });
    }
    stream
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
fn phase_skip_if_true_suppresses_phase_operations() {
    let mut compiled = policy(CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    });
    compiled.phases[0].skip_if = Some(CompiledCondition::FieldComparison {
        path: vec!["container".to_owned(), "name".to_owned()],
        op: ComparisonOp::Eq,
        value: CompiledValue::String {
            value: "mp4".to_owned(),
        },
    });

    let skipped = generate_plan(PlanningRequest {
        policy: compiled.clone(),
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();
    assert!(skipped.nodes.is_empty());
    assert!(skipped.diagnostics.is_empty());

    let not_skipped = generate_plan(PlanningRequest {
        policy: compiled,
        input: input(Some("avi")),
        context: PlanningContext::default(),
    })
    .unwrap();
    assert_eq!(not_skipped.nodes.len(), 1);
    assert_eq!(not_skipped.nodes[0].status, NodeStatus::Planned);
}

#[test]
fn unresolved_phase_run_if_blocks_phase_operations() {
    let mut compiled = policy(CompiledOperation::SetContainer {
        container: "mkv".to_owned(),
    });
    compiled.phases[0].run_if = Some(CompiledCondition::Predicate {
        name: "modified".to_owned(),
    });

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
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
fn track_operations_block_when_snapshot_stream_facts_are_missing() {
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
    assert_eq!(plan.nodes[0].operation_kind, "remux");
    assert_eq!(plan.summary.blocked_node_count, 1);
    assert_eq!(plan.diagnostics.len(), 1);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::InsufficientSnapshotFacts
    );
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

#[test]
fn missing_condition_field_blocks_nested_operation() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Conditional {
            condition: CompiledCondition::FieldComparison {
                path: vec!["video".to_owned(), "codec".to_owned()],
                op: ComparisonOp::Eq,
                value: CompiledValue::String {
                    value: "hevc".to_owned(),
                },
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
fn unsupported_condition_comparison_blocks_nested_operation() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Conditional {
            condition: CompiledCondition::FieldComparison {
                path: vec!["container".to_owned(), "name".to_owned()],
                op: ComparisonOp::Lt,
                value: CompiledValue::String {
                    value: "mkv".to_owned(),
                },
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
fn rules_first_uses_first_matching_rule_only() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Rules {
            mode: RuleMatchMode::First,
            rules: vec![
                rule(
                    "wrong-container",
                    Some(container_is("avi")),
                    vec![CompiledOperation::ClearTags],
                ),
                rule(
                    "matching-container",
                    Some(container_is("mp4")),
                    vec![transcode_video()],
                ),
                rule("not-reached", None, vec![CompiledOperation::ClearTags]),
            ],
        }),
        input: transcodable_input("mp4"),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
}

#[test]
fn rules_all_preserves_matching_rule_order() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Rules {
            mode: RuleMatchMode::All,
            rules: vec![
                rule(
                    "transcode",
                    Some(container_is("mp4")),
                    vec![transcode_video()],
                ),
                rule(
                    "clear-tags",
                    Some(container_is("mp4")),
                    vec![CompiledOperation::ClearTags],
                ),
            ],
        }),
        input: transcodable_input("mp4"),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Planned);
    assert_eq!(plan.nodes[1].operation_kind, "clear_tags");
    assert_eq!(plan.nodes[1].status, NodeStatus::Blocked);
}

#[test]
fn rules_unknown_condition_blocks_nested_leaf_operation() {
    let plan = generate_plan(PlanningRequest {
        policy: policy(CompiledOperation::Rules {
            mode: RuleMatchMode::First,
            rules: vec![rule(
                "host-state",
                Some(CompiledCondition::Predicate {
                    name: "external_host_state".to_owned(),
                }),
                vec![transcode_video()],
            )],
        }),
        input: input(Some("mp4")),
        context: PlanningContext::default(),
    })
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].operation_kind, "transcode_video");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code.as_str(),
        "insufficient_snapshot_facts"
    );
}

#[test]
fn plan_phase_plans_only_the_named_phase() {
    let policy = compiled_policy_with_phases(&[
        (
            "normalize",
            vec![CompiledOperation::SetContainer {
                container: "mkv".to_owned(),
            }],
        ),
        (
            "tracks",
            vec![CompiledOperation::KeepTracks {
                target: TrackTarget::Audio,
                filter: Some(TrackFilter::LanguageIn {
                    values: vec!["eng".to_owned()],
                }),
            }],
        ),
    ]);

    let tracks_plan = plan_phase(
        request(policy.clone(), snapshot_mp4_with_video_audio_subtitle()),
        "tracks",
    )
    .unwrap();

    assert!(!tracks_plan.nodes.is_empty());
    assert!(
        tracks_plan
            .nodes
            .iter()
            .all(|node| node.phase_name == "tracks"),
        "plan_phase must emit nodes only for the named phase"
    );

    let normalize_plan = plan_phase(
        request(policy, snapshot_mp4_with_video_audio_subtitle()),
        "normalize",
    )
    .unwrap();
    assert!(!normalize_plan.nodes.is_empty());
    assert!(
        normalize_plan
            .nodes
            .iter()
            .all(|node| node.phase_name == "normalize")
    );
}

#[test]
fn plan_phase_reevaluates_skip_if_against_supplied_snapshot() {
    let mut policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);
    policy.phases[0].skip_if = Some(container_is("mp4"));

    let skipped = plan_phase(
        request(policy.clone(), snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap();
    assert!(
        skipped.nodes.is_empty(),
        "a skipped phase produces no nodes"
    );
    assert!(skipped.diagnostics.is_empty());

    let planned = plan_phase(
        request(policy, snapshot_with(Some("avi"), None, None)),
        "normalize",
    )
    .unwrap();
    assert_eq!(planned.nodes.len(), 1);
    assert_eq!(planned.nodes[0].status, NodeStatus::Planned);
}

#[test]
fn plan_phase_reevaluates_run_if_against_supplied_snapshot() {
    let mut policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);
    policy.phases[0].run_if = Some(container_is("avi"));

    let does_not_run = plan_phase(
        request(policy.clone(), snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap();
    assert!(does_not_run.nodes.is_empty());

    let runs = plan_phase(
        request(policy, snapshot_with(Some("avi"), None, None)),
        "normalize",
    )
    .unwrap();
    assert_eq!(runs.nodes.len(), 1);
    assert_eq!(runs.nodes[0].status, NodeStatus::Planned);
}

#[test]
fn plan_phase_unplannable_operation_yields_blocked_node_and_diagnostic() {
    let policy = compiled_policy_with_phases(&[(
        "audio",
        vec![CompiledOperation::ExtractAudio {
            target_codec: "opus".to_owned(),
            container: "ogg".to_owned(),
            filter: Some(TrackFilter::LanguageIn {
                values: vec!["jpn".to_owned()],
            }),
        }],
    )]);

    let plan = plan_phase(
        request(policy, snapshot_mkv_with_video_audio_subtitle()),
        "audio",
    )
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].operation_kind, "extract_audio");
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert!(
        plan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.phase_name.as_deref() == Some("audio")),
        "the blocking diagnostic must name the phase the coordinator blocks"
    );
}

#[test]
fn plan_phase_rejects_phase_not_in_phase_order() {
    let policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);

    let error: PlanGenerationError = plan_phase(
        request(policy, snapshot_with(Some("mp4"), None, None)),
        "ghost",
    )
    .unwrap_err();

    assert_eq!(
        error.diagnostics[0].code,
        PlanningDiagnosticCode::InvalidPlanningRequest
    );
    assert!(error.diagnostics[0].message.contains("ghost"));
}

#[test]
fn plan_phase_rejects_phase_declared_in_order_but_missing_from_phases() {
    let mut policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);
    // An internally inconsistent policy: the name is bounded by phase_order but
    // has no phase body to plan. Fail loud rather than returning a node-less
    // plan a coordinator could misread as a legitimately skipped phase.
    policy.phases.clear();

    let error = plan_phase(
        request(policy, snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap_err();

    assert_eq!(
        error.diagnostics[0].code,
        PlanningDiagnosticCode::InvalidPlanningRequest
    );
    assert!(error.diagnostics[0].message.contains("normalize"));
}

#[test]
fn plan_phase_blocks_phase_when_run_if_facts_are_insufficient() {
    let mut policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);
    policy.phases[0].run_if = Some(CompiledCondition::Predicate {
        name: "modified".to_owned(),
    });

    let plan = plan_phase(
        request(policy, snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap();

    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(plan.nodes[0].status, NodeStatus::Blocked);
    assert_eq!(
        plan.diagnostics[0].code,
        PlanningDiagnosticCode::InsufficientSnapshotFacts
    );
}

#[test]
fn plan_phase_is_deterministic_for_same_inputs() {
    let policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);

    let first = plan_phase(
        request(policy.clone(), snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap();
    let second = plan_phase(
        request(policy, snapshot_with(Some("mp4"), None, None)),
        "normalize",
    )
    .unwrap();

    assert_eq!(first.plan_id, second.plan_id);
    assert_eq!(first.plan_hash, second.plan_hash);
}

#[test]
fn plan_phase_carries_no_inter_phase_edges() {
    let policy = compiled_policy_with_phases(&[
        (
            "normalize",
            vec![CompiledOperation::SetContainer {
                container: "mkv".to_owned(),
            }],
        ),
        (
            "tracks",
            vec![CompiledOperation::KeepTracks {
                target: TrackTarget::Audio,
                filter: Some(TrackFilter::LanguageIn {
                    values: vec!["eng".to_owned()],
                }),
            }],
        ),
    ]);

    let plan = plan_phase(
        request(policy, snapshot_mp4_with_video_audio_subtitle()),
        "tracks",
    )
    .unwrap();

    assert!(
        plan.edges.is_empty(),
        "a single-phase plan carries no inter-phase edges; ordering is the coordinator's barrier"
    );
}

#[test]
fn plan_phase_rejects_empty_input_set() {
    let policy = compiled_policy_with_phases(&[(
        "normalize",
        vec![CompiledOperation::SetContainer {
            container: "mkv".to_owned(),
        }],
    )]);
    let mut input = input_with_snapshot(snapshot_with(Some("mp4"), None, None));
    input.media_snapshots.clear();

    let error = plan_phase(
        PlanningRequest {
            policy,
            input,
            context: PlanningContext::default(),
        },
        "normalize",
    )
    .unwrap_err();
    assert_eq!(
        error.diagnostics[0].code,
        PlanningDiagnosticCode::EmptyInputSet
    );
}

fn rule(
    name: &str,
    condition: Option<CompiledCondition>,
    operations: Vec<CompiledOperation>,
) -> CompiledRule {
    CompiledRule {
        name: name.to_owned(),
        condition,
        operations,
    }
}

fn container_is(container: &str) -> CompiledCondition {
    CompiledCondition::FieldComparison {
        path: vec!["container".to_owned(), "name".to_owned()],
        op: ComparisonOp::Eq,
        value: CompiledValue::String {
            value: container.to_owned(),
        },
    }
}

fn transcode_video() -> CompiledOperation {
    CompiledOperation::TranscodeVideo {
        target_codec: "hevc".to_owned(),
        container: "mkv".to_owned(),
        profile: voom_policy::VideoProfileRef::Named("default-hevc".to_owned()),
        resolved_profile: Some(voom_worker_protocol::TranscodeVideoProfile::default_hevc()),
    }
}

fn transcodable_input(container: &str) -> PolicyInputSetDraft {
    input_with_snapshot(snapshot_with(Some(container), Some("h264"), Some(1)))
}
