#![expect(
    clippy::unwrap_used,
    reason = "integration test setup should fail loudly with direct assertions"
)]

//! Planner-oracle test for the committed `remux-and-hevc` sample policy.
//!
//! For each `(container, video_codec)` input combination the test compiles the
//! sample policy, generates a compliance report against a store-backed
//! `ControlPlane` (so the named `default-hevc` profile resolves), and asserts the
//! set of operation kinds the planner marks `Planned`. The planner is the source
//! of truth: these assertions record what it actually plans, not what a spec
//! assumes.

use serde_json::json;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_plan::{NodeStatus, PlanOperationKind};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, PolicySyntheticTarget,
    TargetKind, TargetRef,
};

const SAMPLE_POLICY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/policies/remux-and-hevc.voom"
);

#[tokio::test]
async fn sample_policy_plans_expected_operations_per_input() {
    let (cp, _db) = open_control_plane().await;
    let source = std::fs::read_to_string(SAMPLE_POLICY_PATH).unwrap();
    let policy = cp
        .create_policy_document("remux-to-mkv-and-transcode-to-hevc", &source)
        .await
        .unwrap();

    let cases: [(&str, &str, &[PlanOperationKind]); 4] = [
        (
            "mp4",
            "h264",
            &[PlanOperationKind::Remux, PlanOperationKind::TranscodeVideo],
        ),
        (
            "mp4",
            "hevc",
            &[PlanOperationKind::Remux, PlanOperationKind::TranscodeVideo],
        ),
        ("mkv", "h264", &[PlanOperationKind::TranscodeVideo]),
        ("mkv", "hevc", &[]),
    ];

    for (container, video_codec, expected) in cases {
        let slug = format!("sample-{container}-{video_codec}");
        let input = cp
            .create_policy_input_set(input_for(&slug, container, video_codec))
            .await
            .unwrap();
        let report = cp
            .generate_compliance_report(policy.version.id, input.id)
            .await
            .unwrap();
        let planned = planned_operation_kinds(&report.plan);
        assert_eq!(
            planned, expected,
            "planned operations for container={container} video_codec={video_codec}"
        );
    }
}

fn planned_operation_kinds(plan: &voom_plan::ExecutionPlan) -> Vec<PlanOperationKind> {
    plan.nodes
        .iter()
        .filter(|node| node.status == NodeStatus::Planned)
        .map(|node| node.operation_kind)
        .collect()
}

async fn open_control_plane() -> (ControlPlane, NamedTempFile) {
    let db = NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    (cp, db)
}

fn input_for(slug: &str, container: &str, video_codec: &str) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec![format!("sample-policy-plan-{slug}")],
        synthetic_targets: vec![PolicySyntheticTarget {
            synthetic_key: "variant-1".to_owned(),
            target_kind: TargetKind::MediaVariant,
            display_name: Some("Synthetic Variant".to_owned()),
        }],
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 0,
            target: TargetRef::Synthetic {
                key: "variant-1".to_owned(),
                kind: TargetKind::MediaVariant,
            },
            container: Some(container.to_owned()),
            stream_summary: json!({"video_stream_count": 1}),
            video_codec: Some(video_codec.to_owned()),
            width: Some(1920),
            height: Some(1080),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
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
