//! Multi-file phase-barrier coordinator (issue #162, Sprint 16 §3/§6).
//!
//! Phase 2 lands the snapshot projection the coordinator uses to feed each
//! phase's planner against the artifact the prior phase committed: it reads a
//! file's active version (chain tip) plus its latest [`MediaSnapshot`] and
//! projects them into a [`MediaSnapshotInput`]. The coordinator core
//! (`run_phase_barrier`) arrives in Phase 3 and reuses these helpers.

use serde_json::Value;
use voom_core::{FileAssetId, VoomError};
use voom_policy::{MediaSnapshotInput, TargetRef};
use voom_store::repo::identity::{FileVersion, IdentityRepo, MediaSnapshot};

use crate::cases::policy_inputs::stream_summary_from_snapshot_payload;

/// First stream in the reprobe payload tagged with the given `kind`.
fn first_stream_of_kind<'a>(payload: &'a Value, kind: &str) -> Option<&'a Value> {
    payload
        .get("streams")
        .and_then(Value::as_array)?
        .iter()
        .find(|stream| stream.get("kind").and_then(Value::as_str) == Some(kind))
}

/// Read a string field off a payload object.
fn payload_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// Read a `u32` field off a payload object (snapshot dimensions are `u64` JSON).
fn payload_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

/// Project a committed file version's reprobe [`MediaSnapshot`] into the planner
/// input the next phase plans against.
///
/// The reprobe payload (`scan::persist::snapshot_with_stream_ids` output) carries
/// `container.format_name` plus a `streams` array whose entries are tagged with a
/// `kind` (`video`/`audio`/`subtitle`). Top-level `container`, `video_codec`,
/// `width`, and `height` are lifted from the container object and the first video
/// stream; the full `streams` array is forwarded verbatim as `stream_summary` so
/// the planner's per-stream readers see refreshed facts.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "first caller is the phase-barrier coordinator core (#162 Phase 3)"
    )
)]
pub(crate) fn project_media_snapshot_input(
    ordinal: u32,
    snapshot: &MediaSnapshot,
) -> MediaSnapshotInput {
    let payload = &snapshot.payload;
    let container = payload
        .get("container")
        .and_then(|container| payload_str(container, "format_name"));
    let video = first_stream_of_kind(payload, "video");
    let video_codec = video.and_then(|stream| payload_str(stream, "codec_name"));
    let width = video.and_then(|stream| payload_u32(stream, "width"));
    let height = video.and_then(|stream| payload_u32(stream, "height"));
    MediaSnapshotInput {
        ordinal,
        target: TargetRef::FileVersion {
            id: snapshot.file_version_id,
        },
        container,
        stream_summary: stream_summary_from_snapshot_payload(payload),
        video_codec,
        width,
        height,
        hdr: None,
        bitrate: None,
        duration_millis: None,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        health_flags: Vec::new(),
        existing_media_snapshot_id: Some(snapshot.id),
    }
}

/// Read a file asset's active version (chain tip = latest non-retired
/// `file_versions` row) and its latest [`MediaSnapshot`].
///
/// Returns `Ok(None)` when the asset has no live version, or when the live tip
/// has no recorded snapshot yet. The coordinator resolves `file_asset_id` from a
/// starting `FileVersionId` via `IdentityRepo::get_file_version`.
///
/// # Errors
/// Propagates repository read errors.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "first caller is the phase-barrier coordinator core (#162 Phase 3)"
    )
)]
pub(crate) async fn active_version_with_snapshot(
    repo: &impl IdentityRepo,
    file_asset_id: FileAssetId,
) -> Result<Option<(FileVersion, MediaSnapshot)>, VoomError> {
    let versions = repo.list_file_versions_by_asset(file_asset_id).await?;
    let Some(tip) = versions
        .into_iter()
        .filter(|version| version.retired_at.is_none())
        .max_by_key(|version| version.id.0)
    else {
        return Ok(None);
    };
    let snapshots = repo.list_media_snapshots_by_version(tip.id).await?;
    let Some(snapshot) = snapshots.into_iter().max_by_key(|snapshot| snapshot.id.0) else {
        return Ok(None);
    };
    Ok(Some((tip, snapshot)))
}

#[cfg(test)]
#[path = "coordinator_test.rs"]
mod tests;
