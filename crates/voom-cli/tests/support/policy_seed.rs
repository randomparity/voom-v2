use voom_control_plane::ControlPlane;
use voom_core::{FileVersionId, MediaSnapshotId};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef, load_policy_fixture,
};

pub struct SeededPolicyIds {
    pub policy_version_id: u64,
    pub input_set_id: u64,
}

/// Create the transcode-hevc policy document plus an input set targeting one
/// scanned source. Callers pass the durable ids published by the scan-session
/// chain (`voom_test_support::scan_seed`) — the CLI no longer reads bytes or
/// emits scanned-file envelopes.
pub async fn seed_transcode_policy(
    cp: &ControlPlane,
    slug: &str,
    container: &str,
    video_codec: &str,
    file_version_id: FileVersionId,
    existing_media_snapshot_id: Option<MediaSnapshotId>,
) -> Result<SeededPolicyIds, Box<dyn std::error::Error>> {
    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom")?,
        )
        .await
        .map_err(|err| std::io::Error::other(format!("create policy document: {err:?}")))?;
    let input = cp
        .create_policy_input_set(PolicyInputSetDraft {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            schema_version: 1,
            source_kind: PolicyInputSourceKind::Test,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            description: None,
            fixture_labels: vec![format!("chaos-librarian-{slug}")],
            synthetic_targets: Vec::new(),
            media_snapshots: vec![MediaSnapshotInput {
                ordinal: 1,
                target: TargetRef::FileVersion {
                    id: file_version_id,
                },
                container: Some(container.to_owned()),
                stream_summary: serde_json::json!({"video_stream_count": 1}),
                video_codec: Some(video_codec.to_owned()),
                width: Some(32),
                height: Some(32),
                hdr: None,
                bitrate: None,
                duration_millis: Some(1000),
                audio_languages: Vec::new(),
                subtitle_languages: Vec::new(),
                health_flags: Vec::new(),
                existing_media_snapshot_id,
            }],
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        })
        .await?;

    Ok(SeededPolicyIds {
        policy_version_id: policy.version.id.0,
        input_set_id: input.id.0,
    })
}
