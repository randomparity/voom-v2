use serde_json::json;
use voom_core::{FileVersionId, MediaSnapshotId};
use voom_policy::{
    FixtureName, MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef,
    load_fixture, load_policy_fixture,
};

use crate::{
    ExecutionPlan, PlanningContext, PlanningRequest, generate_compliance_report, generate_plan,
};

use super::*;

const REMUX_TRACK_SELECTION_POLICY: &str = r#"
policy "remux track selection" {
  phase normalize {
    container mkv
    keep audio where lang in [eng, und]
    remove subtitle where forced
    order tracks [video, audio, subtitle]
    defaults audio: first
    defaults subtitle: none
  }
}
"#;

#[test]
fn compliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_compliant_baseline".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_compliant").unwrap()
    );
}

#[test]
fn noncompliant_container_fixture_matches_golden_plan() {
    let policy_source = load_policy_fixture("fixtures/policies/container-metadata.voom").unwrap();
    let compiled = voom_policy::compile_policy(&policy_source).unwrap().policy;
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input,
        context: PlanningContext {
            input_source_label: Some("synthetic_noncompliant_transcode_needed".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("container_metadata_noncompliant").unwrap()
    );
}

#[test]
fn remux_track_selection_fixture_matches_golden_plan() {
    let compiled = voom_policy::compile_policy(REMUX_TRACK_SELECTION_POLICY)
        .unwrap()
        .policy;

    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input: remux_track_selection_input(),
        context: PlanningContext {
            input_source_label: Some("remux_track_selection".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        load_golden_plan("remux_track_selection").unwrap()
    );
}

#[test]
fn remux_track_selection_report_matches_golden_report() {
    let compiled = voom_policy::compile_policy(REMUX_TRACK_SELECTION_POLICY)
        .unwrap()
        .policy;
    let plan = generate_plan(PlanningRequest {
        policy: compiled,
        input: remux_track_selection_input(),
        context: PlanningContext {
            input_source_label: Some("remux_track_selection".to_owned()),
            ..PlanningContext::default()
        },
    })
    .unwrap();

    let report = generate_compliance_report(&plan).unwrap();

    assert_eq!(
        serde_json::to_value(&report).unwrap(),
        load_golden_compliance_report("remux_track_selection").unwrap()
    );
}

#[test]
fn golden_plans_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
        "remux_track_selection",
    ] {
        let value = load_golden_plan(name).unwrap();
        serde_json::from_value::<ExecutionPlan>(value).unwrap();
    }
}

#[test]
fn golden_compliance_reports_deserialize_through_public_type() {
    for name in [
        "container_metadata_compliant",
        "container_metadata_noncompliant",
        "container_metadata_blocked",
        "container_metadata_mixed",
        "remux_track_selection",
    ] {
        let value = load_golden_compliance_report(name).unwrap();
        serde_json::from_value::<crate::ComplianceReport>(value).unwrap();
    }
}

fn remux_track_selection_input() -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: "remux-track-selection".to_owned(),
        display_name: "remux track selection".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec!["remux_track_selection".to_owned()],
        synthetic_targets: Vec::new(),
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 1,
            target: TargetRef::FileVersion {
                id: FileVersionId(7),
            },
            container: Some("mkv".to_owned()),
            stream_summary: json!({
                "video_stream_count": 1,
                "streams": [
                    {"id": "stream-0", "index": 0, "kind": "video", "codec_name": "h264"},
                    {
                        "id": "stream-1",
                        "index": 1,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "eng",
                        "disposition": {"default": false, "forced": false}
                    },
                    {
                        "id": "stream-2",
                        "index": 2,
                        "kind": "audio",
                        "codec_name": "aac",
                        "language": "spa",
                        "disposition": {"default": false, "forced": false}
                    },
                    {
                        "id": "stream-3",
                        "index": 3,
                        "kind": "subtitle",
                        "codec_name": "subrip",
                        "language": "eng",
                        "disposition": {"default": true, "forced": false}
                    },
                    {
                        "id": "stream-4",
                        "index": 4,
                        "kind": "subtitle",
                        "codec_name": "subrip",
                        "language": "spa",
                        "disposition": {"default": false, "forced": true}
                    }
                ]
            }),
            video_codec: Some("h264".to_owned()),
            width: Some(32),
            height: Some(32),
            hdr: None,
            bitrate: None,
            duration_millis: Some(1000),
            audio_languages: vec!["eng".to_owned(), "spa".to_owned()],
            subtitle_languages: vec!["eng".to_owned(), "spa".to_owned()],
            health_flags: Vec::new(),
            existing_media_snapshot_id: Some(MediaSnapshotId(42)),
        }],
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}

#[test]
fn unknown_golden_plan_name_fails_loudly() {
    let err = load_golden_plan("typo").unwrap_err();

    assert!(matches!(err, GoldenPlanFixtureError::UnknownFixture(name) if name == "typo"));
}
