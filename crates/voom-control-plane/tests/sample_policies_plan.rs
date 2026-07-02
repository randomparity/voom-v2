#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration test setup should fail loudly with direct assertions"
)]

//! Planner-oracle tests for the committed sample real-media policies
//! (`crates/voom-control-plane/tests/fixtures/policies/*.voom`, issue #288).
//!
//! Each committed sample is proof that the V1(+V1.1) DSL vocabulary compiles and
//! plans end-to-end. For every sample these tests compile the policy through a
//! store-backed `ControlPlane` (so named video profiles resolve), generate a
//! compliance report against a synthetic input, and pin the per-phase set of
//! `(operation kind, node status)` the planner emits. The planner is the source
//! of truth: the assertions record what it actually plans, not what a spec
//! assumes.
//!
//! The final test drives the flagship `reference-user` policy over a small mixed
//! library (a normal file, an untagged-language file, and a no-matching-language
//! file) and pins the per-file plan, including the language-filter edge cases
//! from ADR 0021 (#272): an untagged track is matched as `und` (with a per-file
//! warning) and a no-match language filter blocks only that one file's audio
//! transcode while the rest of the library still plans. The execution-time
//! "never leave a file with no audio" guard that turns a no-match remux keep into
//! a per-file terminal failure is covered by the control-plane unit test
//! `keep_audio_matching_zero_tracks_rejects_empty_audio`.

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_plan::{ExecutionPlan, NodeStatus, PlanOperationKind, PlanningDiagnosticCode};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, PolicySyntheticTarget,
    TargetKind, TargetRef,
};

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/policies/");

#[tokio::test]
async fn container_normalize_sample_plans_expected_operations_per_input() {
    let (cp, _db) = open_control_plane().await;
    let policy = create_policy(&cp, "container-normalize", "container-normalize.voom").await;

    // Container-only normalize: a non-mkv source is remuxed; an mkv source with
    // the same tracks is a complete no-op.
    for (container, expected) in [
        ("mp4", vec![(PlanOperationKind::Remux, NodeStatus::Planned)]),
        ("mkv", vec![(PlanOperationKind::Remux, NodeStatus::NoOp)]),
    ] {
        let input = single_input(
            &format!("container-{container}"),
            snapshot(container, "h264", stereo_eng()),
        );
        let plan = report(&cp, policy, input).await;
        assert_eq!(
            phase_ops(&plan, "normalize"),
            expected,
            "container={container}"
        );
    }
}

#[tokio::test]
async fn language_cleanup_sample_plans_expected_operations_per_input() {
    let (cp, _db) = open_control_plane().await;
    let policy = create_policy(&cp, "language-cleanup", "language-cleanup.voom").await;

    // A file carrying a non-preferred-language track (jpn) is remuxed to drop it.
    let input = single_input(
        "language-mixed",
        snapshot(
            "mkv",
            "h264",
            vec![
                audio_stream("a-1", 1, "aac", Some("eng"), 6, true),
                audio_stream("a-2", 2, "aac", Some("jpn"), 2, false),
            ],
        ),
    );
    let plan = report(&cp, policy, input).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::Planned)],
    );

    // A file already carrying only the preferred, defaulted track is a no-op.
    let input = single_input(
        "language-clean",
        snapshot(
            "mkv",
            "h264",
            vec![audio_stream("a-1", 1, "aac", Some("eng"), 2, true)],
        ),
    );
    let plan = report(&cp, policy, input).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::NoOp)],
    );
}

