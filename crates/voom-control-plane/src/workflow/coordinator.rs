//! Multi-file phase-barrier coordinator (issue #162, Sprint 16 §3/§6).
//!
//! Phase 2 lands the snapshot projection the coordinator uses to feed each
//! phase's planner against the artifact the prior phase committed: it reads a
//! file's active version (chain tip) plus its latest [`MediaSnapshot`] and
//! projects them into a [`MediaSnapshotInput`]. The coordinator core
//! (`run_phase_barrier`) arrives in Phase 3 and reuses these helpers.

use std::time::Duration;

use serde_json::{Value, json};
use voom_core::{FileAssetId, FileVersionId, JobId, PolicyInputSetId, PolicyVersionId, VoomError};
use voom_policy::{MediaSnapshotInput, TargetRef};
use voom_store::repo::identity::{FileVersion, IdentityRepo, MediaSnapshot};
use voom_store::repo::jobs::NewJob;
use voom_store::repo::policy_inputs::PolicyInputTargetRef;
use voom_store::repo::workflow_summaries::{
    FilePhaseSummary, NewWorkflowSummary, PhaseSummary, WorkflowSummary, WorkflowSummaryRepo,
};

use crate::ControlPlane;
use crate::cases::compliance::ComplianceExecutionOptions;
use crate::cases::policy_inputs::stream_summary_from_snapshot_payload;

use super::executor::WORKFLOW_JOB_KIND;

/// Durable result of a phase-barrier run: the owning job's summary plus the
/// per-phase and per-`(file, phase)` rows the run wrote.
#[derive(Debug, Clone)]
pub struct CoordinatorOutcome {
    pub job_id: JobId,
    pub summary: WorkflowSummary,
    pub phases: Vec<PhaseSummary>,
    pub file_phases: Vec<FilePhaseSummary>,
}

/// A phase-barrier run that failed after the job opened. `partial` carries the
/// per-`(file, phase)` rows for files that committed inline before the failure.
#[derive(Debug)]
pub struct CoordinatorError {
    pub source: VoomError,
    pub partial: Option<CoordinatorOutcome>,
}

impl ControlPlane {
    /// Drive the existing workflow executor one phase at a time across every
    /// file in a policy input set, phases acting as barriers across files
    /// (issue #162, Sprint 16 §3/§6). The coordinator owns one job for the whole
    /// run (ADR-0007) and persists a durable per-phase / per-`(file, phase)`
    /// summary.
    ///
    /// # Errors
    /// Returns [`CoordinatorError`] when durable inputs are missing, the policy
    /// fails to compile, or a phase's tickets fail. Any error after the job
    /// opens finalizes the job as `failed`.
    pub async fn run_phase_barrier(
        &self,
        policy_version_id: PolicyVersionId,
        input_set_id: PolicyInputSetId,
        options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, CoordinatorError> {
        let inputs = self
            .load_current_accepted_policy_and_input(policy_version_id, input_set_id)
            .await
            .map_err(|source| CoordinatorError {
                source,
                partial: None,
            })?;
        let policy = self
            .compiled_policy_for_version(&inputs.version)
            .await
            .map_err(|source| CoordinatorError {
                source,
                partial: None,
            })?;
        let active: Vec<FileVersionId> = inputs
            .input
            .media_snapshots
            .iter()
            .filter_map(|snapshot| match snapshot.target {
                PolicyInputTargetRef::FileVersion { id } => Some(id),
                _ => None,
            })
            .collect();

        let now = self.clock().now();
        let job = self
            .open_job(NewJob {
                kind: WORKFLOW_JOB_KIND.to_owned(),
                priority: 0,
                created_at: now,
            })
            .await
            .map_err(|source| CoordinatorError {
                source,
                partial: None,
            })?;

        // Job-cleanup contract: once the job is open, every error path finalizes
        // it as `failed` rather than orphaning it in `open`.
        match self
            .run_phase_barrier_in_job(job.id, &policy, &active, options)
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(source) => {
                let _ = self
                    .fail_job(job.id, source.to_string(), self.clock().now())
                    .await;
                Err(CoordinatorError {
                    source,
                    partial: None,
                })
            }
        }
    }

    async fn run_phase_barrier_in_job(
        &self,
        job_id: JobId,
        policy: &voom_policy::CompiledPolicy,
        active: &[FileVersionId],
        _options: ComplianceExecutionOptions,
    ) -> Result<CoordinatorOutcome, VoomError> {
        if active.is_empty() || policy.phase_order.is_empty() {
            return self.finalize_zero_phase_run(job_id).await;
        }
        // The dispatching phase loop (project → plan_phase → bridge → run →
        // finalize) lands in the next #162 Phase 3 step. No production caller
        // reaches this path yet; `compliance execute` is wired in Phase 4.
        Err(VoomError::Internal(
            "multi-file phase-barrier execution is not yet implemented".to_owned(),
        ))
    }

    /// Succeed the job and write a zero-count job-grain summary for a run with no
    /// active files or no declared phases (no work, no phase or file rows).
    async fn finalize_zero_phase_run(
        &self,
        job_id: JobId,
    ) -> Result<CoordinatorOutcome, VoomError> {
        let now = self.clock().now();
        self.succeed_job(job_id, now).await?;
        let summary = self
            .workflow_summaries()
            .insert_summary(
                NewWorkflowSummary {
                    job_id,
                    branch_count: 0,
                    ticket_count: 0,
                    dispatch_count: 0,
                    retry_count: 0,
                    failure_count: 0,
                    peak_active_workflow_leases: 0,
                    elapsed: Duration::ZERO,
                    per_operation: json!({}),
                },
                now,
            )
            .await?;
        Ok(CoordinatorOutcome {
            job_id,
            summary,
            phases: Vec::new(),
            file_phases: Vec::new(),
        })
    }
}

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
