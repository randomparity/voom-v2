//! Relational publication after every extraction member's capability and
//! applied receipt has passed the final transaction's guards.

use serde_json::json;
use sqlx::{Sqlite, Transaction};
use time::OffsetDateTime;
use voom_core::VoomError;
use voom_events::payload::{ArtifactCommitCompletedPayload, AssetBundleMemberAddedPayload};
use voom_events::{Event, SubjectType};
use voom_store::repo::media::artifact_commit_intents::ArtifactCommitIntent;
use voom_store::repo::media::artifacts::{ArtifactCommitRecord, NewSidecarArtifactCommit};
use voom_store::repo::media::audio_extract_operations::{
    AudioExtractOperationRecord, NewAudioExtractClaim, NewFinalizedAudioExtractOutput,
    SqliteAudioExtractOperationRepo,
};
use voom_store::repo::media::bundles::{BundleMemberRole, NewBundleMember};
use voom_store::repo::media::identity::{FileLocationRepo, NewMediaSnapshot};
use voom_worker_protocol::{CommitGroupCompleteOutcome, CommitGroupResult, OwnerProbeEvidence};

use crate::ControlPlane;
use crate::cases::append_event;

pub(crate) async fn finish_extraction_group_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    operation: &AudioExtractOperationRecord,
    claim: &NewAudioExtractClaim,
    members: &[(ArtifactCommitIntent, ArtifactCommitRecord)],
    now: OffsetDateTime,
) -> Result<CommitGroupCompleteOutcome, VoomError> {
    if members.len() != operation.outputs.len() || members.is_empty() {
        return Err(VoomError::database(
            "group publication output set is incomplete".to_owned(),
        ));
    }
    // Decode all persisted completion/probe data and prove the common original
    // request before creating even the first catalog row.
    let contexts = read_member_evidence_in_tx(cp, tx, operation, claim, members).await?;
    let context = contexts[0].0.clone();
    if contexts.iter().any(|entry| entry.0 != context) {
        return Err(VoomError::database(
            "group members carry different source completions".to_owned(),
        ));
    }
    let mut results = Vec::with_capacity(members.len());
    let mut outputs = Vec::with_capacity(members.len());
    for (((intent, record), output), (_, probe, verification)) in
        members.iter().zip(&operation.outputs).zip(&contexts)
    {
        let (committed, snapshot) =
            create_member_catalog_in_tx(cp, tx, intent, record, probe, verification, now).await?;
        let (member, lineage_id) =
            bind_member_lineage_in_tx(cp, tx, operation, output, &committed, snapshot.id, now)
                .await?;
        append_event(
            &cp.events,
            tx,
            SubjectType::ArtifactHandle,
            Some(intent.artifact_handle_id.0),
            now,
            Event::ArtifactCommitCompleted(ArtifactCommitCompletedPayload {
                commit_record_id: record.id,
                artifact_handle_id: intent.artifact_handle_id,
                result_file_version_id: committed.file_version_id,
                result_file_location_id: committed.file_location_id,
                target_path: record.target_path.clone(),
                gate_evaluated_lease_ids: Vec::new(),
            }),
        )
        .await?;
        cp.artifact_commit_intents
            .mark_completed_in_tx(tx, intent.id, now)
            .await?;
        results.push(CommitGroupResult {
            intent_id: intent.id,
            commit_record_id: record.id,
            result_file_version_id: committed.file_version_id,
            result_file_location_id: committed.file_location_id,
        });
        outputs.push(crate::audio::ExecuteExtractAudioOutputReport {
            operation_output_id: output.id,
            output_id: output.output_id.clone(),
            source_file_version_id: operation.operation.source_file_version_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id,
            source_snapshot_stream_id: output.source_snapshot_stream_id.clone(),
            source_provider_stream_index: output.source_provider_stream_index,
            role: output.bundle_role.clone(),
            staged_artifact_handle_id: intent.artifact_handle_id,
            staged_artifact_location_id: verification.artifact_location_id,
            verification_id: verification.id,
            commit_record_id: record.id,
            result_file_version_id: committed.file_version_id,
            result_file_location_id: committed.file_location_id,
            result_file_asset_id: committed.file_asset_id.0,
            result_media_snapshot_id: snapshot.id,
            bundle_member_id: member.id,
            lineage_id,
            staging_path: std::path::PathBuf::from(&verification.path),
            target_path: std::path::PathBuf::from(&record.target_path),
        });
    }
    SqliteAudioExtractOperationRepo::complete_operation_in_tx(
        tx,
        operation.operation.id,
        claim,
        now,
    )
    .await?;
    let result = completion_result(&context, &outputs)?;
    super::finish_in_tx(cp, tx, &context, result, now).await?;
    Ok(CommitGroupCompleteOutcome {
        operation_id: operation.operation.id,
        results,
    })
}

