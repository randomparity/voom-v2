//! Reuse immutable, already committed audio evidence without another encoder
//! or a fabricated owner access-plan consumption.

use serde_json::{Value, json};
use sqlx::{Sqlite, Transaction};
use voom_core::ids::{ArtifactCommitRecordId, ArtifactVerificationId};
use voom_core::{
    ArtifactHandleId, FileLocationId, FileVersionId, MediaSnapshotId, NodeId, VoomError, WorkerId,
};
use voom_store::repo::execution::leases::Lease;
use voom_store::repo::execution::tickets::Ticket;
use voom_store::repo::media::artifacts::{
    ArtifactCommitRecord, ArtifactCommitState, ArtifactVerification, ArtifactVerificationStatus,
};
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractOperationRecord, SqliteAudioExtractOperationRepo,
};
use voom_store::repo::media::audio_synthesis_operations::{
    AudioSynthesisOperationRecord, SqliteAudioSynthesisOperationRepo,
};
use voom_store::repo::media::identity::{
    FileLocationRepo, FileVersion, FileVersionRepo, MediaSnapshot, MediaSnapshotRepo,
};

use crate::ControlPlane;

struct AudioCommitIdentity {
    source_id: FileVersionId,
    handle_id: ArtifactHandleId,
    verification_id: ArtifactVerificationId,
    commit_id: ArtifactCommitRecordId,
    version_id: FileVersionId,
    location_id: FileLocationId,
    snapshot_id: MediaSnapshotId,
    probe_worker_id: WorkerId,
    probe_payload: Value,
}

struct ValidatedCommit {
    record: ArtifactCommitRecord,
    verification: ArtifactVerification,
    version: FileVersion,
    snapshot: MediaSnapshot,
    path: std::path::PathBuf,
    original_worker_id: Option<WorkerId>,
}

fn missing() -> VoomError {
    VoomError::database("committed audio operation lacks canonical publication evidence".to_owned())
}

fn validate_identity(identity: &AudioCommitIdentity) -> Result<(), VoomError> {
    for id in [
        identity.source_id.0,
        identity.handle_id.0,
        identity.verification_id.0,
        identity.commit_id.0,
        identity.version_id.0,
        identity.location_id.0,
        identity.snapshot_id.0,
        identity.probe_worker_id.0,
    ] {
        if id == 0 || i64::try_from(id).is_err() {
            return Err(missing());
        }
    }
    Ok(())
}