#[tokio::test]
async fn reference_user_sample_plans_the_full_pipeline() {
    let (cp, _db) = open_control_plane().await;
    let policy = create_policy(
        &cp,
        "reference-user-library-normalize",
        "reference-user.voom",
    )
    .await;

    // A fresh mp4/h264 file with a 5.1 English track and a stereo Japanese track:
    // every phase does real work — remux to mkv (dropping jpn), transcode video
    // to hevc, transcode the kept audio to eac3, synthesize a stereo downmix, and
    // verify the result.
    let fresh = single_input(
        "reference-fresh",
        snapshot(
            "mp4",
            "h264",
            vec![
                audio_stream("a-1", 1, "aac", Some("eng"), 6, true),
                audio_stream("a-2", 2, "aac", Some("jpn"), 2, false),
            ],
        ),
    );
    let plan = report(&cp, policy, fresh).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "video"),
        vec![(PlanOperationKind::TranscodeVideo, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "audio"),
        vec![
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
        ],
    );
    assert_eq!(
        phase_ops(&plan, "verify"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );

    // Pin the reference-user *parameters*, not just the pipeline shape — the
    // samples are the proof the DSL targets these exact formats. This also
    // disambiguates the two same-kind audio nodes by their payload `type`.
    let remux = only_node(&plan, "normalize", |_| true);
    assert_eq!(remux.operation_payload["container"], "mkv");

    let video = only_node(&plan, "video", |_| true);
    assert_eq!(video.operation_payload["target_codec"], "hevc");
    assert_eq!(video.operation_payload["container"], "mkv");

    let eac3 = only_node(&plan, "audio", |node| {
        node.operation_payload["type"] == "transcode_audio"
    });
    assert_eq!(eac3.operation_payload["target_codec"], "eac3");
    assert_eq!(eac3.operation_payload["container"], "mkv");

    let downmix = only_node(&plan, "audio", |node| {
        node.operation_payload["type"] == "synthesize_audio"
    });
    assert_eq!(downmix.operation_payload["target_codec"], "aac");
    assert_eq!(downmix.operation_payload["target_channels"], 2);

    // An already-normalized mkv/hevc file with a 5.1 eac3 track: container,
    // video, and audio transcode are all no-ops. The stereo downmix is still
    // planned — synthesis always *adds* a companion track, so it is never a
    // no-op (ADR 0026). Verification always runs.
    let normalized = single_input(
        "reference-normalized",
        snapshot(
            "mkv",
            "hevc",
            vec![audio_stream("a-1", 1, "eac3", Some("eng"), 6, true)],
        ),
    );
    let plan = report(&cp, policy, normalized).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::NoOp)],
    );
    assert_eq!(
        phase_ops(&plan, "video"),
        vec![(PlanOperationKind::TranscodeVideo, NodeStatus::NoOp)],
    );
    assert_eq!(
        phase_ops(&plan, "audio"),
        vec![
            (PlanOperationKind::TranscodeAudio, NodeStatus::NoOp),
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
        ],
    );
    assert_eq!(
        phase_ops(&plan, "verify"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );
}

