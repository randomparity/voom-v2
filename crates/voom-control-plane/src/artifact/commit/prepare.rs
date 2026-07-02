use std::path::{Path, PathBuf};

use serde_json::json;
use sqlx::Row;
use voom_core::ids::ArtifactVerificationId;
use voom_core::{ArtifactHandleId, ArtifactLocationId, FileAssetId, FileVersionId, VoomError};
use voom_events::Event;
use voom_events::payload::{ArtifactCommitFailedPreMutationPayload, ArtifactCommitStartedPayload};
use voom_store::repo::artifacts::{ArtifactVerification, NewArtifactCommitRecord};
use voom_store::repo::check_lineage_commit_leases_in_tx;
use voom_store::repo::identity::IdentityRepo;

use voom_artifact::commit_pipeline::{
    PendingCommitRecordError, append_commit_event_in_tx,
    create_pending_commit_with_started_event_in_tx,
};

use crate::ControlPlane;
use crate::artifact::commit::{
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactPreMutationReport,
    PreparedCommit,
};
use crate::artifact::fs::{
    ArtifactFileFacts, canonical_new_leaf_no_symlink, observe_regular_file,
    unique_temp_sibling_path,
};
use crate::cases::{begin_tx, commit_tx};

pub(super) async fn prepare_commit(
    cp: &ControlPlane,
    input: CommitArtifactInput,
) -> Result<PreparedCommit, CommitArtifactCommandError> {
    let mut tx = begin_tx(&cp.pool).await?;
    let now = cp.clock().now();
    let prepared_result = prepare_commit_in_tx(cp, &mut tx, input, now).await;
    match prepared_result {
        Ok(prepared) => {
            commit_tx(tx).await?;
            Ok(prepared)
        }
        Err(PrepareCommitError::PreMutation(failure)) => {
            append_failed_pre_mutation(cp, &mut tx, &failure, now).await?;
            commit_tx(tx).await?;
            Err(CommitArtifactCommandError::pre_mutation(failure))
        }
        Err(PrepareCommitError::AfterPending(err)) => Err(err.into()),
    }
}

#[derive(Debug)]
pub(super) enum PrepareCommitError {
    PreMutation(CommitArtifactPreMutationReport),
    AfterPending(VoomError),
}

async fn prepare_commit_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: CommitArtifactInput,
    now: time::OffsetDateTime,
) -> Result<PreparedCommit, PrepareCommitError> {
    let context = PreMutationContext {
        artifact_handle_id: input.artifact_handle_id,
        verification_id: None,
        target_path: input.target_path.clone(),
    };
    let source = read_commit_source_facts(cp, tx, input.artifact_handle_id, &context).await?;
    let verified_staging = read_verified_staging_facts(
        cp,
        tx,
        input.artifact_handle_id,
        &input.target_path,
        &context,
    )
    .await?;
    // Commit safety gate: a blocking use lease live at commit time on the
    // affected scope fails the commit here, inside the host transaction and
    // before any irreversible filesystem mutation (design §1187–1190). Any
    // gate-check error is fail-closed — the commit does not proceed.
    let gate_evaluated_lease_ids =
        check_commit_safety_gate(cp, tx, &source, &verified_staging.context, now).await?;
    let paths = prepare_commit_paths(&input.target_path, &source.handle, &verified_staging).await?;

    let target_path_string = paths.target_path.display().to_string();
    let temp_path_string = paths.temp_path.display().to_string();
    let pending_input = NewArtifactCommitRecord {
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        verification_id: verified_staging.verification.id,
        target_path: target_path_string.clone(),
        temp_path: Some(temp_path_string.clone()),
        report: json!({
            "phase": "prepared",
            "staging_path": paths.staging_path.display().to_string(),
            "target_path": target_path_string,
            "temp_path": temp_path_string,
            "expected_size_bytes": paths.expected_facts.size_bytes,
            "expected_checksum": paths.expected_facts.content_hash,
            "staging_local_file_key": paths.expected_facts.local_file_key,
        }),
        started_at: now,
    };
    let record = create_pending_commit_with_started_event_in_tx(
        &cp.artifacts,
        &cp.events,
        tx,
        pending_input,
        |commit_record_id| {
            Event::ArtifactCommitStarted(ArtifactCommitStartedPayload {
                commit_record_id: commit_record_id.0,
                artifact_handle_id: input.artifact_handle_id.0,
                source_file_version_id: source.source_file_version_id.0,
                verification_id: verified_staging.verification.id.0,
                target_path: paths.target_path.display().to_string(),
                temp_path: paths.temp_path.display().to_string(),
            })
        },
    )
    .await
    .map_err(|err| match err {
        PendingCommitRecordError::BeforePending(err) => {
            PrepareCommitError::PreMutation(pre_mutation(&verified_staging.context, &err))
        }
        PendingCommitRecordError::AfterPending(err) => PrepareCommitError::AfterPending(err),
    })?;

    Ok(PreparedCommit {
        record,
        artifact_handle_id: input.artifact_handle_id,
        source_file_version_id: source.source_file_version_id,
        source_file_asset_id: source.source_file_asset_id,
        staging_location_id: verified_staging.staging.id,
        staging_path: paths.staging_path,
        target_path: paths.target_path,
        temp_path: paths.temp_path,
        expected_facts: paths.expected_facts,
        promotion_started_at: now,
        gate_evaluated_lease_ids,
    })
}

