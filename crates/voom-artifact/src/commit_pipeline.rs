use time::OffsetDateTime;
use voom_core::ids::ArtifactCommitRecordId;
use voom_core::{ArtifactHandleId, VoomError};
use voom_events::{Event, EventEnvelope, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitRepo, NewArtifactCommitRecord,
};
use voom_store::repo::events::{EventRepo, SqliteEventRepo};

#[derive(Debug)]
pub enum PendingCommitRecordError {
    BeforePending(VoomError),
    AfterPending(VoomError),
}

impl PendingCommitRecordError {
    #[must_use]
    pub fn into_inner(self) -> VoomError {
        match self {
            Self::BeforePending(err) | Self::AfterPending(err) => err,
        }
    }
}

pub async fn create_pending_commit_with_started_event_in_tx<R>(
    artifacts: &R,
    events: &SqliteEventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: NewArtifactCommitRecord,
    started_event: impl FnOnce(ArtifactCommitRecordId) -> Event,
) -> Result<ArtifactCommitRecord, PendingCommitRecordError>
where
    R: ArtifactCommitRepo + ?Sized,
{
    let artifact_handle_id = input.artifact_handle_id;
    let started_at = input.started_at;
    let record = artifacts
        .create_pending_commit_in_tx(tx, input)
        .await
        .map_err(PendingCommitRecordError::BeforePending)?;
    append_commit_event_in_tx(
        events,
        tx,
        artifact_handle_id,
        started_at,
        started_event(record.id),
    )
    .await
    .map_err(PendingCommitRecordError::AfterPending)?;
    Ok(record)
}

#[derive(Debug)]
pub struct RecoveryRequiredCommit {
    pub commit_record_id: ArtifactCommitRecordId,
    pub artifact_handle_id: ArtifactHandleId,
    pub failure: ArtifactCommitFailure,
    pub recovery_reason: String,
    pub event: Event,
    pub occurred_at: OffsetDateTime,
}

pub async fn mark_recovery_required_with_event_in_tx<R>(
    artifacts: &R,
    events: &SqliteEventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: RecoveryRequiredCommit,
) -> Result<ArtifactCommitRecord, VoomError>
where
    R: ArtifactCommitRepo + ?Sized,
{
    let recovered = artifacts
        .mark_commit_recovery_required_in_tx(
            tx,
            input.commit_record_id,
            input.failure,
            input.recovery_reason,
        )
        .await?;
    append_commit_event_in_tx(
        events,
        tx,
        input.artifact_handle_id,
        input.occurred_at,
        input.event,
    )
    .await?;
    Ok(recovered)
}

pub async fn append_commit_event_in_tx(
    events: &SqliteEventRepo,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    artifact_handle_id: ArtifactHandleId,
    occurred_at: OffsetDateTime,
    payload: Event,
) -> Result<(), VoomError> {
    events
        .append_in_tx(
            tx,
            EventEnvelope {
                occurred_at,
                subject_type: SubjectType::ArtifactHandle,
                subject_id: Some(artifact_handle_id.0),
                trace_id: None,
                payload,
            },
        )
        .await?;
    Ok(())
}
