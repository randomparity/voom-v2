use std::path::{Path, PathBuf};

use tokio::fs;
use voom_core::{FailureClass, VoomError};
use voom_events::Event;
use voom_events::payload::ArtifactCommitRecoveryRequiredPayload;
use voom_store::repo::artifacts::{ArtifactCommitFailure, ArtifactCommitState};

use voom_artifact::commit_pipeline::{
    RecoveryRequiredCommit, mark_recovery_required_with_event_in_tx,
};

use crate::ControlPlane;
use crate::artifact::commit::finalize::{
    finalize_commit, report_from_record, update_commit_report_in_tx,
};
use crate::artifact::commit::prepare::{
    PreMutationContext, PrepareCommitError, read_commit_source_facts, read_verified_staging_facts,
    require_expected_facts,
};
use crate::artifact::commit::promote::promote_prepared;
use crate::artifact::commit::{
    CommitArtifactCommandError, CommitArtifactReport, CommitRecoveryReport, NoCommitArtifactHooks,
    PreparedCommit, PromotionOutcome, same_file_facts,
};
use crate::artifact::fs::{
    canonical_new_leaf_no_symlink, observe_regular_file, unique_temp_sibling_path,
};
use crate::cases::{begin_tx, commit_tx};
use voom_core::ArtifactHandleId;

fn recovery_read_error(err: PrepareCommitError) -> VoomError {
    match err {
        PrepareCommitError::AfterPending(err) => err,
        PrepareCommitError::PreMutation(report) => {
            VoomError::CommitFailure(format!("commit recovery cannot re-read inputs: {report:?}"))
        }
    }
}

pub(super) async fn recover_commit_inner(
    cp: &ControlPlane,
    artifact_handle_id: ArtifactHandleId,
) -> Result<CommitArtifactReport, VoomError> {
    let record = cp
        .artifacts
        .list_commit_records(artifact_handle_id)
        .await?
        .into_iter()
        .find(|record| record.state == ArtifactCommitState::RecoveryRequired)
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_handle {artifact_handle_id} has no commit in recovery_required state"
            ))
        })?;
    let target_path = PathBuf::from(&record.target_path);

    // Re-read the same inputs the initial prepare used; the staging artifact is
    // still live because finalize (which retires it) never completed.
    let context = PreMutationContext {
        artifact_handle_id,
        verification_id: Some(record.verification_id),
        target_path: target_path.clone(),
    };
    let mut tx = begin_tx(&cp.pool).await?;
    let source = read_commit_source_facts(cp, &mut tx, artifact_handle_id, &context)
        .await
        .map_err(recovery_read_error)?;
    let verified_staging =
        read_verified_staging_facts(cp, &mut tx, artifact_handle_id, &target_path, &context)
            .await
            .map_err(recovery_read_error)?;
    commit_tx(tx).await?;

    let staging_path = PathBuf::from(&verified_staging.staging.value);
    let expected_facts = observe_regular_file(&staging_path).await?;
    require_expected_facts(
        &source.handle,
        &verified_staging.verification,
        &expected_facts,
    )?;

    // Decide where to resume from based on the target's current state. Only a
    // genuinely absent target (NotFound) means "resume a fresh install"; a
    // permission/IO error or an occupied path must surface loudly rather than
    // be misread as absent (which would attempt a spurious fresh install).
    let existing_target = match fs::symlink_metadata(&target_path).await {
        Ok(_) => Some(observe_regular_file(&target_path).await?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(VoomError::CommitFailure(format!(
                "commit recovery cannot stat target {}: {err}",
                target_path.display()
            )));
        }
    };
    let already_installed = match &existing_target {
        Some(facts) if same_file_facts(facts, &expected_facts) => true,
        Some(_) => {
            return Err(VoomError::Conflict(format!(
                "commit recovery: target {} exists with mismatched facts",
                target_path.display()
            )));
        }
        None => false,
    };
    let canonical_target = if already_installed {
        fs::canonicalize(&target_path).await.map_err(|err| {
            VoomError::CommitFailure(format!(
                "commit recovery cannot canonicalize installed target {}: {err}",
                target_path.display()
            ))
        })?
    } else {
        canonical_new_leaf_no_symlink(&target_path).await?
    };
    let temp_path = unique_temp_sibling_path(&canonical_target)?;

    let prepared = PreparedCommit {
        record,
        artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        source_file_asset_id: source.source_file_asset_id,
        staging_location_id: verified_staging.staging.id,
        staging_path,
        target_path: canonical_target,
        temp_path,
        expected_facts: expected_facts.clone(),
        promotion_started_at: cp.clock().now(),
        // Recovery re-drives a commit that already passed the gate at its
        // original prepare; it does not re-evaluate the gate (out of scope,
        // documented in the spec/ADR), so no leases are recorded here.
        gate_evaluated_lease_ids: Vec::new(),
    };

    let promotion = if already_installed {
        PromotionOutcome {
            target_facts: existing_target.unwrap_or(expected_facts),
        }
    } else {
        promote_prepared(cp, &prepared, &NoCommitArtifactHooks).await?
    };

    finalize_commit(cp, &prepared, &promotion).await
}

