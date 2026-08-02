use voom_core::{FileLocationId, FileVersionId, VoomError};
use voom_events::payload::{ArtifactHandleCreatedPayload, ArtifactLocationRecordedPayload};
use voom_events::{Event, SubjectType};
use voom_store::repo::media::artifacts::PolicyArtifactTarget;

use crate::ControlPlane;
use crate::cases::{append_event, begin_immediate_tx, commit_tx};

impl ControlPlane {
    pub(crate) async fn resolve_policy_artifact_target(
        &self,
        file_version_id: FileVersionId,
        file_location_id: Option<FileLocationId>,
    ) -> Result<PolicyArtifactTarget, VoomError> {
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let now = self.clock().now();
        let resolution = self
            .artifacts
            .resolve_policy_artifact_target_in_tx(&mut tx, file_version_id, file_location_id, now)
            .await?;
        if let Some(handle) = &resolution.created_handle {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::ArtifactHandle,
                Some(handle.id.0),
                now,
                Event::ArtifactHandleCreated(ArtifactHandleCreatedPayload {
                    artifact_handle_id: handle.id,
                    privacy_class: handle.privacy_class.clone(),
                    durability_class: handle.durability_class.clone(),
                    mutability: handle.mutability.clone(),
                }),
            )
            .await?;
        }
        if let Some(location) = &resolution.created_location {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::ArtifactLocation,
                Some(location.id.0),
                now,
                Event::ArtifactLocationRecorded(ArtifactLocationRecordedPayload {
                    artifact_location_id: location.id,
                    artifact_handle_id: location.artifact_handle_id,
                    kind: location.kind.clone(),
                    value: location.value.clone(),
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(resolution.target)
    }
}
