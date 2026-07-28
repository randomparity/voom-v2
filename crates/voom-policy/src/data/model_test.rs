use super::{
    BundleTargetInput, BundleTargetState, IdentityEvidenceInput, IssueInput, IssueInputState,
    MediaSnapshotInput, POLICY_INPUT_MAX_MEMBERS, POLICY_INPUT_MAX_SERIALIZED_BYTES,
    PolicyInputSetDraft, PolicyInputSourceKind, PolicySyntheticTarget, QualityProfileSelection,
    TargetKind, TargetRef, ValidatedPolicyInputSetDraft, validate_input_set,
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

fn input_with_member_count(member_count: usize) -> PolicyInputSetDraft {
    let mut input = minimal_input_set();
    let fixed_members = input.fixture_labels.len() + input.synthetic_targets.len();
    let snapshot_count = member_count.checked_sub(fixed_members).unwrap();
    let template = input.media_snapshots[0].clone();
    input.media_snapshots = (0..snapshot_count)
        .map(|ordinal| MediaSnapshotInput {
            ordinal: u32::try_from(ordinal).unwrap(),
            ..template.clone()
        })
        .collect();
    input
}

#[test]
fn valid_minimal_input_set_passes() {
    let input = minimal_input_set();

    assert!(validate_input_set(&input).is_ok());
}

#[test]
fn validated_draft_preserves_valid_input() {
    let input = minimal_input_set();

    let validated = ValidatedPolicyInputSetDraft::new(input.clone()).unwrap();

    assert_eq!(validated.as_draft(), &input);
    assert_eq!(validated.into_draft(), input);
}

#[test]
fn invalid_input_cannot_produce_validated_draft() {
    let mut input = minimal_input_set();
    input.slug = " ".to_owned();

    assert!(ValidatedPolicyInputSetDraft::new(input).is_err());
}

#[test]
fn empty_scan_draft_uses_explicit_validation_without_weakening_generic_validation() {
    let mut input = minimal_input_set();
    input.source_kind = PolicyInputSourceKind::Imported;
    input.synthetic_targets.clear();
    input.media_snapshots.clear();

    assert!(matches!(
        ValidatedPolicyInputSetDraft::new(input.clone()),
        Err(super::PolicyInputSetValidationError::MissingSnapshotOrBundleTarget)
    ));

    let validated = ValidatedPolicyInputSetDraft::new_empty_scan(input.clone()).unwrap();
    assert_eq!(validated.into_draft(), input);
}

#[test]
fn empty_scan_validation_rejects_non_imported_or_member_bearing_drafts() {
    let mut non_imported = minimal_input_set();
    non_imported.synthetic_targets.clear();
    non_imported.media_snapshots.clear();
    assert!(ValidatedPolicyInputSetDraft::new_empty_scan(non_imported).is_err());

    let mut member_bearing = minimal_input_set();
    member_bearing.source_kind = PolicyInputSourceKind::Imported;
    assert!(ValidatedPolicyInputSetDraft::new_empty_scan(member_bearing).is_err());
}

#[test]
fn aggregate_member_budget_accepts_boundary_and_rejects_one_over() {
    let boundary = input_with_member_count(POLICY_INPUT_MAX_MEMBERS);
    assert!(validate_input_set(&boundary).is_ok());

    let over = input_with_member_count(POLICY_INPUT_MAX_MEMBERS + 1);
    let err = validate_input_set(&over).unwrap_err();

    assert_eq!(
        err.message(),
        format!(
            "policy input aggregate has {} members; maximum is {}",
            POLICY_INPUT_MAX_MEMBERS + 1,
            POLICY_INPUT_MAX_MEMBERS
        )
    );
}

#[test]
fn aggregate_serialized_budget_accepts_boundary_and_rejects_one_over() {
    let mut input = minimal_input_set();
    input.description = Some(String::new());
    let base_size = serde_json::to_vec(&input).unwrap().len();
    input.description = Some("x".repeat(POLICY_INPUT_MAX_SERIALIZED_BYTES - base_size));
    assert_eq!(
        serde_json::to_vec(&input).unwrap().len(),
        POLICY_INPUT_MAX_SERIALIZED_BYTES
    );
    assert!(validate_input_set(&input).is_ok());

    input.description.as_mut().unwrap().push('x');
    let err = validate_input_set(&input).unwrap_err();

    assert_eq!(
        err.message(),
        format!(
            "policy input aggregate serializes to {} bytes; maximum is {}",
            POLICY_INPUT_MAX_SERIALIZED_BYTES + 1,
            POLICY_INPUT_MAX_SERIALIZED_BYTES
        )
    );
}

#[test]
fn policy_enum_as_str_returns_wire_values() {
    assert_eq!(PolicyInputSourceKind::Fixture.as_str(), "fixture");
    assert_eq!(PolicyInputSourceKind::Test.as_str(), "test");
    assert_eq!(PolicyInputSourceKind::Imported.as_str(), "imported");
    assert_eq!(PolicyInputSourceKind::Manual.as_str(), "manual");

    assert_eq!(TargetKind::MediaWork.as_str(), "media_work");
    assert_eq!(TargetKind::MediaVariant.as_str(), "media_variant");
    assert_eq!(TargetKind::AssetBundle.as_str(), "asset_bundle");
    assert_eq!(TargetKind::FileAsset.as_str(), "file_asset");
    assert_eq!(TargetKind::FileVersion.as_str(), "file_version");
    assert_eq!(TargetKind::FileLocation.as_str(), "file_location");

    assert_eq!(BundleTargetState::Required.as_str(), "required");
    assert_eq!(BundleTargetState::Allowed.as_str(), "allowed");
    assert_eq!(BundleTargetState::Forbidden.as_str(), "forbidden");
    assert_eq!(BundleTargetState::Preferred.as_str(), "preferred");

    assert_eq!(IssueInputState::Open.as_str(), "open");
    assert_eq!(IssueInputState::Accepted.as_str(), "accepted");
    assert_eq!(IssueInputState::Suppressed.as_str(), "suppressed");
    assert_eq!(IssueInputState::Planned.as_str(), "planned");
}

#[test]
fn empty_slug_is_rejected() {
    let mut input = minimal_input_set();
    input.slug = "   ".to_owned();

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn zero_schema_version_is_rejected() {
    let mut input = minimal_input_set();
    input.schema_version = 0;

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::InvalidSchemaVersion)
    ));
}

