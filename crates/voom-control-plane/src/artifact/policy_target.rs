use voom_core::{FileLocationId, FileVersionId, VoomError};
use voom_events::payload::{ArtifactHandleCreatedPayload, ArtifactLocationRecordedPayload};
use voom_events::{Event, SubjectType};
use voom_store::repo::media::artifacts::PolicyArtifactTarget;

use crate::ControlPlane;
use crate::cases::{append_event, commit_tx};
use voom_store::tx::begin_read_then_write;

impl ControlPlane {
    pub(crate) async fn resolve_policy_artifact_target(
        &self,
        file_version_id: FileVersionId,
        file_location_id: Option<FileLocationId>,
    ) -> Result<PolicyArtifactTarget, VoomError> {
        let selected = crate::operation_source::select_local_source(
            self,
            "policy artifact verification",
            file_version_id,
            file_location_id,
        )
        .await?;
        let (selected_root_id, selected_relative_locator) = selected.location.rooted_address()?;
        let canonical_path = selected.canonical_path.to_str().ok_or_else(|| {
            VoomError::Config(
                "policy artifact verification canonical path must be valid UTF-8".into(),
            )
        })?;
        let mut tx =
            begin_read_then_write(&self.pool, "policy_target: resolve_policy_artifact_target")
                .await?;
        let now = self.clock().now();
        let resolution = self
            .artifacts
            .resolve_policy_artifact_target_in_tx(
                &mut tx,
                file_version_id,
                file_location_id,
                canonical_path,
                now,
            )
            .await?;
        if resolution.target.file_location_id != selected.location.id
            || resolution.target.storage_root_id != selected_root_id
            || resolution.target.provider_relative_locator != *selected_relative_locator
        {
            return Err(VoomError::Conflict(format!(
                "policy artifact location {} changed during resolution",
                selected.location.id
            )));
        }
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
                    kind: location.kind.to_string(),
                    value: location.value.clone(),
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(resolution.target)
    }
}
