use voom_core::{FileVersionId, MediaSnapshotId, PolicyInputSetId, VoomError};
use voom_policy::{MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef};
use voom_store::repo::{
    identity::IdentityRepo,
    policy_inputs::{PolicyInputSet, PolicyInputSetSummary},
};

use crate::ControlPlane;

use super::{begin_tx, commit_tx};

#[derive(Debug, Clone)]
pub struct PolicyInputFromScanInput {
    pub slug: String,
    pub file_version_id: FileVersionId,
    pub media_snapshot_id: MediaSnapshotId,
    pub container: String,
    pub video_codec: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyInputFromScanResult {
    pub input_set_id: PolicyInputSetId,
    pub slug: String,
    pub source_kind: PolicyInputSourceKind,
    pub file_version_id: FileVersionId,
    pub media_snapshot_id: MediaSnapshotId,
}

#[derive(Debug, Clone)]
pub struct WholeScanInput {
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeScanInputResult {
    pub input_set_id: PolicyInputSetId,
    pub slug: String,
    /// Live file-versions whose latest snapshot had a video stream.
    pub included_count: u32,
    /// Live file-versions skipped because they had no snapshot or no
    /// video stream (non-video / unprobeable).
    pub skipped_count: u32,
}

impl ControlPlane {
    /// Create a durable policy input set without emitting events in Sprint 3.
    ///
    /// # Errors
    /// Propagates policy validation and repository errors.
    pub async fn create_policy_input_set(
        &self,
        input: voom_policy::PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self
            .policy_inputs
            .create_input_set_in_tx(&mut tx, input)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    /// Create a durable policy input set from scan-created durable rows.
    ///
    /// # Errors
    /// Returns `NOT_FOUND` for missing scan rows, `CONFLICT` for stale or
    /// mismatched scan rows, and propagates policy validation/repository errors.
    pub async fn create_policy_input_set_from_scan(
        &self,
        input: PolicyInputFromScanInput,
    ) -> Result<PolicyInputFromScanResult, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let file_version = self
            .identity
            .get_file_version_in_tx(&mut tx, input.file_version_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("file version {} not found", input.file_version_id))
            })?;
        if file_version.retired_at.is_some() {
            return Err(VoomError::Conflict(format!(
                "file version {} is retired",
                input.file_version_id
            )));
        }
        let snapshot = self
            .identity
            .get_media_snapshot_in_tx(&mut tx, input.media_snapshot_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!(
                    "media snapshot {} not found",
                    input.media_snapshot_id
                ))
            })?;
        if snapshot.file_version_id != input.file_version_id {
            return Err(VoomError::Conflict(format!(
                "media snapshot {} does not belong to file version {}",
                input.media_snapshot_id, input.file_version_id
            )));
        }

        let source_kind = PolicyInputSourceKind::Imported;
        let draft = PolicyInputSetDraft {
            slug: input.slug.clone(),
            display_name: input.slug.clone(),
            schema_version: 1,
            source_kind,
            created_at: self.clock().now(),
            description: None,
            fixture_labels: vec![format!("scan-{}", input.slug)],
            synthetic_targets: Vec::new(),
            media_snapshots: vec![MediaSnapshotInput {
                ordinal: 1,
                target: TargetRef::FileVersion {
                    id: input.file_version_id,
                },
                container: Some(input.container),
                stream_summary: stream_summary_from_snapshot_payload(&snapshot.payload),
                video_codec: Some(input.video_codec),
                width: None,
                height: None,
                hdr: None,
                bitrate: None,
                duration_millis: None,
                audio_languages: Vec::new(),
                subtitle_languages: Vec::new(),
                health_flags: Vec::new(),
                existing_media_snapshot_id: Some(input.media_snapshot_id),
            }],
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        };
        let created = self
            .policy_inputs
            .create_input_set_in_tx(&mut tx, draft)
            .await?;
        commit_tx(tx).await?;
        Ok(PolicyInputFromScanResult {
            input_set_id: created.id,
            slug: created.slug,
            source_kind,
            file_version_id: input.file_version_id,
            media_snapshot_id: input.media_snapshot_id,
        })
    }

    /// Create one durable policy input set covering every currently-scanned
    /// video file in the library.
    ///
    /// There is no durable scan id, so the anchor is "all live (non-retired)
    /// file-versions whose latest media snapshot has a video stream". Each
    /// such file contributes one media-snapshot member; non-video or
    /// unprobeable file-versions are skipped and counted.
    ///
    /// # Errors
    /// Propagates policy validation and repository errors.
    pub async fn create_policy_input_set_from_whole_scan(
        &self,
        input: WholeScanInput,
    ) -> Result<WholeScanInputResult, VoomError> {
        let versions = self.identity.list_live_file_versions().await?;
        let mut media_snapshots: Vec<MediaSnapshotInput> = Vec::new();
        let mut included_count: u32 = 0;
        let mut skipped_count: u32 = 0;
        for version in versions {
            let latest = self
                .identity
                .list_media_snapshots_by_version(version.id)
                .await?
                .into_iter()
                .next_back();
            let Some(snapshot) = latest.filter(|s| snapshot_has_video_stream(&s.payload)) else {
                skipped_count += 1;
                continue;
            };
            let mut member = crate::media_snapshot::planning_input(&snapshot);
            included_count += 1;
            member.ordinal = included_count;
            media_snapshots.push(member);
        }

        let draft = PolicyInputSetDraft {
            slug: input.slug.clone(),
            display_name: input.slug.clone(),
            schema_version: 1,
            source_kind: PolicyInputSourceKind::Imported,
            created_at: self.clock().now(),
            description: None,
            fixture_labels: vec![format!("whole-scan-{}", input.slug)],
            synthetic_targets: Vec::new(),
            media_snapshots,
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        };
        let created = self.create_policy_input_set(draft).await?;
        Ok(WholeScanInputResult {
            input_set_id: created.id,
            slug: created.slug,
            included_count,
            skipped_count,
        })
    }

    /// Get a policy input set by id.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn get_policy_input_set(
        &self,
        id: PolicyInputSetId,
    ) -> Result<Option<PolicyInputSet>, VoomError> {
        self.policy_inputs.get_input_set(id).await
    }

    /// List policy input set summaries in repository order.
    ///
    /// # Errors
    /// Propagates repository errors.
    pub async fn list_policy_input_sets(&self) -> Result<Vec<PolicyInputSetSummary>, VoomError> {
        self.policy_inputs.list_input_sets().await
    }
}

fn snapshot_has_video_stream(payload: &serde_json::Value) -> bool {
    payload
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|streams| {
            streams.iter().any(|stream| {
                stream.get("kind").and_then(serde_json::Value::as_str) == Some("video")
            })
        })
}

pub(crate) fn stream_summary_from_snapshot_payload(
    payload: &serde_json::Value,
) -> serde_json::Value {
    let streams = payload
        .get("streams")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let video_stream_count = streams.as_array().map_or(0, |streams| {
        streams
            .iter()
            .filter(|stream| {
                stream.get("kind").and_then(serde_json::Value::as_str) == Some("video")
            })
            .count()
    });
    serde_json::json!({
        "video_stream_count": video_stream_count,
        "streams": streams,
    })
}

#[cfg(test)]
#[path = "policy_inputs_test.rs"]
mod tests;
