use std::collections::HashMap;
use voom_core::{FileVersionId, MediaSnapshotId, PolicyInputSetId, StorageRootId, VoomError};
use voom_policy::{
    MediaSnapshotInput, PolicyInputSetDraft, PolicyInputSourceKind, TargetRef,
    ValidatedPolicyInputSetDraft,
};
use voom_store::repo::{
    media::identity::{
        FileLocationAddress, FileLocationRepo, FileVersionRepo, MediaSnapshotFileVersionQuery,
        MediaSnapshotRepo,
    },
    policy::policy_inputs::{PolicyInputSet, PolicyInputSetSummary},
};

use crate::ControlPlane;
use crate::cases::begin_immediate_tx;
use crate::media_snapshot::stream_summary_from_snapshot_payload;

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

#[derive(Debug, Clone)]
pub struct RootScopedScanInput {
    pub slug: String,
    pub library_root_id: StorageRootId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootScopedScanInputResult {
    pub input_set_id: PolicyInputSetId,
    pub slug: String,
    pub library_root_id: StorageRootId,
    /// Live file-versions under the root whose latest snapshot had a video
    /// stream.
    pub included_count: u32,
    /// Live file-versions skipped: no live location under the root, or no
    /// snapshot / no video stream.
    pub skipped_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeScanInputResult {
    pub input_set_id: PolicyInputSetId,
    pub slug: String,
    /// Live file-versions whose latest snapshot had a video stream.
    pub included_count: u32,
    /// Live file-versions skipped because they had no effectively available
    /// rooted location, no snapshot, or no video stream.
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
        let input = ValidatedPolicyInputSetDraft::new(input)
            .map_err(|error| VoomError::PolicyValidationError(error.message()))?;
        self.persist_policy_input_set(input).await
    }

    async fn persist_policy_input_set(
        &self,
        input: ValidatedPolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        let query = MediaSnapshotFileVersionQuery::new(
            input
                .as_draft()
                .media_snapshots
                .iter()
                .filter_map(|snapshot| snapshot.existing_media_snapshot_id),
        )?;
        let mut tx = begin_immediate_tx(&self.pool).await?;
        let snapshot_versions: HashMap<MediaSnapshotId, FileVersionId> = self
            .identity
            .get_media_snapshot_file_versions_in_tx(&mut tx, &query)
            .await?
            .into_iter()
            .collect();
        validate_snapshot_links(input.as_draft(), &snapshot_versions)?;
        let out = self
            .policy_inputs
            .create_input_set_in_tx(&mut tx, input)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn create_scan_policy_input_set(
        &self,
        input: PolicyInputSetDraft,
    ) -> Result<PolicyInputSet, VoomError> {
        let input = if input.media_snapshots.is_empty() && input.bundle_targets.is_empty() {
            ValidatedPolicyInputSetDraft::new_empty_scan(input)
        } else {
            ValidatedPolicyInputSetDraft::new(input)
        }
        .map_err(|error| VoomError::PolicyValidationError(error.message()))?;
        self.persist_policy_input_set(input).await
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
        let draft = ValidatedPolicyInputSetDraft::new(draft)
            .map_err(|error| VoomError::PolicyValidationError(error.message()))?;
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
    /// file-versions with an effectively available rooted location whose
    /// latest media snapshot has a video stream". Each such file contributes
    /// one media-snapshot member; quarantined, unavailable, non-video, or
    /// unprobeable file-versions are skipped and counted.
    ///
    /// # Errors
    /// Propagates policy validation and repository errors.
    pub async fn create_policy_input_set_from_whole_scan(
        &self,
        input: WholeScanInput,
    ) -> Result<WholeScanInputResult, VoomError> {
        let versions = self.identity.list_live_file_versions().await?;
        let mut root_availability = HashMap::new();
        let mut media_snapshots: Vec<MediaSnapshotInput> = Vec::new();
        let mut included_count: u32 = 0;
        let mut skipped_count: u32 = 0;
        for version in versions {
            if !self
                .file_version_has_available_root(version.id, &mut root_availability)
                .await?
            {
                skipped_count += 1;
                continue;
            }
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
            included_count += 1;
            let member = crate::media_snapshot::planning_input(included_count, &snapshot);
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
        let created = self.create_scan_policy_input_set(draft).await?;
        Ok(WholeScanInputResult {
            input_set_id: created.id,
            slug: created.slug,
            included_count,
            skipped_count,
        })
    }

    /// Create one durable policy input set covering the currently-scanned video
    /// files under a single library root.
    ///
    /// Scopes the whole-scan anchor to file-versions with a live local location
    /// whose canonical path is the root path or a component-wise descendant of
    /// it, replacing the un-scoped whole-library selection. This is the
    /// per-library input builder the "DB-per-library" workaround stood in for
    /// (ADR 0027).
    ///
    /// # Errors
    /// Returns `NotFound` for a missing root; propagates policy validation and
    /// repository errors.
    pub async fn create_policy_input_set_from_root(
        &self,
        input: RootScopedScanInput,
    ) -> Result<RootScopedScanInputResult, VoomError> {
        let effective = self
            .effective_library_root(input.library_root_id)
            .await?
            .ok_or_else(|| {
                VoomError::NotFound(format!("library root {} not found", input.library_root_id))
            })?;
        if !effective.available {
            return Err(VoomError::Config(format!(
                "library root {} unavailable: {}",
                input.library_root_id,
                effective.reason.as_str()
            )));
        }

        let versions = self.identity.list_live_file_versions().await?;
        let mut media_snapshots: Vec<MediaSnapshotInput> = Vec::new();
        let mut included_count: u32 = 0;
        let mut skipped_count: u32 = 0;
        for version in versions {
            if !self
                .file_version_is_under_root(version.id, input.library_root_id)
                .await?
            {
                skipped_count += 1;
                continue;
            }
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
            included_count += 1;
            let member = crate::media_snapshot::planning_input(included_count, &snapshot);
            media_snapshots.push(member);
        }

        let draft = PolicyInputSetDraft {
            slug: input.slug.clone(),
            display_name: input.slug.clone(),
            schema_version: 1,
            source_kind: PolicyInputSourceKind::Imported,
            created_at: self.clock().now(),
            description: None,
            fixture_labels: vec![format!("root-scan-{}", input.slug)],
            synthetic_targets: Vec::new(),
            media_snapshots,
            identity_evidence: Vec::new(),
            bundle_targets: Vec::new(),
            quality_profiles: Vec::new(),
            issues: Vec::new(),
        };
        let created = self.create_scan_policy_input_set(draft).await?;
        Ok(RootScopedScanInputResult {
            input_set_id: created.id,
            slug: created.slug,
            library_root_id: input.library_root_id,
            included_count,
            skipped_count,
        })
    }

    /// True when a file-version has a live location in the selected root.
    async fn file_version_is_under_root(
        &self,
        file_version_id: FileVersionId,
        storage_root_id: StorageRootId,
    ) -> Result<bool, VoomError> {
        let locations = self
            .identity
            .list_live_file_locations_by_version(file_version_id)
            .await?;
        Ok(locations.iter().any(|location| {
            matches!(
                location.address,
                FileLocationAddress::Rooted {
                    storage_root_id: id,
                    ..
                } if id == storage_root_id
            )
        }))
    }

    async fn file_version_has_available_root(
        &self,
        file_version_id: FileVersionId,
        root_availability: &mut HashMap<StorageRootId, bool>,
    ) -> Result<bool, VoomError> {
        let locations = self
            .identity
            .list_live_file_locations_by_version(file_version_id)
            .await?;
        for location in locations {
            let FileLocationAddress::Rooted {
                storage_root_id, ..
            } = location.address
            else {
                continue;
            };
            let available = if let Some(available) = root_availability.get(&storage_root_id) {
                *available
            } else {
                let effective = self
                    .effective_library_root(storage_root_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::database(format!(
                            "file version {file_version_id} references missing storage root \
                             {storage_root_id}"
                        ))
                    })?;
                root_availability.insert(storage_root_id, effective.available);
                effective.available
            };
            if available {
                return Ok(true);
            }
        }
        Ok(false)
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

fn validate_snapshot_links(
    input: &PolicyInputSetDraft,
    snapshot_versions: &HashMap<MediaSnapshotId, FileVersionId>,
) -> Result<(), VoomError> {
    for member in &input.media_snapshots {
        let Some(snapshot_id) = member.existing_media_snapshot_id else {
            continue;
        };
        let Some(snapshot_version_id) = snapshot_versions.get(&snapshot_id) else {
            return Err(VoomError::NotFound(format!(
                "media snapshot {snapshot_id} not found"
            )));
        };
        let TargetRef::FileVersion {
            id: target_version_id,
        } = member.target
        else {
            return Err(VoomError::Conflict(format!(
                "media snapshot {snapshot_id} linked from policy input member ordinal {} \
                 must target a file version",
                member.ordinal
            )));
        };
        if *snapshot_version_id != target_version_id {
            return Err(VoomError::Conflict(format!(
                "media snapshot {snapshot_id} does not belong to file version {target_version_id}"
            )));
        }
    }
    Ok(())
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

#[cfg(test)]
#[path = "policy_inputs_test.rs"]
mod tests;