type MemberEvidence = (
    super::RemoteMediaCompletionContext,
    OwnerProbeEvidence,
    voom_store::repo::media::artifacts::ArtifactVerification,
);

async fn read_member_evidence_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    operation: &AudioExtractOperationRecord,
    claim: &NewAudioExtractClaim,
    members: &[(ArtifactCommitIntent, ArtifactCommitRecord)],
) -> Result<Vec<MemberEvidence>, VoomError> {
    let mut contexts = Vec::with_capacity(members.len());
    for ((intent, record), output) in members.iter().zip(&operation.outputs) {
        if output.commit_record_id != Some(record.id)
            || output.verification_id != Some(intent.verification_id)
            || output.artifact_handle_id != Some(intent.artifact_handle_id)
        {
            return Err(VoomError::database(
                "group member/operation binding differs".to_owned(),
            ));
        }
        let verification = cp
            .artifacts
            .get_verification_in_tx(tx, record.verification_id)
            .await?
            .ok_or_else(|| VoomError::database("group verification missing".to_owned()))?;
        let context: super::RemoteMediaCompletionContext =
            super::decode_report(&verification.report, "remote_completion")?;
        context.validate()?;
        let probe: OwnerProbeEvidence = super::decode_report(&verification.report, "owner_probe")?;
        if output.probe_worker_id != Some(probe.worker_id)
            || output.probe_payload.as_ref() != Some(&probe.result.snapshot)
            || context.source_file_version_id != operation.operation.source_file_version_id
            || context.lease_id != claim.lease_id
        {
            return Err(VoomError::database(
                "group probe/source binding differs".to_owned(),
            ));
        }
        contexts.push((context, probe, verification));
    }
    Ok(contexts)
}

async fn create_member_catalog_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    intent: &ArtifactCommitIntent,
    record: &ArtifactCommitRecord,
    probe: &OwnerProbeEvidence,
    verification: &voom_store::repo::media::artifacts::ArtifactVerification,
    now: OffsetDateTime,
) -> Result<
    (
        voom_store::repo::media::artifacts::SidecarArtifactCommit,
        voom_store::repo::media::identity::MediaSnapshot,
    ),
    VoomError,
> {
    let target = voom_core::ProviderRelativeLocator::parse_database(
        "group target",
        &intent.target_provider_relative_locator,
    )?;
    let committed = cp
        .artifacts
        .record_verified_sidecar_commit_rows_in_tx(
            tx,
            NewSidecarArtifactCommit {
                commit_record_id: record.id,
                target_path: record.target_path.clone(),
                storage_root_id: intent.target_storage_root_id,
                provider_relative_locator: target,
                content_hash: intent.expected_facts.content_hash.clone(),
                size_bytes: intent.expected_facts.size_bytes,
                observed_at: intent.authorized_at.unwrap_or(record.started_at),
                finished_at: now,
            },
        )
        .await?;
    cp.artifacts
        .retire_location_in_tx(tx, verification.artifact_location_id, now)
        .await?;
    cp.identity
        .retire_file_location_in_tx(
            tx,
            intent.staging_location_id,
            now,
            intent.staging_location_epoch,
        )
        .await?;
    let snapshot = crate::media_snapshot::record_with_event_in_tx(
        cp,
        tx,
        NewMediaSnapshot {
            file_version_id: committed.file_version_id,
            probed_by: Some(probe.worker_id),
            probed_at: now,
            payload: probe.result.snapshot.clone(),
        },
    )
    .await?;
    Ok((committed, snapshot))
}