pub(super) async fn transition_recovery(
    cp: &ControlPlane,
    prepared: &PreparedCommit,
    err: VoomError,
) -> Result<CommitArtifactReport, CommitArtifactCommandError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let recovery = observe_recovery(prepared, recovery_reason(&err)).await;
    update_commit_report_in_tx(&mut tx, prepared.record.id, &recovery)
        .await
        .map_err(CommitArtifactCommandError::from)?;
    let error_code = err.error_code().as_str().to_owned();
    let message = err.to_string();
    let recovered = mark_recovery_required_with_event_in_tx(
        &cp.artifacts,
        &cp.events,
        &mut tx,
        RecoveryRequiredCommit {
            commit_record_id: prepared.record.id,
            artifact_handle_id: prepared.artifact_handle_id,
            failure: ArtifactCommitFailure {
                failure_class: failure_class_for_error(&err),
                error_code: error_code.clone(),
                message: message.clone(),
                finished_at: now,
            },
            recovery_reason: recovery.recovery_reason.clone(),
            event: Event::ArtifactCommitRecoveryRequired(ArtifactCommitRecoveryRequiredPayload {
                commit_record_id: prepared.record.id.0,
                artifact_handle_id: prepared.artifact_handle_id.0,
                target_path: prepared.target_path.display().to_string(),
                temp_path: prepared.temp_path.display().to_string(),
                recovery_reason: recovery.recovery_reason.clone(),
                error_code,
                message,
            }),
            occurred_at: now,
        },
    )
    .await
    .map_err(CommitArtifactCommandError::from)?;
    commit_tx(tx)
        .await
        .map_err(CommitArtifactCommandError::from)?;
    Ok(report_from_record(
        &recovered,
        &prepared.target_path,
        Some(recovery),
    ))
}

async fn observe_recovery(
    prepared: &PreparedCommit,
    recovery_reason: String,
) -> CommitRecoveryReport {
    let target_exists = path_exists(&prepared.target_path).await;
    let temp_exists = path_exists(&prepared.temp_path).await;
    let staging_exists = path_exists(&prepared.staging_path).await;
    CommitRecoveryReport {
        recovery_reason,
        target_path: prepared.target_path.clone(),
        target_exists,
        temp_path: temp_exists.then(|| prepared.temp_path.clone()),
        temp_exists,
        staging_path: prepared.staging_path.clone(),
        staging_exists,
        result_file_version_id: None,
        result_file_location_id: None,
    }
}

async fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).await.is_ok()
}

fn recovery_reason(err: &VoomError) -> String {
    match err {
        VoomError::ArtifactChecksumMismatch(_) => "staged_bytes_drifted",
        VoomError::VerificationFailure(_) => "target_verification_failed",
        VoomError::Database { .. } => "finalize_failed",
        _ => "promotion_failed",
    }
    .to_owned()
}

fn failure_class_for_error(err: &VoomError) -> String {
    let class = match err {
        VoomError::ArtifactChecksumMismatch(_) => FailureClass::ArtifactChecksumMismatch,
        VoomError::ArtifactUnavailable(_) => FailureClass::ArtifactUnavailable,
        VoomError::VerificationFailure(_) => FailureClass::VerificationFailure,
        _ => FailureClass::CommitFailure,
    };
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "commit_failure".to_owned())
}
