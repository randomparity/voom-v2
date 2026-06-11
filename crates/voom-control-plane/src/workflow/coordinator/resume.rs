//! Resume reconciliation and chain-tip/snapshot projection.
//!
//! Reconciles a new resume job against the most-recently-failed run's per-`(file,
//! phase)` rows (ADR-0009), derives stable per-file branch ids, and projects a
//! committed file version's reprobe snapshot into the planner input the next
//! phase plans against.

use voom_core::{FileAssetId, FileVersionId, JobId, VoomError};
use voom_policy::{MediaSnapshotInput, TargetRef};
use voom_store::repo::identity::{FileLocationKind, FileVersion, IdentityRepo, MediaSnapshot};
use voom_store::repo::workflow_summaries::{FilePhaseOutcome, FilePhaseSummary};

use crate::ControlPlane;
use crate::cases::policy::policy_inputs::stream_summary_from_snapshot_payload;
use crate::workflow::coordinator::PhaseFile;
use crate::workflow::coordinator::finalize::{
    ProducedRefs, first_stream_of_kind, payload_str, payload_u32,
};
use crate::workflow::coordinator::promotion::ensure_unique_active_branch_ids;
use crate::workflow::plan::expansion::branch_ids_from_paths;

impl ControlPlane {
    /// Derive stable branch ids for active files, disambiguating colliding path
    /// stems while preserving stem-only ids for non-colliding paths.
    pub(super) async fn active_branch_ids(
        &self,
        active: &[FileVersionId],
    ) -> Result<Vec<(FileVersionId, String)>, VoomError> {
        let mut paths = Vec::with_capacity(active.len());
        for &file_version_id in active {
            paths.push((
                file_version_id,
                self.file_branch_path(file_version_id).await?,
            ));
        }
        let path_values = paths
            .iter()
            .map(|(_, path)| path.clone())
            .collect::<Vec<_>>();
        let branch_ids = branch_ids_from_paths(&path_values)?;
        let branch_ids = paths
            .into_iter()
            .zip(branch_ids)
            .map(|((file_version_id, _), branch_id)| (file_version_id, branch_id))
            .collect::<Vec<_>>();
        ensure_unique_active_branch_ids(&branch_ids)?;
        Ok(branch_ids)
    }

    async fn file_branch_path(&self, file_version_id: FileVersionId) -> Result<String, VoomError> {
        let locations = self
            .identity
            .list_live_file_locations_by_version(file_version_id)
            .await?;
        let path = locations
            .iter()
            .find(|location| location.kind == FileLocationKind::LocalPath)
            .or_else(|| locations.first())
            .map(|location| location.value.clone())
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "file version {file_version_id} has no live location to derive a branch id"
                ))
            })?;
        Ok(path)
    }

    /// Compute each active file's `resume_ordinal` from the most-recent failed
    /// job's per-`(file, phase)` rows (spec §3.1). Drops files that are terminal
    /// (`Blocked` at their highest recorded phase) or complete
    /// (`resume_ordinal >= phase_count`). Backfills a `Committed` row for any file
    /// whose chain tip advanced past its highest recorded committed version
    /// (a crash between the inline commit and the row write, or a stale prior id).
    /// Returns the surviving files (with `resume_ordinal` set) and the rows it
    /// backfilled (#165).
    pub(super) async fn reconcile_resume(
        &self,
        prior_job_id: JobId,
        job_id: JobId,
        files: Vec<PhaseFile>,
        phase_count: u32,
    ) -> Result<(Vec<PhaseFile>, Vec<FilePhaseSummary>), VoomError> {
        let prior = self
            .workflow_summaries
            .file_phases_for_job(prior_job_id)
            .await?;
        let mut survivors = Vec::with_capacity(files.len());
        let mut backfilled = Vec::new();
        for mut file in files {
            let rows: Vec<&FilePhaseSummary> = prior
                .iter()
                .filter(|row| row.branch_id == file.branch_id)
                .collect();
            let highest = rows.iter().max_by_key(|row| row.phase_ordinal);
            if highest.is_some_and(|top| top.outcome == FilePhaseOutcome::Blocked) {
                continue; // terminal: aborted-for-file under the prior run
            }
            let mut resume_ordinal = highest.map_or(0, |top| top.phase_ordinal + 1);

            // Consistency backfill: default the recorded tip to the input-set
            // starting version when no committed row is visible.
            let recorded_tip = rows
                .iter()
                .filter(|row| row.outcome == FilePhaseOutcome::Committed)
                .max_by_key(|row| row.phase_ordinal)
                .and_then(|row| row.produced_file_version_id)
                .unwrap_or(file.start_version_id);
            if file.version_id != recorded_tip {
                let tip = self
                    .identity
                    .get_file_version(file.version_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "resume: chain tip {} vanished for {}",
                            file.version_id, file.branch_id
                        ))
                    })?;
                let produced = ProducedRefs::resolve(self, &tip, &file.snapshot).await?;
                let row = self
                    .write_file_row(
                        job_id,
                        resume_ordinal,
                        &file,
                        FilePhaseOutcome::Committed,
                        &[],
                        Some(produced),
                    )
                    .await?;
                backfilled.push(row);
                resume_ordinal += 1;
            }

            if resume_ordinal >= phase_count {
                continue; // complete: nothing left to run
            }
            file.resume_ordinal = resume_ordinal;
            survivors.push(file);
        }
        Ok((survivors, backfilled))
    }
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