#[test]
fn slug_must_be_a_stable_token() {
    let mut input = minimal_input_set();
    input.slug = "bad slug".to_owned();

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::InvalidSlug { slug }) if slug == "bad slug"
    ));
}

#[test]
fn duplicate_fixture_label_is_rejected() {
    let mut input = minimal_input_set();
    input.fixture_labels = vec!["dup".to_owned(), "dup".to_owned()];

    assert!(validate_input_set(&input).is_err());
}

#[test]
fn fixture_labels_must_be_stable_tokens() {
    let mut input = minimal_input_set();
    input.fixture_labels = vec!["not stable".to_owned()];

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::InvalidFixtureLabel { label })
            if label == "not stable"
    ));
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
fn synthetic_key_must_be_a_stable_token() {
    let mut input = minimal_input_set();
    input.synthetic_targets[0].synthetic_key = "work 1".to_owned();
    input.media_snapshots[0].target = TargetRef::Synthetic {
        key: "work 1".to_owned(),
        kind: TargetKind::MediaWork,
    };

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::InvalidSyntheticKey { key })
            if key == "work 1"
    ));
}

#[test]
fn duplicate_synthetic_key_with_same_kind_is_rejected() {
    let mut input = minimal_input_set();
    input.synthetic_targets.push(PolicySyntheticTarget {
        synthetic_key: "work-1".to_owned(),
        target_kind: TargetKind::MediaWork,
        display_name: None,
    });

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::DuplicateSyntheticTarget { key })
            if key == "work-1"
    ));
}

#[test]
fn duplicate_child_ordinal_within_same_input_area_is_rejected() {
    let mut input = minimal_input_set();
    let mut duplicate = input.media_snapshots[0].clone();
    duplicate.container = Some("mp4".to_owned());
    input.media_snapshots.push(duplicate);

    assert!(matches!(
        validate_input_set(&input),
        Err(super::PolicyInputSetValidationError::DuplicateChildOrdinal {
            collection,
            ordinal: 0,
        }) if collection == "media_snapshots"
    ));
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