async fn validate_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    owner: NodeId,
    identity: &AudioCommitIdentity,
    worker_result: &Value,
) -> Result<ValidatedCommit, VoomError> {
    validate_identity(identity)?;
    let record = cp
        .artifacts
        .get_commit_record_in_tx(tx, identity.commit_id)
        .await?
        .ok_or_else(missing)?;
    let verification = cp
        .artifacts
        .get_verification_in_tx(tx, identity.verification_id)
        .await?
        .ok_or_else(missing)?;
    let version = cp
        .identity
        .get_file_version_in_tx(tx, identity.version_id)
        .await?
        .ok_or_else(missing)?;
    let location = cp
        .identity
        .get_file_location_in_tx(tx, identity.location_id)
        .await?
        .ok_or_else(missing)?;
    let snapshot = cp
        .identity
        .get_media_snapshot_in_tx(tx, identity.snapshot_id)
        .await?
        .ok_or_else(missing)?;
    let (root_id, relative) = location.rooted_address()?;
    let root = cp
        .libraries
        .get_library_root_in_tx(tx, root_id)
        .await?
        .ok_or_else(missing)?;
    // A bound verification remains bound forever. Audio semantic reuse is the
    // existing unbound verification contract, never a reassociation of an old
    // workflow observation with a fresh lease.
    if record.state != ArtifactCommitState::Committed
        || record.source_file_version_id != identity.source_id
        || record.artifact_handle_id != identity.handle_id
        || record.verification_id != identity.verification_id
        || record.result_file_version_id != Some(identity.version_id)
        || record.result_file_location_id != Some(identity.location_id)
        || verification.artifact_handle_id != identity.handle_id
        || verification.status != ArtifactVerificationStatus::Succeeded
        || verification.expected_size_bytes != version.size_bytes
        || verification.expected_checksum != version.content_hash
        || verification.observed_size_bytes != Some(version.size_bytes)
        || verification.observed_checksum.as_ref() != Some(&version.content_hash)
        || version.produced_from_version_id != Some(identity.source_id)
        || version.retired_at.is_some()
        || location.file_version_id != identity.version_id
        || location.retired_at.is_some()
        || snapshot.file_version_id != identity.version_id
        || snapshot.probed_by != Some(identity.probe_worker_id)
        || snapshot.payload != identity.probe_payload
    {
        return Err(VoomError::database(
            "committed audio catalog, verification, snapshot or lineage disagree".to_owned(),
        ));
    }
    let original_worker_id = validate_original_provenance_in_tx(
        cp,
        tx,
        &verification,
        &snapshot,
        &version,
        identity,
        worker_result,
    )
    .await?;
    if verification.workflow_ticket_id.is_some() || verification.workflow_lease_id.is_some() {
        return Err(VoomError::Conflict(
            "committed audio verification belongs to another workflow attempt".to_owned(),
        ));
    }
    if root.owner_node_id != Some(owner)
        || root.state != voom_core::StorageRootState::Active
        || !root.enabled
    {
        return Err(VoomError::Conflict(
            "committed audio target root is no longer active at its owner".to_owned(),
        ));
    }
    let path = std::path::Path::new(root.provider_locator.as_str()).join(relative.as_str());
    Ok(ValidatedCommit {
        record,
        verification,
        version,
        snapshot,
        path,
        original_worker_id,
    })
}

fn current_result(
    ticket: &Ticket,
    lease: &Lease,
    source_id: FileVersionId,
    committed: &ValidatedCommit,
    raw: Value,
) -> Result<Value, VoomError> {
    let mut result = raw;
    let object = result.as_object_mut().ok_or_else(missing)?;
    object.extend(serde_json::from_value::<serde_json::Map<String, Value>>(json!({
        "job_id": ticket.job_id.ok_or_else(missing)?, "ticket_id": ticket.id, "lease_id": lease.id,
        "source_file_version_id": source_id,
        "staged_artifact_handle_id": committed.record.artifact_handle_id,
        "staged_artifact_location_id": committed.verification.artifact_location_id,
        "verification_id": committed.verification.id, "commit_record_id": committed.record.id,
        "result_file_version_id": committed.version.id, "result_file_location_id": committed.record.result_file_location_id,
        "result_media_snapshot_id": committed.snapshot.id,
    })).map_err(|error| VoomError::Internal(format!("encode committed audio reuse: {error}")))?);
    Ok(result)
}

