use voom_policy::{FixtureName, load_fixture, load_policy_fixture};

use super::*;
use crate::cases::{cp, transcodable_input};

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
        .find(|node| node.operation_kind == "transcode_video")
        .unwrap();
    assert_eq!(node.status, voom_plan::NodeStatus::Planned);
    assert_eq!(node.operation_payload["profile"], "default-hevc");
    assert_eq!(
        node.operation_payload["resolved_profile"]["encoder"],
        "libx265"
    );
    assert_eq!(node.operation_payload["resolved_profile"]["crf"], 23);
}
