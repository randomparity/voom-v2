use voom_plan::PlanOperationKind;
use voom_policy::{FixtureName, load_fixture, load_policy_fixture};
use voom_store::repo::media::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};

use super::*;
use crate::cases::policy::policy_inputs::PolicyInputFromScanInput;
use crate::cases::{cp, transcodable_input};

const T0: time::OffsetDateTime = time::OffsetDateTime::UNIX_EPOCH;

async fn seed_stored_snapshot(
    cp: &crate::ControlPlane,
    path: &str,
    payload: serde_json::Value,
) -> (voom_core::FileVersionId, voom_core::MediaSnapshotId) {
    let file_version_id = seed_stored_version(cp, path).await;
    let snapshot = cp
        .record_media_snapshot(file_version_id, None, payload, T0)
        .await
        .unwrap();
    (file_version_id, snapshot.id)
}

async fn seed_stored_version(cp: &crate::ControlPlane, path: &str) -> voom_core::FileVersionId {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.to_owned(),
                content_hash: format!("hash-{path}"),
                size_bytes: 1024,
                observed_at: T0,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id, ..
    } = outcome
    else {
        panic!("expected a new file asset");
    };
    file_version_id
}

fn compiled_policy(body: &str) -> voom_policy::CompiledPolicy {
    voom_policy::compile_policy(&format!(
        "policy \"stored authority\" {{ phase normalize {{ {body} }} }}"
    ))
    .unwrap()
    .policy
}

fn assert_stored_facts_error(error: &VoomError) {
    assert_eq!(error.code(), "PLAN_GENERATION_ERROR");
    assert!(
        error
            .to_string()
            .contains("stored policy stream facts are invalid:")
    );
}

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
    assert_eq!(
        plan.input.source_label.as_deref(),
        Some("synthetic_noncompliant_transcode_needed")
    );
    assert!(
        plan.nodes
            .iter()
            .any(|node| node.status == voom_plan::NodeStatus::Planned)
    );
}

#[test]
fn stored_stream_shape_gate_rejects_condition_slots_in_path_order() {
    for (value, expected_path) in [
        (
            serde_json::json!({
                "phases": [{
                    "run_if": {
                        "type": "exists",
                        "target": "audio",
                        "extra": true
                    }
                }]
            }),
            "/phases/0/run_if",
        ),
        (
            serde_json::json!({
                "phases": [{
                    "operations": [{
                        "type": "conditional",
                        "condition": {"type": "predicate", "name": "ready"},
                        "operations": [{
                            "type": "rules",
                            "rules": [{
                                "condition": {
                                    "type": "and",
                                    "conditions": [
                                        {"type": "predicate", "name": "ready"},
                                        {"type": "count", "target": "audio", "op": "gte"}
                                    ]
                                },
                                "operations": []
                            }]
                        }]
                    }]
                }]
            }),
            "/phases/0/operations/0/operations/0/rules/0/condition/conditions/1",
        ),
    ] {
        let error = validate_stored_stream_condition_shapes(&value).unwrap_err();

        assert_eq!(error.code(), "PLAN_GENERATION_ERROR");
        assert!(
            error
                .to_string()
                .contains("unpublished compiled stream condition at")
        );
        assert!(error.to_string().contains(expected_path));
    }
}

#[test]
fn stored_stream_shape_gate_ignores_unrelated_tagged_json() {
    let value = serde_json::json!({
        "metadata": {
            "tagged": {"type": "exists", "target": "audio", "extra": true}
        },
        "provenance": {
            "flags": {"tagged": {"type": "count", "extra": true}}
        },
        "phases": [{
            "operations": [{
                "type": "set_tag",
                "key": "tagged",
                "value": {"type": "exists", "target": "audio", "extra": true}
            }]
        }]
    });

    validate_stored_stream_condition_shapes(&value).unwrap();
}

#[tokio::test]
async fn durable_planning_reads_compiled_policy_without_creating_execution_state() {
    let (cp, _tmp) = cp().await;
    let source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let created_policy = cp
        .create_policy_document("container-metadata", &source)
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(
            load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap(),
        )
        .await
        .unwrap();

    let before = read_only_table_counts(&cp).await;

    let plan = cp
        .plan_accepted_policy_version_with_input_set(created_policy.version.id, input.id)
        .await
        .unwrap();

    assert_eq!(plan.policy.version_id, Some(created_policy.version.id));
    assert_eq!(plan.input.input_set_id, Some(input.id));
    assert_eq!(before, read_only_table_counts(&cp).await);
}