pub(super) async fn extraction_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    owner: NodeId,
    ticket: &Ticket,
    lease: &Lease,
    operation: &AudioExtractOperationRecord,
) -> Result<Value, VoomError> {
    let attempt =
        SqliteAudioExtractOperationRepo::require_committed_lineage_in_tx(tx, operation).await?;
    let raw = operation
        .operation
        .worker_result
        .clone()
        .ok_or_else(missing)?;
    serde_json::from_value::<voom_worker_protocol::ExtractAudioResult>(raw.clone()).map_err(
        |error| VoomError::database_context("committed extraction worker result", error),
    )?;
    let mut outputs = Vec::with_capacity(operation.outputs.len());
    let mut first = None;
    for output in &operation.outputs {
        let identity = AudioCommitIdentity {
            source_id: operation.operation.source_file_version_id,
            handle_id: output.artifact_handle_id.ok_or_else(missing)?,
            verification_id: output.verification_id.ok_or_else(missing)?,
            commit_id: output.commit_record_id.ok_or_else(missing)?,
            version_id: output.result_file_version_id.ok_or_else(missing)?,
            location_id: output.result_file_location_id.ok_or_else(missing)?,
            snapshot_id: output.result_media_snapshot_id.ok_or_else(missing)?,
            probe_worker_id: output.probe_worker_id.ok_or_else(missing)?,
            probe_payload: output.probe_payload.clone().ok_or_else(missing)?,
        };
        let committed = validate_in_tx(cp, tx, owner, &identity, &raw).await?;
        if committed
            .original_worker_id
            .is_some_and(|worker| worker != attempt.worker_id)
        {
            return Err(VoomError::database(
                "committed extraction original worker differs from its attempt".to_owned(),
            ));
        }
        let member = cp
            .bundles
            .get_member_by_file_asset_in_tx(tx, committed.version.file_asset_id)
            .await?
            .ok_or_else(missing)?;
        if output.result_file_asset_id != Some(committed.version.file_asset_id.0)
            || member.id != output.bundle_member_id.ok_or_else(missing)?
            || member.bundle_id != operation.operation.source_bundle_id
            || member.role.as_str() != output.bundle_role
        {
            return Err(VoomError::database(
                "committed extraction bundle identity differs".to_owned(),
            ));
        }
        outputs.push(crate::audio::ExecuteExtractAudioOutputReport {
            operation_output_id: output.id,
            output_id: output.output_id.clone(),
            source_file_version_id: identity.source_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id,
            source_snapshot_stream_id: output.source_snapshot_stream_id.clone(),
            source_provider_stream_index: output.source_provider_stream_index,
            role: output.bundle_role.clone(),
            staged_artifact_handle_id: identity.handle_id,
            staged_artifact_location_id: output.artifact_location_id.ok_or_else(missing)?,
            verification_id: identity.verification_id,
            commit_record_id: identity.commit_id,
            result_file_version_id: identity.version_id,
            result_file_location_id: identity.location_id,
            result_file_asset_id: committed.version.file_asset_id.0,
            result_media_snapshot_id: identity.snapshot_id,
            bundle_member_id: member.id,
            lineage_id: output.lineage_id.ok_or_else(missing)?,
            staging_path: std::path::PathBuf::from(
                output.staging_path.as_ref().ok_or_else(missing)?,
            ),
            target_path: committed.path.clone(),
        });
        if first.is_none() {
            first = Some(committed);
        }
    }
    let mut result = current_result(
        ticket,
        lease,
        operation.operation.source_file_version_id,
        &first.ok_or_else(missing)?,
        raw,
    )?;
    result.as_object_mut().ok_or_else(missing)?.insert(
        "outputs".to_owned(),
        serde_json::to_value(outputs).map_err(|error| {
            VoomError::Internal(format!("encode reused extraction outputs: {error}"))
        })?,
    );
    Ok(result)
}