#[tokio::test]
async fn verify_heavy_sample_plans_verification_between_mutating_phases() {
    let (cp, _db) = open_control_plane().await;
    let policy = create_policy(&cp, "verify-heavy-normalize", "verify-heavy.voom").await;

    // Fresh mp4/h264: remux, then verify, then transcode video, then verify.
    let fresh = single_input("verify-fresh", snapshot("mp4", "h264", stereo_eng()));
    let plan = report(&cp, policy, fresh).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "verify_container"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "transcode"),
        vec![(PlanOperationKind::TranscodeVideo, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "verify_transcode"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );

    // Already-normalized mkv/hevc: the mutating phases are no-ops, but both
    // verifications still run — a verify is never elided.
    let normalized = single_input("verify-normalized", snapshot("mkv", "hevc", stereo_eng()));
    let plan = report(&cp, policy, normalized).await;
    assert_eq!(
        phase_ops(&plan, "normalize"),
        vec![(PlanOperationKind::Remux, NodeStatus::NoOp)],
    );
    assert_eq!(
        phase_ops(&plan, "verify_container"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );
    assert_eq!(
        phase_ops(&plan, "transcode"),
        vec![(PlanOperationKind::TranscodeVideo, NodeStatus::NoOp)],
    );
    assert_eq!(
        phase_ops(&plan, "verify_transcode"),
        vec![(PlanOperationKind::VerifyArtifact, NodeStatus::Planned)],
    );
}

#[tokio::test]
async fn reference_user_over_mixed_library_isolates_untagged_and_no_match() {
    let (cp, _db) = open_control_plane().await;
    let policy = create_policy(
        &cp,
        "reference-user-library-normalize",
        "reference-user.voom",
    )
    .await;

    // A small mkv/h264 library: a normal English-tagged file, a file whose audio
    // carries no language tag, and a file whose only audio is a non-preferred
    // language (fra). Each is its own target so the plan carries per-file nodes.
    let normal = "movie-eng";
    let untagged = "movie-untagged";
    let no_match = "movie-fra";
    let input = library_input(
        "reference-mixed-library",
        vec![
            (
                normal,
                snapshot_for(
                    normal,
                    "mkv",
                    "h264",
                    vec![audio_stream("a-1", 1, "aac", Some("eng"), 6, true)],
                ),
            ),
            (
                untagged,
                snapshot_for(
                    untagged,
                    "mkv",
                    "h264",
                    vec![audio_stream("a-1", 1, "aac", None, 6, true)],
                ),
            ),
            (
                no_match,
                snapshot_for(
                    no_match,
                    "mkv",
                    "h264",
                    vec![audio_stream("a-1", 1, "aac", Some("fra"), 6, true)],
                ),
            ),
        ],
    );
    let plan = report(&cp, policy, input).await;

    // The normal file: its kept English audio is transcoded to eac3 and a stereo
    // downmix is synthesized.
    assert_eq!(
        target_phase_ops(&plan, normal, "audio"),
        vec![
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
        ],
    );

    // The untagged file: the untagged track is matched as `und` (which the policy
    // keeps and transcodes), and the planner attaches a per-file warning that the
    // language was defaulted (ADR 0021 / #272). The file is not blocked.
    assert_eq!(
        target_phase_ops(&plan, untagged, "audio"),
        vec![
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
        ],
    );
    let untagged_warnings = plan
        .diagnostics
        .iter()
        .filter(|d| {
            d.code == PlanningDiagnosticCode::UntaggedTrackLanguageDefaulted
                && d.target.as_ref() == Some(&synthetic_target(untagged))
        })
        .count();
    assert!(
        untagged_warnings >= 1,
        "expected an untagged-language warning for {untagged}; diagnostics={:?}",
        plan.diagnostics
    );

    // The no-match file: the fra track satisfies neither eng nor und, so the
    // language-filtered audio transcode is blocked for this file only, with an
    // error diagnostic. The synthesized downmix (no language filter) still plans,
    // and the other files above are unaffected — the odd file is isolated, not a
    // library-wide block.
    assert_eq!(
        target_phase_ops(&plan, no_match, "audio"),
        vec![
            (PlanOperationKind::TranscodeAudio, NodeStatus::Blocked),
            (PlanOperationKind::TranscodeAudio, NodeStatus::Planned),
        ],
    );
    assert!(
        plan.diagnostics.iter().any(|d| {
            d.operation_kind.as_deref() == Some("transcode_audio")
                && d.target.as_ref() == Some(&synthetic_target(no_match))
        }),
        "expected a per-file transcode_audio diagnostic for {no_match}; diagnostics={:?}",
        plan.diagnostics
    );
}

// --- helpers ---------------------------------------------------------------

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

async fn create_policy(cp: &ControlPlane, slug: &str, fixture: &str) -> voom_core::PolicyVersionId {
    let source = std::fs::read_to_string(format!("{FIXTURE_DIR}{fixture}")).unwrap();
    cp.create_policy_document(slug, &source)
        .await
        .unwrap()
        .version
        .id
}

async fn report(
    cp: &ControlPlane,
    policy_version_id: voom_core::PolicyVersionId,
    input: PolicyInputSetDraft,
) -> ExecutionPlan {
    let input = cp.create_policy_input_set(input).await.unwrap();
    cp.generate_compliance_report(policy_version_id, input.id)
        .await
        .unwrap()
        .plan
}

/// Pins the multiset of `(operation kind, node status)` planned in a phase,
/// sorted for a stable comparison.
fn phase_ops(plan: &ExecutionPlan, phase: &str) -> Vec<(PlanOperationKind, NodeStatus)> {
    let mut ops = plan
        .nodes
        .iter()
        .filter(|node| node.phase_name == phase)
        .map(|node| (node.operation_kind, node.status))
        .collect::<Vec<_>>();
    ops.sort_by_key(|(kind, status)| (kind.as_str(), format!("{status:?}")));
    ops
}

/// Returns the single node in a phase matching `predicate`, panicking unless
/// exactly one matches — used to pin an operation's payload parameters.
fn only_node<'a>(
    plan: &'a ExecutionPlan,
    phase: &str,
    predicate: impl Fn(&&'a voom_plan::PlanNode) -> bool,
) -> &'a voom_plan::PlanNode {
    let mut matches = plan
        .nodes
        .iter()
        .filter(|node| node.phase_name == phase)
        .filter(predicate);
    let Some(node) = matches.next() else {
        panic!("no node matched in phase {phase}");
    };
    assert!(
        matches.next().is_none(),
        "more than one node matched in phase {phase}"
    );
    node
}

