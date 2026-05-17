//! Artifact-lifecycle use cases. Each method composes the matching
//! `ArtifactRepo` `_in_tx` call with the appropriate
//! `EventRepo::append_in_tx` inside one transaction.

use time::OffsetDateTime;
use voom_core::{ArtifactHandleId, ArtifactLocationId, VoomError};
use voom_events::payload::{
    ArtifactHandleCreatedPayload, ArtifactLineageRecordedPayload, ArtifactLocationRecordedPayload,
    ArtifactLocationRetiredPayload,
};
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::artifacts::{
    ArtifactHandle, ArtifactLineage, ArtifactLocation, ArtifactRepo, NewArtifactHandle,
    NewArtifactLineage, NewArtifactLocation,
};

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Create an artifact handle and emit `artifact_handle.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_artifact_handle(
        &self,
        input: NewArtifactHandle,
    ) -> Result<ArtifactHandle, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let occurred = input.created_at;
        let handle = self.artifacts.create_handle_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::ArtifactHandleCreated,
            SubjectType::ArtifactHandle,
            Some(handle.id.0),
            occurred,
            Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
                artifact_handle_id: handle.id.0,
                privacy_class: handle.privacy_class.clone(),
                durability_class: handle.durability_class.clone(),
                mutability: handle.mutability.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        let occurred = input.observed_at;
        let location = self.artifacts.record_location_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::ArtifactLocationRecorded,
            SubjectType::ArtifactLocation,
            Some(location.id.0),
            occurred,
            Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
                artifact_location_id: location.id.0,
                artifact_handle_id: location.artifact_handle_id.0,
                kind: location.kind.clone(),
                value: location.value.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        self.artifacts
            .retire_location_in_tx(&mut tx, id, now)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::ArtifactLocationRetired,
            SubjectType::ArtifactLocation,
            Some(id.0),
            now,
            Event::ArtifactLocationRetired(ArtifactLocationRetiredPayload {
                artifact_location_id: id.0,
                artifact_handle_id: handle_id.0,
            }),
        )
        .await?;
        commit_tx(tx).await?;
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
        let mut tx = begin_tx(&self.pool).await?;
        let occurred = input.recorded_at;
        let parent = input.parent_artifact_id.0;
        let child = input.child_artifact_id.0;
        let op = input.operation.clone();
        let lineage = self.artifacts.record_lineage_in_tx(&mut tx, input).await?;
        append_event(
            &self.events,
            &mut tx,
            EventKind::ArtifactLineageRecorded,
            SubjectType::ArtifactHandle,
            Some(child),
            occurred,
            Event::ArtifactLineageRecorded(ArtifactLineageRecordedPayload {
                artifact_lineage_id: lineage.id,
                parent_artifact_id: parent,
                child_artifact_id: child,
                operation: op,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(lineage)
    }
}

#[cfg(test)]
#[path = "artifacts_test.rs"]
mod tests;