#[tokio::test]
async fn stored_stream_input_replaces_cached_facts() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) = seed_stored_snapshot(
        &cp,
        "/srv/authority.mkv",
        serde_json::json!({
            "container": {"format_name": "mkv"},
            "streams": [{"id": "audio-old", "kind": "audio"}]
        }),
    )
    .await;
    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "stored-authority".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "cached".to_owned(),
            video_codec: "cached".to_owned(),
        })
        .await
        .unwrap();
    let latest = cp
        .record_media_snapshot(
            file_version_id,
            None,
            serde_json::json!({
                "container": {"format_name": "mkv"},
                "streams": [{"id": "subtitle-current", "kind": "subtitle"}]
            }),
            T0,
        )
        .await
        .unwrap();
    let mut input = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    input.media_snapshots[0].existing_media_snapshot_id = None;
    let policy = compiled_policy("when exists subtitle { container mkv }");

    let resolved = cp
        .resolve_stored_planning_input(&policy, input)
        .await
        .unwrap();

    assert_eq!(resolved.files.len(), 1);
    assert_eq!(resolved.files[0].ordinal, 1);
    assert_eq!(resolved.files[0].selected_version_id, file_version_id);
    assert_eq!(
        resolved.files[0].file_asset_id,
        resolved.files[0].active_version.file_asset_id
    );
    assert_eq!(resolved.files[0].active_version.id, file_version_id);
    assert_eq!(resolved.files[0].active_snapshot.id, latest.id);
    assert_eq!(
        resolved.draft.media_snapshots[0].stream_summary["streams"],
        latest.payload["streams"]
    );
    assert_eq!(
        resolved.draft.media_snapshots[0].existing_media_snapshot_id,
        Some(latest.id)
    );
}

#[tokio::test]
async fn stored_stream_plan_and_report_use_current_snapshot_facts() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "stored-current-container",
            "policy \"stored current container\" { phase normalize { container mkv } }",
        )
        .await
        .unwrap();
    let (file_version_id, media_snapshot_id) = seed_stored_snapshot(
        &cp,
        "/srv/current-container.mkv",
        serde_json::json!({
            "container": {"format_name": "matroska,webm"},
            "streams": [{"id": "video-0", "kind": "video", "codec_name": "h264"}]
        }),
    )
    .await;
    let input = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "cached-old-container".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mp4".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();

    let plan = cp
        .plan_accepted_policy_version_with_input_set(policy.version.id, input.input_set_id)
        .await
        .unwrap();
    let report = cp
        .generate_compliance_report(policy.version.id, input.input_set_id)
        .await
        .unwrap();

    assert_eq!(plan.nodes[0].status, voom_plan::NodeStatus::NoOp);
    assert_eq!(report.plan.nodes[0].status, voom_plan::NodeStatus::NoOp);
    assert_eq!(
        plan.nodes[0].observed_state.as_ref().unwrap()["container"],
        "mkv"
    );
    assert_eq!(
        report.plan.nodes[0].observed_state.as_ref().unwrap()["container"],
        "mkv"
    );
}

#[tokio::test]
async fn stored_stream_plan_and_report_block_unknown_or_malformed_containers() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "stored-invalid-container",
            "policy \"stored invalid container\" { phase normalize { container mkv } }",
        )
        .await
        .unwrap();

    for (index, container) in [
        serde_json::json!("matroska,unknown"),
        serde_json::json!({"format_name": 42}),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("/srv/invalid-container-{index}.mkv");
        let (file_version_id, media_snapshot_id) = seed_stored_snapshot(
            &cp,
            &path,
            serde_json::json!({
                "container": container,
                "streams": [{"id": "video-0", "kind": "video", "codec_name": "h264"}]
            }),
        )
        .await;
        let input = cp
            .create_policy_input_set_from_scan(PolicyInputFromScanInput {
                slug: format!("invalid-container-{index}"),
                file_version_id,
                media_snapshot_id,
                container: "mkv".to_owned(),
                video_codec: "h264".to_owned(),
            })
            .await
            .unwrap();

        let plan = cp
            .plan_accepted_policy_version_with_input_set(policy.version.id, input.input_set_id)
            .await
            .unwrap();
        let report = cp
            .generate_compliance_report(policy.version.id, input.input_set_id)
            .await
            .unwrap();

        for checked in [&plan, &report.plan] {
            assert_eq!(checked.nodes[0].status, voom_plan::NodeStatus::Blocked);
            assert_eq!(
                checked.nodes[0].status_reason,
                "snapshot container is unknown"
            );
            assert_eq!(
                checked.diagnostics[0].code,
                voom_plan::PlanningDiagnosticCode::InsufficientSnapshotFacts
            );
        }
    }
}