/// Same as [`phase_ops`] but scoped to a single library target.
fn target_phase_ops(
    plan: &ExecutionPlan,
    target_key: &str,
    phase: &str,
) -> Vec<(PlanOperationKind, NodeStatus)> {
    let target = synthetic_target(target_key);
    let mut ops = plan
        .nodes
        .iter()
        .filter(|node| node.phase_name == phase && node.target == target)
        .map(|node| (node.operation_kind, node.status))
        .collect::<Vec<_>>();
    ops.sort_by_key(|(kind, status)| (kind.as_str(), format!("{status:?}")));
    ops
}

fn synthetic_target(key: &str) -> TargetRef {
    TargetRef::Synthetic {
        key: key.to_owned(),
        kind: TargetKind::MediaVariant,
    }
}

fn audio_stream(
    id: &str,
    index: u64,
    codec: &str,
    language: Option<&str>,
    channels: u64,
    default: bool,
) -> Value {
    let mut stream = json!({
        "id": id,
        "index": index,
        "kind": "audio",
        "codec_name": codec,
        "channels": channels,
        "disposition": {"default": default, "forced": false, "commentary": false},
    });
    if let Some(language) = language {
        stream["language"] = json!(language);
    }
    stream
}

fn stereo_eng() -> Vec<Value> {
    vec![audio_stream("a-1", 1, "aac", Some("eng"), 2, true)]
}

/// A single-target media snapshot with one video stream plus the given audio
/// streams, keyed to the `variant-1` synthetic target.
fn snapshot(container: &str, video_codec: &str, audio: Vec<Value>) -> MediaSnapshotInput {
    snapshot_for("variant-1", container, video_codec, audio)
}

fn snapshot_for(
    target_key: &str,
    container: &str,
    video_codec: &str,
    audio: Vec<Value>,
) -> MediaSnapshotInput {
    let mut streams = vec![json!({
        "id": format!("{target_key}-v-0"),
        "index": 0,
        "kind": "video",
        "codec_name": video_codec,
    })];
    streams.extend(audio);
    MediaSnapshotInput {
        ordinal: 0,
        target: synthetic_target(target_key),
        container: Some(container.to_owned()),
        stream_summary: json!({"video_stream_count": 1, "streams": streams}),
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
    }
}

fn single_input(slug: &str, snapshot: MediaSnapshotInput) -> PolicyInputSetDraft {
    let target_key = match &snapshot.target {
        TargetRef::Synthetic { key, .. } => key.clone(),
        _ => panic!("synthetic target expected"),
    };
    draft(
        slug,
        vec![PolicySyntheticTarget {
            synthetic_key: target_key,
            target_kind: TargetKind::MediaVariant,
            display_name: None,
        }],
        vec![snapshot],
    )
}

fn library_input(slug: &str, entries: Vec<(&str, MediaSnapshotInput)>) -> PolicyInputSetDraft {
    let mut targets = Vec::new();
    let mut snapshots = Vec::new();
    for (ordinal, (key, mut snapshot)) in entries.into_iter().enumerate() {
        targets.push(PolicySyntheticTarget {
            synthetic_key: key.to_owned(),
            target_kind: TargetKind::MediaVariant,
            display_name: None,
        });
        snapshot.ordinal = u32::try_from(ordinal).unwrap();
        snapshots.push(snapshot);
    }
    draft(slug, targets, snapshots)
}

fn draft(
    slug: &str,
    synthetic_targets: Vec<PolicySyntheticTarget>,
    media_snapshots: Vec<MediaSnapshotInput>,
) -> PolicyInputSetDraft {
    PolicyInputSetDraft {
        slug: slug.to_owned(),
        display_name: slug.to_owned(),
        schema_version: 1,
        source_kind: PolicyInputSourceKind::Test,
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        description: None,
        fixture_labels: vec![format!("sample-policy-{slug}")],
        synthetic_targets,
        media_snapshots,
        identity_evidence: Vec::new(),
        bundle_targets: Vec::new(),
        quality_profiles: Vec::new(),
        issues: Vec::new(),
    }
}
