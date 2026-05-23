use super::{FixtureName, load_fixture};
use crate::{BundleTargetState, IssueInputState, validate_input_set};

#[test]
fn compliant_fixture_loads_and_validates() {
    let input = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();

    validate_input_set(&input).unwrap();
    assert_eq!(input.slug, "synthetic-compliant-baseline");
    assert_eq!(input.fixture_labels, ["synthetic_compliant_baseline"]);
    assert_eq!(input.synthetic_targets.len(), 6);
    assert_eq!(input.media_snapshots.len(), 1);
    assert_eq!(input.media_snapshots[0].container.as_deref(), Some("mkv"));
    assert_eq!(
        input.media_snapshots[0].video_codec.as_deref(),
        Some("hevc")
    );
    assert_eq!(input.media_snapshots[0].audio_languages, ["en"]);
    assert_eq!(input.media_snapshots[0].subtitle_languages, ["en"]);
    assert!(input.media_snapshots[0].health_flags.is_empty());
    assert_eq!(input.identity_evidence[0].confidence, 0.99);
    assert_eq!(input.bundle_targets.len(), 2);
    assert_eq!(
        input.bundle_targets[0].desired_state,
        BundleTargetState::Required
    );
    assert_eq!(
        input.bundle_targets[1].desired_state,
        BundleTargetState::Required
    );
    assert_eq!(input.quality_profiles[0].profile_name, "balanced-home");
    assert!(input.issues.is_empty());
}

#[test]
fn noncompliant_fixture_loads_and_validates() {
    let input = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    validate_input_set(&input).unwrap();
    assert_eq!(input.slug, "synthetic-noncompliant-transcode-needed");
    assert_eq!(
        input.fixture_labels,
        ["synthetic_noncompliant_transcode_needed"]
    );
    assert_eq!(input.synthetic_targets.len(), 6);
    assert_eq!(input.media_snapshots.len(), 1);
    assert_eq!(input.media_snapshots[0].container.as_deref(), Some("mp4"));
    assert_eq!(
        input.media_snapshots[0].video_codec.as_deref(),
        Some("h264")
    );
    assert!(input.media_snapshots[0].subtitle_languages.is_empty());
    assert_eq!(
        input.media_snapshots[0].stream_summary["facts"]["missing_english_subtitle"],
        true
    );
    assert_eq!(input.identity_evidence[0].confidence, 0.91);
    assert_eq!(input.bundle_targets.len(), 1);
    assert_eq!(
        input.bundle_targets[0].desired_state,
        BundleTargetState::Required
    );
    assert_eq!(input.bundle_targets[0].language.as_deref(), Some("en"));
    assert_eq!(input.quality_profiles[0].profile_name, "balanced-home");
    assert_eq!(input.issues.len(), 1);
    assert_eq!(input.issues[0].kind, "policy_noncompliant");
    assert_eq!(input.issues[0].severity, voom_core::IssueSeverity::Medium);
    assert_eq!(input.issues[0].priority, voom_core::IssuePriority::Normal);
    assert_eq!(input.issues[0].state, IssueInputState::Open);
}

#[test]
fn fixtures_round_trip_through_pretty_json() {
    for name in [
        FixtureName::SyntheticCompliantBaseline,
        FixtureName::SyntheticNoncompliantTranscodeNeeded,
    ] {
        let input = load_fixture(name).unwrap();
        let json = serde_json::to_string_pretty(&input).unwrap();
        let reparsed = serde_json::from_str(&json).unwrap();

        assert_eq!(input, reparsed);
    }
}

#[test]
fn fixture_labels_are_canonical() {
    let compliant = load_fixture(FixtureName::SyntheticCompliantBaseline).unwrap();
    let noncompliant = load_fixture(FixtureName::SyntheticNoncompliantTranscodeNeeded).unwrap();

    assert_eq!(compliant.fixture_labels, ["synthetic_compliant_baseline"]);
    assert_eq!(
        noncompliant.fixture_labels,
        ["synthetic_noncompliant_transcode_needed"]
    );
}

#[test]
fn fixture_names_parse_public_labels() {
    assert_eq!(
        "synthetic_compliant_baseline"
            .parse::<FixtureName>()
            .unwrap(),
        FixtureName::SyntheticCompliantBaseline
    );
    assert_eq!(
        FixtureName::SyntheticNoncompliantTranscodeNeeded.as_str(),
        "synthetic_noncompliant_transcode_needed"
    );
}

#[test]
fn fixture_names_reject_unknown_labels() {
    assert!("unknown_fixture".parse::<FixtureName>().is_err());
}