/// Consult the commit safety gate for a lineage commit. Returns the use-lease
/// ids the gate evaluated (for the audit event) when no blocking lease is
/// live, or a pre-mutation failure when one is (`BlockedByUseLease`) or when
/// the check itself fails (fail-closed).
async fn check_commit_safety_gate(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    source: &CommitSourceFacts,
    context: &PreMutationContext,
    now: time::OffsetDateTime,
) -> Result<Vec<voom_core::UseLeaseId>, PrepareCommitError> {
    let check = check_lineage_commit_leases_in_tx(
        tx,
        &cp.identity,
        source.source_file_asset_id,
        source.source_file_version_id,
        now,
    )
    .await
    .map_err(|err| pre_mutation_error(context, &err))?;
    if let Some((lease_id, scope)) = check.blocking {
        return Err(pre_mutation_error(
            context,
            &VoomError::BlockedByUseLease(format!(
                "commit blocked by active use lease {lease_id} on {} {}",
                scope.type_str(),
                scope.id_u64()
            )),
        ));
    }
    Ok(check.evaluated_lease_ids)
}

#[derive(Debug)]
pub(super) struct CommitSourceFacts {
    pub(super) handle: HandleFacts,
    pub(super) source_file_version_id: FileVersionId,
    pub(super) source_file_asset_id: FileAssetId,
}

#[derive(Debug)]
pub(super) struct VerifiedStagingFacts {
    pub(super) staging: LiveStagingLocation,
    pub(super) verification: ArtifactVerification,
    pub(super) context: PreMutationContext,
}

#[derive(Debug)]
struct CommitPreparedPaths {
    target_path: PathBuf,
    staging_path: PathBuf,
    temp_path: PathBuf,
    expected_facts: ArtifactFileFacts,
}

pub(super) async fn read_commit_source_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    context: &PreMutationContext,
) -> Result<CommitSourceFacts, PrepareCommitError> {
    let handle = read_handle_facts_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let Some(source_file_version_id) = handle.source_file_version_id else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} is not linked to a source file_version"
            )),
        ));
    };
    let Some(source) = cp
        .identity
        .get_file_version_in_tx(tx, source_file_version_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::NotFound(format!("file_versions {source_file_version_id} missing")),
        ));
    };
    if source.retired_at.is_some() {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!("file_versions {source_file_version_id} is retired")),
        ));
    }

    Ok(CommitSourceFacts {
        handle,
        source_file_version_id,
        source_file_asset_id: source.file_asset_id,
    })
}

pub(super) async fn read_verified_staging_facts(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    target_path: &Path,
    context: &PreMutationContext,
) -> Result<VerifiedStagingFacts, PrepareCommitError> {
    let staging = live_staging_location_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let Some(verification) = cp
        .artifacts
        .latest_successful_verification_for_live_staging_in_tx(tx, artifact_handle_id)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?
    else {
        return Err(pre_mutation_error(
            context,
            &VoomError::Config(format!(
                "artifact_handle {artifact_handle_id} has no successful verification for its live staging location"
            )),
        ));
    };
    let context = PreMutationContext {
        artifact_handle_id,
        verification_id: Some(verification.id),
        target_path: target_path.to_owned(),
    };
    if verification.artifact_location_id != staging.id || verification.path != staging.value {
        return Err(pre_mutation_error(
            &context,
            &VoomError::Config(format!(
                "artifact verification {} is stale for live staging location {}",
                verification.id, staging.id
            )),
        ));
    }

    Ok(VerifiedStagingFacts {
        staging,
        verification,
        context,
    })
}

async fn prepare_commit_paths(
    target_path: &Path,
    handle: &HandleFacts,
    verified_staging: &VerifiedStagingFacts,
) -> Result<CommitPreparedPaths, PrepareCommitError> {
    let context = &verified_staging.context;
    let target_path = canonical_new_leaf_no_symlink(target_path)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    let staging_path = PathBuf::from(&verified_staging.staging.value);
    let expected_facts = observe_regular_file(&staging_path)
        .await
        .map_err(|err| pre_mutation_error(context, &err))?;
    require_expected_facts(handle, &verified_staging.verification, &expected_facts)
        .map_err(|err| pre_mutation_error(context, &err))?;
    let temp_path =
        unique_temp_sibling_path(&target_path).map_err(|err| pre_mutation_error(context, &err))?;

    Ok(CommitPreparedPaths {
        target_path,
        staging_path,
        temp_path,
        expected_facts,
    })
}

fn pre_mutation_error(context: &PreMutationContext, err: &VoomError) -> PrepareCommitError {
    PrepareCommitError::PreMutation(pre_mutation(context, err))
}