fn completion_result(
    context: &super::RemoteMediaCompletionContext,
    outputs: &[crate::audio::ExecuteExtractAudioOutputReport],
) -> Result<serde_json::Value, VoomError> {
    let first = &outputs[0];
    let mut result = context.result.clone();
    let object = result
        .as_object_mut()
        .ok_or_else(|| VoomError::database("group completion result is not object".to_owned()))?;
    object.extend(serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(json!({
        "job_id": context.job_id, "ticket_id": context.ticket_id, "lease_id": context.lease_id,
        "source_file_version_id": context.source_file_version_id, "source_file_location_id": context.source_location_id,
        "staged_artifact_handle_id": first.staged_artifact_handle_id, "staged_artifact_location_id": first.staged_artifact_location_id,
        "verification_id": first.verification_id, "commit_record_id": first.commit_record_id,
        "result_file_version_id": first.result_file_version_id, "result_file_location_id": first.result_file_location_id,
        "result_media_snapshot_id": first.result_media_snapshot_id, "outputs": outputs,
    })).map_err(|error| VoomError::Internal(format!("encode extraction result: {error}")))?);
    Ok(result)
}

async fn bind_member_lineage_in_tx(
    cp: &ControlPlane,
    tx: &mut Transaction<'_, Sqlite>,
    operation: &AudioExtractOperationRecord,
    output: &voom_store::repo::media::audio_extract_operations::AudioExtractOperationOutput,
    committed: &voom_store::repo::media::artifacts::SidecarArtifactCommit,
    snapshot_id: voom_core::MediaSnapshotId,
    now: OffsetDateTime,
) -> Result<(voom_store::repo::media::bundles::BundleMember, u64), VoomError> {
    let role = match output.bundle_role.as_str() {
        "commentary_audio" => BundleMemberRole::CommentaryAudio,
        "external_audio" => BundleMemberRole::ExternalAudio,
        _ => {
            return Err(VoomError::database(
                "group output role is outside extraction vocabulary".to_owned(),
            ));
        }
    };
    let member = cp
        .bundles
        .add_member_in_tx(
            tx,
            NewBundleMember {
                bundle_id: operation.operation.source_bundle_id,
                file_asset_id: committed.file_asset_id,
                role,
            },
        )
        .await?;
    append_event(
        &cp.events,
        tx,
        SubjectType::AssetBundle,
        Some(member.bundle_id.0),
        now,
        Event::AssetBundleMemberAdded(AssetBundleMemberAddedPayload {
            bundle_id: member.bundle_id,
            file_asset_id: member.file_asset_id,
            role: member.role.as_str().to_owned(),
        }),
    )
    .await?;
    let lineage_id = SqliteAudioExtractOperationRepo::record_finalized_output_in_tx(
        tx,
        &NewFinalizedAudioExtractOutput {
            operation_output_id: output.id,
            source_file_version_id: operation.operation.source_file_version_id,
            source_media_snapshot_id: operation.operation.source_media_snapshot_id,
            source_snapshot_stream_id: output.source_snapshot_stream_id.clone(),
            source_provider_stream_index: output.source_provider_stream_index,
            result_file_asset_id: committed.file_asset_id.0,
            result_file_version_id: committed.file_version_id,
            result_file_location_id: committed.file_location_id,
            result_media_snapshot_id: snapshot_id,
            bundle_member_id: member.id,
            recorded_at: now,
        },
    )
    .await?;
    Ok((member, lineage_id))
}