#[tokio::test]
async fn stored_stream_input_rejects_non_file_members_and_links() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let input = cp.get_policy_input_set(created.id).await.unwrap().unwrap();

    let stream_error = cp
        .resolve_stored_planning_input(
            &compiled_policy("when exists audio { container mkv }"),
            input.clone(),
        )
        .await
        .unwrap_err();
    assert_stored_facts_error(&stream_error);
    assert!(stream_error.to_string().contains("targets synthetic"));

    let mut linked = input;
    linked.media_snapshots[0].existing_media_snapshot_id =
        Some(voom_core::MediaSnapshotId(999_999));
    let linked_error = cp
        .resolve_stored_planning_input(&compiled_policy("container mkv"), linked)
        .await
        .unwrap_err();
    assert_stored_facts_error(&linked_error);
    assert!(linked_error.to_string().contains("links snapshot"));
}

#[tokio::test]
async fn stored_stream_input_rejects_mismatched_snapshot_provenance() {
    let (cp, _tmp) = cp().await;
    let (selected_version, selected_snapshot) =
        seed_stored_snapshot(&cp, "/srv/selected.mkv", serde_json::json!({})).await;
    let (_, other_snapshot) =
        seed_stored_snapshot(&cp, "/srv/other.mkv", serde_json::json!({})).await;
    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "mismatched-provenance".to_owned(),
            file_version_id: selected_version,
            media_snapshot_id: selected_snapshot,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let mut input = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    input.media_snapshots[0].existing_media_snapshot_id = Some(other_snapshot);

    let error = cp
        .resolve_stored_planning_input(&compiled_policy("container mkv"), input)
        .await
        .unwrap_err();

    assert_stored_facts_error(&error);
    assert!(error.to_string().contains("but selects file version"));
}

#[tokio::test]
async fn stored_stream_input_rejects_duplicate_lineage_before_active_snapshot() {
    let (cp, _tmp) = cp().await;
    let version_id = seed_stored_version(&cp, "/srv/unprobed.mkv").await;
    let created = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let mut input = cp.get_policy_input_set(created.id).await.unwrap().unwrap();
    input.media_snapshots[0].target = PolicyInputTargetRef::FileVersion { id: version_id };
    input.media_snapshots[0].existing_media_snapshot_id = None;
    let mut duplicate = input.media_snapshots[0].clone();
    duplicate.ordinal = 2;
    input.media_snapshots.push(duplicate);

    let error = cp
        .resolve_stored_planning_input(&compiled_policy("container mkv"), input)
        .await
        .unwrap_err();

    assert_stored_facts_error(&error);
    assert!(error.to_string().contains("selects file asset"));
    assert!(error.to_string().contains("twice"));
    assert!(!error.to_string().contains("no active snapshot"));
}

#[tokio::test]
async fn stored_stream_input_reports_missing_authority() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let base = cp.get_policy_input_set(created.id).await.unwrap().unwrap();
    let policy = compiled_policy("container mkv");

    let mut missing_version = base.clone();
    missing_version.media_snapshots[0].target = PolicyInputTargetRef::FileVersion {
        id: voom_core::FileVersionId(999_999),
    };
    let error = cp
        .resolve_stored_planning_input(&policy, missing_version)
        .await
        .unwrap_err();
    assert_stored_facts_error(&error);
    assert!(error.to_string().contains("selects missing file version"));

    let unprobed = seed_stored_version(&cp, "/srv/missing-snapshot.mkv").await;
    let mut missing_snapshot = base.clone();
    missing_snapshot.media_snapshots[0].target = PolicyInputTargetRef::FileVersion { id: unprobed };
    missing_snapshot.media_snapshots[0].existing_media_snapshot_id = None;
    let error = cp
        .resolve_stored_planning_input(&policy, missing_snapshot)
        .await
        .unwrap_err();
    assert_stored_facts_error(&error);
    assert!(error.to_string().contains("has no active snapshot"));

    let (version_id, _) =
        seed_stored_snapshot(&cp, "/srv/missing-link.mkv", serde_json::json!({})).await;
    let mut missing_link = base;
    missing_link.media_snapshots[0].target = PolicyInputTargetRef::FileVersion { id: version_id };
    missing_link.media_snapshots[0].existing_media_snapshot_id =
        Some(voom_core::MediaSnapshotId(999_999));
    let error = cp
        .resolve_stored_planning_input(&policy, missing_link)
        .await
        .unwrap_err();
    assert_stored_facts_error(&error);
    assert!(error.to_string().contains("links missing snapshot"));
}