#[derive(Debug, Clone)]
pub(super) struct PreMutationContext {
    pub(super) artifact_handle_id: ArtifactHandleId,
    pub(super) verification_id: Option<ArtifactVerificationId>,
    pub(super) target_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct HandleFacts {
    source_file_version_id: Option<FileVersionId>,
    size_bytes: u64,
    checksum: String,
}

#[derive(Debug)]
pub(super) struct LiveStagingLocation {
    pub(super) id: ArtifactLocationId,
    pub(super) value: String,
}

async fn read_handle_facts_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactHandleId,
) -> Result<HandleFacts, VoomError> {
    let row = sqlx::query(
        "SELECT file_version_id, size_bytes, checksum FROM artifact_handles WHERE id = ?",
    )
    .bind(i64::try_from(id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("artifact_handles commit lookup", err))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handles {id} missing"
        )));
    };
    let file_version_id: Option<i64> = row
        .try_get("file_version_id")
        .map_err(|err| VoomError::database_context("artifact_handles.file_version_id", err))?;
    let size_bytes: Option<i64> = row
        .try_get("size_bytes")
        .map_err(|err| VoomError::database_context("artifact_handles.size_bytes", err))?;
    let checksum: Option<String> = row
        .try_get("checksum")
        .map_err(|err| VoomError::database_context("artifact_handles.checksum", err))?;
    let source_file_version_id = file_version_id
        .map(|v| {
            u64::try_from(v).map(FileVersionId).map_err(|err| {
                VoomError::database_context("artifact_handles.file_version_id negative", err)
            })
        })
        .transpose()?;
    Ok(HandleFacts {
        source_file_version_id,
        size_bytes: u64::try_from(size_bytes.ok_or_else(|| {
            VoomError::Config(format!("artifact_handle {id} missing expected size_bytes"))
        })?)
        .map_err(|err| VoomError::database_context("artifact_handles.size_bytes negative", err))?,
        checksum: checksum.ok_or_else(|| {
            VoomError::Config(format!("artifact_handle {id} missing expected checksum"))
        })?,
    })
}

async fn live_staging_location_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
) -> Result<LiveStagingLocation, VoomError> {
    let rows = sqlx::query(
        "SELECT id, value FROM artifact_locations \
         WHERE artifact_handle_id = ? AND kind = 'staging' AND retired_at IS NULL \
         ORDER BY id ASC",
    )
    .bind(i64::try_from(handle_id.0).map_err(|err| {
        VoomError::Internal(format!("artifact handle id exceeds SQLite integer: {err}"))
    })?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("artifact_locations commit live staging", err))?;
    let [row] = rows.as_slice() else {
        return Err(VoomError::Config(format!(
            "artifact_handle {handle_id} must have exactly one live staging location; found {}",
            rows.len()
        )));
    };
    let id: i64 = row
        .try_get("id")
        .map_err(|err| VoomError::database_context("artifact_locations.id", err))?;
    let value = row
        .try_get("value")
        .map_err(|err| VoomError::database_context("artifact_locations.value", err))?;
    let id = u64::try_from(id)
        .map(ArtifactLocationId)
        .map_err(|err| VoomError::database_context("artifact_locations.id negative", err))?;
    Ok(LiveStagingLocation { id, value })
}

pub(super) fn require_expected_facts(
    handle: &HandleFacts,
    verification: &ArtifactVerification,
    staged: &ArtifactFileFacts,
) -> Result<(), VoomError> {
    if handle.size_bytes != staged.size_bytes
        || handle.checksum != staged.content_hash
        || verification.expected_size_bytes != staged.size_bytes
        || verification.expected_checksum != staged.content_hash
        || verification.observed_size_bytes != Some(staged.size_bytes)
        || verification.observed_checksum.as_deref() != Some(staged.content_hash.as_str())
    {
        return Err(VoomError::ArtifactChecksumMismatch(
            "staged artifact facts no longer match the successful verification".to_owned(),
        ));
    }
    Ok(())
}

fn pre_mutation(context: &PreMutationContext, err: &VoomError) -> CommitArtifactPreMutationReport {
    CommitArtifactPreMutationReport {
        artifact_handle_id: context.artifact_handle_id,
        verification_id: context.verification_id,
        target_path: context.target_path.clone(),
        error_code: err.error_code(),
        message: err.to_string(),
    }
}

async fn append_failed_pre_mutation(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    failure: &CommitArtifactPreMutationReport,
    occurred_at: time::OffsetDateTime,
) -> Result<(), VoomError> {
    append_commit_event_in_tx(
        &cp.events,
        tx,
        failure.artifact_handle_id,
        occurred_at,
        Event::ArtifactCommitFailedPreMutation(ArtifactCommitFailedPreMutationPayload {
            artifact_handle_id: failure.artifact_handle_id.0,
            commit_record_id: None,
            target_path: failure.target_path.display().to_string(),
            error_code: failure.error_code.as_str().to_owned(),
            message: failure.message.clone(),
        }),
    )
    .await
}
