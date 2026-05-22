use super::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, PolicySyntheticTarget,
    QualityProfileSelection, TargetKind, TargetRef, validate_input_set,
};

fn minimal_input_set() -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: "minimal-policy-inputs".to_owned(),
        display_name: "Minimal policy inputs".to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec!["minimal".to_owned()],
        synthetic_targets: vec![PolicySyntheticTarget {
            synthetic_key: "work-1".to_owned(),
            target_kind: TargetKind::MediaWork,
            display_name: Some("Work 1".to_owned()),
        }],
        media_snapshots: vec![MediaSnapshotInput {
            ordinal: 0,
            target: TargetRef::Synthetic {
                key: "work-1".to_owned(),
                kind: TargetKind::MediaWork,
            },
            container: Some("mkv".to_owned()),
            stream_summary: serde_json::json!({"streams": 1}),
            video_codec: Some("hevc".to_owned()),
            width: Some(1920),
            height: Some(1080),
            hdr: None,
            bitrate: Some(8_000_000),
            duration_millis: Some(7_200_000),
            audio_languages: vec!["en".to_owned()],
            subtitle_languages: vec!["en".to_owned()],
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
fn valid_minimal_input_set_passes() {
    let input = minimal_input_set();

    assert!(validate_input_set(&input).is_ok());
}

#[test]
fn empty_slug_is_rejected() {
    let mut input = minimal_input_set();
    input.slug = "   ".to_owned();

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn duplicate_fixture_label_is_rejected() {
    let mut input = minimal_input_set();
    input.fixture_labels = vec!["dup".to_owned(), "dup".to_owned()];

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn input_set_without_snapshot_or_bundle_target_is_rejected() {
    let mut input = minimal_input_set();
    input.media_snapshots.clear();

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn undeclared_synthetic_target_is_rejected() {
    let mut input = minimal_input_set();
    input.media_snapshots[0].target = TargetRef::Synthetic {
        key: "missing".to_owned(),
        kind: TargetKind::MediaWork,
    };

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn synthetic_key_reused_with_different_kind_is_rejected() {
    let mut input = minimal_input_set();
    input.synthetic_targets.push(PolicySyntheticTarget {
        synthetic_key: "work-1".to_owned(),
        target_kind: TargetKind::MediaVariant,
        display_name: None,
    });

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn evidence_confidence_out_of_range_is_rejected() {
    let mut input = minimal_input_set();
    input.identity_evidence.push(IdentityEvidenceInput {
        ordinal: 0,
        target: TargetRef::Synthetic {
            key: "work-1".to_owned(),
            kind: TargetKind::MediaWork,
        },
        assertion_type: "match".to_owned(),
        provider: "fixture".to_owned(),
        provider_version: "1".to_owned(),
        confidence: 1.1,
        provenance: serde_json::json!({"source": "test"}),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
        existing_evidence_id: None,
    });

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn empty_provider_and_profile_names_are_rejected() {
    let mut empty_provider = minimal_input_set();
    empty_provider
        .identity_evidence
        .push(IdentityEvidenceInput {
            ordinal: 0,
            target: TargetRef::Synthetic {
                key: "work-1".to_owned(),
                kind: TargetKind::MediaWork,
            },
            assertion_type: "match".to_owned(),
            provider: String::new(),
            provider_version: "1".to_owned(),
            confidence: 0.5,
            provenance: serde_json::json!({"source": "test"}),
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            existing_evidence_id: None,
        });

    let mut empty_profile = minimal_input_set();
    empty_profile
        .quality_profiles
        .push(QualityProfileSelection {
            ordinal: 0,
            target: TargetRef::Synthetic {
                key: "work-1".to_owned(),
                kind: TargetKind::MediaWork,
            },
            profile_name: " ".to_owned(),
            profile_version: "1".to_owned(),
            dimension_weights: serde_json::json!({}),
        });

    assert!(validate_input_set(&empty_provider).is_err());
    assert!(validate_input_set(&empty_profile).is_err());
}

#[test]
fn bundle_target_issue_types_are_part_of_the_model_surface() {
    let bundle_target = BundleTargetInput {
        ordinal: 0,
        target: TargetRef::MediaVariant {
            id: voom_core::MediaVariantId(1),
        },
        role: "subtitle".to_owned(),
        desired_state: BundleTargetState::Required,
        language: Some("en".to_owned()),
        label: None,
        disposition: None,
        artifact_expectation: serde_json::json!({}),
    };
    let issue = IssueInput {
        ordinal: 0,
        target: TargetRef::MediaVariant {
            id: voom_core::MediaVariantId(1),
        },
        kind: "policy_noncompliant".to_owned(),
        severity: voom_core::IssueSeverity::Medium,
        priority: voom_core::IssuePriority::Normal,
        state: IssueInputState::Open,
        reason: "missing subtitle".to_owned(),
        provenance: serde_json::json!({}),
        existing_issue_id: None,
    };

    assert_eq!(bundle_target.desired_state, BundleTargetState::Required);
    assert_eq!(issue.state, IssueInputState::Open);
}