pub(super) async fn synthesis_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    owner: NodeId,
    ticket: &Ticket,
    lease: &Lease,
    operation: &AudioSynthesisOperationRecord,
) -> Result<Value, VoomError> {
    let attempt =
        SqliteAudioSynthesisOperationRepo::require_committed_lineage_in_tx(tx, operation).await?;
    let stored = &operation.operation;
    let raw = stored.worker_result.clone().ok_or_else(missing)?;
    serde_json::from_value::<voom_worker_protocol::TranscodeAudioResult>(raw.clone())
        .map_err(|error| VoomError::database_context("committed synthesis worker result", error))?;
    let identity = AudioCommitIdentity {
        source_id: stored.source_file_version_id,
        handle_id: stored.artifact_handle_id.ok_or_else(missing)?,
        verification_id: stored.verification_id.ok_or_else(missing)?,
        commit_id: stored.commit_record_id.ok_or_else(missing)?,
        version_id: stored.result_file_version_id.ok_or_else(missing)?,
        location_id: stored.result_file_location_id.ok_or_else(missing)?,
        snapshot_id: stored.result_media_snapshot_id.ok_or_else(missing)?,
        probe_worker_id: stored.probe_worker_id.ok_or_else(missing)?,
        probe_payload: stored.probe_payload.clone().ok_or_else(missing)?,
    };
    let committed = validate_in_tx(cp, tx, owner, &identity, &raw).await?;
    if committed
        .original_worker_id
        .is_some_and(|worker| worker.0 != attempt.worker_id)
    {
        return Err(VoomError::database(
            "committed synthesis original worker differs from its attempt".to_owned(),
        ));
    }
    if stored.result_file_asset_id != Some(committed.version.file_asset_id) {
        return Err(missing());
    }
    let mut result = current_result(
        ticket,
        lease,
        stored.source_file_version_id,
        &committed,
        raw,
    )?;
    let companions = super::audio::synthesis_reports(
        operation,
        &committed.record,
        identity.version_id,
        identity.location_id,
        identity.snapshot_id,
    )?;
    let object = result.as_object_mut().ok_or_else(missing)?;
    object.insert(
        "synthesis_operation_id".to_owned(),
        Value::String(stored.planned_operation_id.clone()),
    );
    object.insert(
        "synthesis_operation_key".to_owned(),
        Value::String(stored.operation_key.clone()),
    );
    object.insert(
        "synthesized_companions".to_owned(),
        serde_json::to_value(companions).map_err(|error| {
            VoomError::Internal(format!("encode reused synthesis companions: {error}"))
        })?,
    );
    Ok(result)
}

async fn validate_original_provenance_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    verification: &ArtifactVerification,
    snapshot: &MediaSnapshot,
    version: &FileVersion,
    identity: &AudioCommitIdentity,
    worker_result: &Value,
) -> Result<Option<WorkerId>, VoomError> {
    let mut original_worker_id = None;
    if verification.report.get("remote_completion").is_some() {
        let original: super::RemoteMediaCompletionContext =
            super::decode_report(&verification.report, "remote_completion")?;
        original.validate()?;
        let lease = cp
            .leases
            .get_in_tx(tx, original.lease_id)
            .await?
            .ok_or_else(missing)?;
        let probe: voom_worker_protocol::OwnerProbeEvidence =
            super::decode_report(&verification.report, "owner_probe")?;
        super::validate_stored_completion_in_tx(
            cp,
            tx,
            verification,
            identity.commit_id,
            &original,
            &probe,
        )
        .await?;
        super::validate_owner_metadata_in_tx(tx, verification, identity.commit_id, &original)
            .await?;
        if super::owner_worker_result(&original.result).as_ref() != Some(worker_result)
            || original.source_file_version_id != identity.source_id
            || verification.worker_id != original.worker_id
            || verification.workflow_ticket_id.is_some()
            || verification.workflow_lease_id.is_some()
            || lease.ticket_id != original.ticket_id
            || lease.worker_id != original.worker_id
            || lease.state != voom_store::repo::execution::leases::LeaseState::Released
            || probe.worker_id != identity.probe_worker_id
            || probe.result.snapshot != snapshot.payload
            || probe.result.pre_probe.size_bytes != version.size_bytes
            || probe.result.pre_probe.content_hash != version.content_hash
            || probe.result.post_probe != probe.result.pre_probe
        {
            return Err(VoomError::database(
                "committed audio original owner provenance differs".to_owned(),
            ));
        }
        original_worker_id = Some(original.worker_id);
    } else if verification.report.get("owner_probe").is_some() {
        return Err(VoomError::database(
            "committed owner audio lost original completion provenance".to_owned(),
        ));
    }
    if super::normalized_snapshot(snapshot.payload.clone())
        .map_err(|error| VoomError::database(format!("committed audio probe snapshot: {error}")))?
        != snapshot.payload
    {
        return Err(VoomError::database(
            "committed audio probe snapshot lacks canonical identities".to_owned(),
        ));
    }
    Ok(original_worker_id)
}
