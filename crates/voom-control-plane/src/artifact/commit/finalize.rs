//! Commit finalization: the durable convergence transaction shared by node
//! completion and recovery-driven finalization (ADR 0074). Creates the result
//! version/location from validated node-reported facts, retires both staging
//! rows (the `artifact_locations` marker and the rooted `file_locations` row
//! addressing the staged bytes), marks the record committed, and emits the
//! completed audit event.

use std::path::{Path, PathBuf};

use voom_core::VoomError;
use voom_events::Event;
use voom_events::payload::ArtifactCommitCompletedPayload;
use voom_store::repo::media::artifacts::ArtifactCommitRecord;
use voom_store::repo::media::identity::{
    FileLocationRepo, FileVersionRepo, NewFileLocation, NewFileVersion, ProducedBy,
};
use voom_artifact::commit_pipeline::append_commit_event_in_tx;

use crate::ControlPlane;
use crate::artifact::commit::{CommitArtifactReport, CommitFinalizeInput, CommitRecoveryReport};
use crate::artifact::fs::ArtifactFileFacts;

pub(crate) async fn finalize_commit_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &CommitFinalizeInput,
    target_facts: &ArtifactFileFacts,
) -> Result<CommitArtifactReport, VoomError> {
    let now = cp.clock().now();
    let result_version = cp
        .identity
        .create_file_version_in_tx(
            tx,
            NewFileVersion {
                file_asset_id: input.source_file_asset_id,
                content_hash: target_facts.content_hash.clone(),
                size_bytes: target_facts.size_bytes,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(input.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let result_location = cp
        .identity
        .create_file_location_in_tx(
            tx,
            NewFileLocation {
                file_version_id: result_version.id,
                storage_root_id: input.target_storage_root_id,
                provider_relative_locator: input.target_relative_locator.clone(),
                proof: None,
                observed_at: now,
            },
        )
        .await?;
    cp.artifacts
        .retire_location_in_tx(tx, input.staging_artifact_location_id, now)
        .await?;
    if let Some((staging_file_location_id, expected_epoch)) = input.staging_file_location {
        // The staged bytes have a durable address no more: retire the rooted
        // row pinned by the intent (spec amendment to ADR 0074).
        cp.identity
            .retire_file_location_in_tx(tx, staging_file_location_id, now, expected_epoch)
            .await?;
    }
    let committed = cp
        .artifacts
        .mark_commit_committed_in_tx(
            tx,
            input.record_id,
            result_version.id,
            result_location.id,
            input.promotion_started_at,
            now,
        )
        .await?;
    append_commit_event_in_tx(
        &cp.events,
        tx,
        input.artifact_handle_id,
        now,
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: committed.id,
            artifact_handle_id: input.artifact_handle_id,
            result_file_version_id: result_version.id,
            result_file_location_id: result_location.id,
            target_path: input.target_path.display().to_string(),
            gate_evaluated_lease_ids: input.gate_evaluated_lease_ids.clone(),
        }),
    )
    .await?;
    Ok(report_from_record(&committed, &input.target_path, None))
}

pub(super) fn report_from_record(
    record: &ArtifactCommitRecord,
    target_path: &Path,
    recovery: Option<CommitRecoveryReport>,
) -> CommitArtifactReport {
    CommitArtifactReport {
        commit_record_id: record.id,
        artifact_handle_id: record.artifact_handle_id,
        verification_id: record.verification_id,
        target_path: target_path.to_path_buf(),
        temp_path: record.temp_path.as_ref().map(PathBuf::from),
        state: record.state,
        result_file_version_id: record.result_file_version_id,
        result_file_location_id: record.result_file_location_id,
        recovery_required: recovery,
    }
}