#[tokio::test]
async fn stored_stream_input_propagates_repository_failures() {
    let (cp, _tmp) = cp().await;
    let (file_version_id, media_snapshot_id) =
        seed_stored_snapshot(&cp, "/srv/repository-error.mkv", serde_json::json!({})).await;
    let created = cp
        .create_policy_input_set_from_scan(PolicyInputFromScanInput {
            slug: "repository-error".to_owned(),
            file_version_id,
            media_snapshot_id,
            container: "mkv".to_owned(),
            video_codec: "h264".to_owned(),
        })
        .await
        .unwrap();
    let input = cp
        .get_policy_input_set(created.input_set_id)
        .await
        .unwrap()
        .unwrap();
    cp.pool_for_test().close().await;

    let error = cp
        .resolve_stored_planning_input(&compiled_policy("container mkv"), input)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "DB_UNREACHABLE");
    assert!(!error.to_string().contains("stored policy stream facts"));
}

#[tokio::test]
async fn stored_stream_plan_rejects_unpublished_condition_before_non_file_member() {
    let (cp, _tmp) = cp().await;
    let created_policy = cp
        .create_policy_document(
            "invalid-before-authority",
            "policy \"invalid before authority\" { phase normalize { \
             when exists audio { container mkv } } }",
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    let mut compiled_json = created_policy.version.compiled_json;
    compiled_json["phases"][0]["operations"][0]["condition"]["filter"] =
        serde_json::json!({"type": "commentary"});
    sqlx::query("DROP TRIGGER policy_versions_are_immutable")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query("UPDATE policy_versions SET compiled_json = ? WHERE id = ?")
        .bind(compiled_json.to_string())
        .bind(i64::try_from(created_policy.version.id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();

    let error = cp
        .plan_accepted_policy_version_with_input_set(created_policy.version.id, input.id)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "PLAN_GENERATION_ERROR");
    assert!(
        error
            .to_string()
            .contains("unpublished compiled stream condition")
    );
    assert!(!error.to_string().contains("stored policy stream facts"));
}

#[tokio::test]
async fn stored_policy_loader_applies_legacy_execution_defaults() {
    let (cp, _tmp) = cp().await;
    let source = "policy \"legacy defaults\" {\n  config {\n    \
        languages: [\"eng\", \"und\"]\n    on_error: continue\n  }\n  \
        phase normalize {}\n}\n";
    let created = cp
        .create_policy_document("legacy-defaults", source)
        .await
        .unwrap();
    let mut version = created.version;
    version.compiled_json["config"] = serde_json::json!({
        "languages": "languages audio: [eng, und]",
        "on_error": "on_error continue"
    });
    version.compiled_json["phases"][0]["on_error"] = serde_json::Value::Null;

    let policy = deserialize_stored_compiled_policy(&version).unwrap();

    assert_eq!(policy.config.languages, ["eng", "und"]);
    assert_eq!(
        policy.phases[0].on_error,
        Some(voom_policy::ErrorStrategy::Continue)
    );
}

#[tokio::test]
async fn stored_policy_loader_rejects_raw_and_typed_unpublished_stream_shapes() {
    let (cp, _tmp) = cp().await;
    let created = cp
        .create_policy_document(
            "stream-shapes",
            "policy \"stream shapes\" { phase normalize { when exists audio { container mkv } } }",
        )
        .await
        .unwrap();
    let mut raw_invalid = created.version.clone();
    raw_invalid.compiled_json["phases"][0]["operations"][0]["condition"]["extra"] =
        serde_json::json!(true);

    let raw_error = deserialize_stored_compiled_policy(&raw_invalid).unwrap_err();

    assert!(
        raw_error
            .to_string()
            .contains("/phases/0/operations/0/condition")
    );
    let mut typed_invalid = created.version;
    typed_invalid.compiled_json["phases"][0]["operations"][0]["condition"]["filter"] =
        serde_json::json!({"type": "commentary"});

    let typed_error = deserialize_stored_compiled_policy(&typed_invalid).unwrap_err();

    assert!(
        typed_error
            .to_string()
            .contains("phase[0:\"normalize\"].operations[0].condition")
    );
    assert!(
        typed_error
            .to_string()
            .contains("exists target=audio filter=present")
    );
}

#[tokio::test]
async fn stored_planning_rejects_unknown_compiled_fields_without_durable_writes() {
    let (cp, _tmp) = cp().await;
    let created_policy = cp
        .create_policy_document(
            "unknown-compiled-field",
            "policy \"unknown compiled field\" { phase normalize { container mkv } }",
        )
        .await
        .unwrap();
    let input = cp
        .create_policy_input_set(load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap())
        .await
        .unwrap();
    sqlx::query("DROP TRIGGER policy_versions_are_immutable")
        .execute(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE policy_versions \
         SET compiled_json = json_set( \
             compiled_json, \
             '$.phases[0].operations[0].future_operation', \
             json('true') \
         ) \
         WHERE id = ?",
    )
    .bind(i64::try_from(created_policy.version.id.0).unwrap())
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    let before = read_only_table_counts(&cp).await;

    let error = cp
        .plan_accepted_policy_version_with_input_set(created_policy.version.id, input.id)
        .await
        .unwrap_err();

    assert_eq!(error.code(), "PLAN_GENERATION_ERROR");
    assert!(
        error
            .to_string()
            .contains("unknown field `future_operation`")
    );
    assert_eq!(before, read_only_table_counts(&cp).await);
}

const PLAN_READ_ONLY_TABLES: &[&str] = &[
    "jobs",
    "tickets",
    "ticket_dependencies",
    "leases",
    "events",
    "issues",
    "issue_links",
    "artifact_handles",
    "artifact_locations",
    "artifact_lineage",
    "policy_versions",
    "policy_input_sets",
    "policy_input_set_fixture_labels",
    "policy_input_synthetic_targets",
    "policy_media_snapshot_inputs",
    "policy_identity_evidence_inputs",
    "policy_bundle_target_inputs",
    "policy_quality_profile_selections",
    "policy_issue_inputs",
];

async fn read_only_table_counts(cp: &crate::ControlPlane) -> Vec<(&'static str, i64)> {
    let mut counts = Vec::with_capacity(PLAN_READ_ONLY_TABLES.len());
    for table in PLAN_READ_ONLY_TABLES {
        counts.push((*table, count_rows(cp, table).await));
    }
    counts
}

async fn count_rows(cp: &crate::ControlPlane, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

#[tokio::test]
async fn dry_run_unknown_named_profile_blocks_before_planning() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "transcode-unknown-profile",
            "policy \"transcode unknown profile\" { phase normalize { transcode video to hevc using profile \"nope\" } }",
        )
        .await
        .unwrap();
    let input_set_id = transcodable_input(&cp, "dry-run-unknown-input").await;

    let err = cp
        .plan_accepted_policy_version_with_input_set(policy.version.id, input_set_id)
        .await
        .unwrap_err();

    assert_eq!(err.code(), "CONFIG_INVALID");
}

#[tokio::test]
async fn dry_run_known_named_profile_resolves_default_hevc_before_planning() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "transcode-default-hevc",
            "policy \"transcode default hevc\" { phase normalize { transcode video to hevc } }",
        )
        .await
        .unwrap();
    let input_set_id = transcodable_input(&cp, "dry-run-default-input").await;

    let plan = cp
        .plan_accepted_policy_version_with_input_set(policy.version.id, input_set_id)
        .await
        .unwrap();

    let node = plan
        .nodes
        .iter()
        .find(|node| node.operation_kind == PlanOperationKind::TranscodeVideo)
        .unwrap();
    assert_eq!(node.status, voom_plan::NodeStatus::Planned);
    assert_eq!(node.operation_payload["profile"], "default-hevc");
    assert_eq!(
        node.operation_payload["resolved_profile"]["encoder"],
        "libx265"
    );
    assert_eq!(node.operation_payload["resolved_profile"]["crf"], 23);
}

#[tokio::test]
async fn dry_run_resolves_named_profile_nested_in_rules() {
    let (cp, _tmp) = cp().await;
    let policy = cp
        .create_policy_document(
            "transcode-rule-default-hevc",
            "policy \"transcode rule default hevc\" { phase normalize { rules first { \
             rule \"encode\" when video.width >= 1 { transcode video to hevc } } } }",
        )
        .await
        .unwrap();
    let input_set_id = transcodable_input(&cp, "dry-run-rule-default-input").await;

    let plan = cp
        .plan_accepted_policy_version_with_input_set(policy.version.id, input_set_id)
        .await
        .unwrap();

    let node = plan
        .nodes
        .iter()
        .find(|node| node.operation_kind == PlanOperationKind::TranscodeVideo)
        .unwrap();
    assert_eq!(node.status, voom_plan::NodeStatus::Planned);
    assert_eq!(node.operation_payload["profile"], "default-hevc");
    assert_eq!(
        node.operation_payload["resolved_profile"]["encoder"],
        "libx265"
    );
}
