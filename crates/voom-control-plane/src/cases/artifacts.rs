//! Artifact-lifecycle use cases. Each method composes the matching
//! `ArtifactRepo` `_in_tx` call with the appropriate
//! `EventRepo::append_in_tx` inside one transaction.

use time::OffsetDateTime;
use voom_core::{ArtifactHandleId, ArtifactLocationId, VoomError};
use voom_events::payload::{
    ArtifactHandleCreatedPayload, ArtifactLineageRecordedPayload, ArtifactLocationRecordedPayload,
    ArtifactLocationRetiredPayload,
};
use voom_events::{Event, EventEnvelope, EventKind, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactHandle, ArtifactLineage, ArtifactLocation, ArtifactRepo, NewArtifactHandle,
    NewArtifactLineage, NewArtifactLocation,
};
use voom_store::repo::events::EventRepo;

use crate::ControlPlane;

impl ControlPlane {
    /// Create an artifact handle and emit `artifact_handle.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_artifact_handle(
        &self,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let occurred = input.created_at;
        let handle = self.artifacts.create_handle_in_tx(&mut tx, input).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::ArtifactHandleCreated,
                    occurred_at: occurred,
                    subject_type: SubjectType::ArtifactHandle,
                    subject_id: Some(handle.id.0),
                    trace_id: None,
                    payload: Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
                        artifact_handle_id: handle.id.0,
                        privacy_class: handle.privacy_class.clone(),
                        durability_class: handle.durability_class.clone(),
                        mutability: handle.mutability.clone(),
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(handle)
    }

    /// Record an artifact location and emit `artifact_location.recorded`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn record_artifact_location(
        &self,
        input: NewArtifactLocation,
    ) -> Result<ArtifactLocation, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let occurred = input.observed_at;
        let location = self.artifacts.record_location_in_tx(&mut tx, input).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::ArtifactLocationRecorded,
                    occurred_at: occurred,
                    subject_type: SubjectType::ArtifactLocation,
                    subject_id: Some(location.id.0),
                    trace_id: None,
                    payload: Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
                        artifact_location_id: location.id.0,
                        artifact_handle_id: location.artifact_handle_id.0,
                        kind: location.kind.clone(),
                        value: location.value.clone(),
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(location)
    }

    /// Retire an artifact location and emit `artifact_location.retired`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn retire_artifact_location(
        &self,
        id: ArtifactLocationId,
        handle_id: ArtifactHandleId,
        now: OffsetDateTime,
    ) -> Result<(), VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        self.artifacts
            .retire_location_in_tx(&mut tx, id, now)
            .await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::ArtifactLocationRetired,
                    occurred_at: now,
                    subject_type: SubjectType::ArtifactLocation,
                    subject_id: Some(id.0),
                    trace_id: None,
                    payload: Event::ArtifactLocationRetired(ArtifactLocationRetiredPayload {
                        artifact_location_id: id.0,
                        artifact_handle_id: handle_id.0,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(())
    }

    /// Record artifact lineage and emit `artifact_lineage.recorded`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn record_artifact_lineage(
        &self,
        input: NewArtifactLineage,
    ) -> Result<ArtifactLineage, VoomError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| VoomError::Database(format!("begin: {e}")))?;
        let occurred = input.recorded_at;
        let parent = input.parent_artifact_id.0;
        let child = input.child_artifact_id.0;
        let op = input.operation.clone();
        let lineage = self.artifacts.record_lineage_in_tx(&mut tx, input).await?;
        self.events
            .append_in_tx(
                &mut tx,
                EventEnvelope {
                    kind: EventKind::ArtifactLineageRecorded,
                    occurred_at: occurred,
                    subject_type: SubjectType::ArtifactHandle,
                    subject_id: Some(child),
                    trace_id: None,
                    payload: Event::ArtifactLineageRecorded(ArtifactLineageRecordedPayload {
                        artifact_lineage_id: lineage.id,
                        parent_artifact_id: parent,
                        child_artifact_id: child,
                        operation: op,
                    }),
                },
            )
            .await?;
        tx.commit()
            .await
            .map_err(|e| VoomError::Database(format!("commit: {e}")))?;
        Ok(lineage)
    }
}

#[cfg(test)]
#[path = "artifacts_test.rs"]
mod tests;
