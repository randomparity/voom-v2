use time::OffsetDateTime;
use voom_core::ids::ArtifactCommitRecordId;
use voom_core::{ArtifactHandleId, VoomError};
use voom_events::{Event, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactRepo, NewArtifactCommitRecord,
};

use crate::ControlPlane;
use crate::cases::append_event;

#[derive(Debug)]
pub(crate) enum PendingCommitRecordError {
    BeforePending(VoomError),
    AfterPending(VoomError),
}

impl PendingCommitRecordError {
    pub(crate) fn into_inner(self) -> VoomError {
        match self {
            Self::BeforePending(err) | Self::AfterPending(err) => err,
        }
    }
}

pub(crate) async fn create_pending_commit_with_started_event_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewArtifactCommitRecord,
    started_event: impl FnOnce(ArtifactCommitRecordId) -> Event,
) -> Result<ArtifactCommitRecord, PendingCommitRecordError> {
    let artifact_handle_id = input.artifact_handle_id;
    let started_at = input.started_at;
    let record = cp
        .artifacts
        .create_pending_commit_in_tx(tx, input)
        .await
        .map_err(PendingCommitRecordError::BeforePending)?;
    append_commit_event_in_tx(
        cp,
        tx,
        artifact_handle_id,
        started_at,
        started_event(record.id),
    )
    .await
    .map_err(PendingCommitRecordError::AfterPending)?;
    Ok(record)
}

pub(crate) struct RecoveryRequiredCommit {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub failure: ArtifactCommitFailure,
    pub recovery_reason: String,
    pub event: Event,
    pub occurred_at: OffsetDateTime,
}

pub(crate) async fn mark_recovery_required_with_event_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: RecoveryRequiredCommit,
) -> Result<ArtifactCommitRecord, VoomError> {
    let recovered = cp
        .artifacts
        .mark_commit_recovery_required_in_tx(
            tx,
            input.commit_record_id,
            input.failure,
            input.recovery_reason,
        )
        .await?;
    append_commit_event_in_tx(
        cp,
        tx,
        input.artifact_handle_id,
        input.occurred_at,
        input.event,
    )
    .await?;
    Ok(recovered)
}

pub(crate) async fn append_commit_event_in_tx(
    cp: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    occurred_at: OffsetDateTime,
    event: Event,
) -> Result<(), VoomError> {
    append_event(
        &cp.events,
        tx,
        SubjectType::ArtifactHandle,
        Some(artifact_handle_id.0),
        occurred_at,
        event,
    )
    .await
}
