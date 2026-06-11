use std::path::{Path, PathBuf};

use serde_json::json;
use voom_core::VoomError;
use voom_core::ids::ArtifactCommitRecordId;
use voom_events::Event;
use voom_events::payload::ArtifactCommitCompletedPayload;
use voom_store::repo::artifacts::ArtifactCommitRecord;
use voom_store::repo::identity::{
    FileLocationKind, IdentityRepo, NewFileLocation, NewFileVersion, ProducedBy,
};

use voom_artifact::commit_pipeline::append_commit_event_in_tx;

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactReport, CommitRecoveryReport, PreparedCommit, PromotionOutcome,
};
use crate::cases::{begin_tx, commit_tx};

pub(super) async fn finalize_commit(
    cp: &ControlPlane,
    prepared: &PreparedCommit,
    promotion: &PromotionOutcome,
) -> Result<CommitArtifactReport, VoomError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let result_version = cp
        .identity
        .create_file_version_in_tx(
            &mut tx,
            NewFileVersion {
                file_asset_id: prepared.source_file_asset_id,
                content_hash: promotion.target_facts.content_hash.clone(),
                size_bytes: promotion.target_facts.size_bytes,
                produced_by: ProducedBy::StagedCommit,
                produced_from_version_id: Some(prepared.source_file_version_id),
                created_at: now,
            },
        )
        .await?;
    let result_location = cp
        .identity
        .create_file_location_in_tx(
            &mut tx,
            NewFileLocation {
                file_version_id: result_version.id,
                kind: FileLocationKind::LocalPath,
                value: prepared.target_path.display().to_string(),
                proof: None,
                observed_at: now,
            },
        )
        .await?;
    cp.artifacts
        .retire_location_in_tx(&mut tx, prepared.staging_location_id, now)
        .await?;
    let committed = cp
        .artifacts
        .mark_commit_committed_in_tx(
            &mut tx,
            prepared.record.id,
            result_version.id,
            result_location.id,
            prepared.promotion_started_at,
            now,
        )
        .await?;
    append_commit_event_in_tx(
        &cp.events,
        &mut tx,
        prepared.artifact_handle_id,
        now,
        Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
            commit_record_id: committed.id.0,
            artifact_handle_id: prepared.artifact_handle_id.0,
            result_file_version_id: result_version.id.0,
            result_file_location_id: result_location.id.0,
            target_path: prepared.target_path.display().to_string(),
        }),
    )
    .await?;
    commit_tx(tx).await?;
    Ok(report_from_record(&committed, &prepared.target_path, None))
}

pub(super) async fn update_commit_report_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
    recovery: &CommitRecoveryReport,
) -> Result<(), VoomError> {
    let report = serde_json::to_string(&json!({
        "phase": "recovery_required",
        "recovery_reason": recovery.recovery_reason,
        "target_path": recovery.target_path.display().to_string(),
        "target_exists": recovery.target_exists,
        "temp_path": recovery.temp_path.as_ref().map(|p| p.display().to_string()),
        "temp_exists": recovery.temp_exists,
        "staging_path": recovery.staging_path.display().to_string(),
        "staging_exists": recovery.staging_exists,
        "result_file_version_id": recovery.result_file_version_id.map(|id| id.0),
        "result_file_location_id": recovery.result_file_location_id.map(|id| id.0),
    }))
    .map_err(|err| VoomError::Internal(format!("commit recovery report encode: {err}")))?;
    sqlx::query("UPDATE artifact_commit_records SET report = ? WHERE id = ? AND state = 'pending'")
        .bind(report)
        .bind(i64::try_from(id.0).map_err(|err| {
            VoomError::Internal(format!("artifact commit id exceeds SQLite integer: {err}"))
        })?)
        .execute(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("artifact_commit_records report update", err))?;
    Ok(())
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
