use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::Path;

use voom_core::{
    ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId, MediaSnapshotId,
    VoomError, WorkerId,
};
use voom_events::payload::{
    FileAssetCreatedPayload, FileLocationAliasedPayload, FileLocationRecordedPayload,
    FileVersionCreatedPayload, IdentityEvidenceRecordedPayload, MediaSnapshotRecordedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::identity::{
    DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, NewMediaSnapshot,
};
use voom_worker_protocol::ProbeFileResult;

use crate::ControlPlane;
use crate::cases::append_event;
use crate::scan::discovery::FileScanStatus;

pub use super::hash::ObservedFileFacts as ObservedCandidateFacts;

const CONTENT_DRIFT_MESSAGE: &str = "file changed between hashing and probing";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedScan {
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    pub media_snapshot_id: MediaSnapshotId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFileError {
    status: FileScanStatus,
    error_code: ErrorCode,
    failure_class: FailureClass,
    message: String,
}

impl ScanFileError {
    #[must_use]
    pub const fn status(&self) -> FileScanStatus {
        self.status
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        self.error_code
    }

    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn content_drift() -> Self {
        Self {
            status: FileScanStatus::FailedContentDrift,
            error_code: ErrorCode::ArtifactChecksumMismatch,
            failure_class: FailureClass::ArtifactChecksumMismatch,
            message: CONTENT_DRIFT_MESSAGE.to_owned(),
        }
    }
}

impl Display for ScanFileError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for ScanFileError {}

#[derive(Debug)]
pub enum ScanPersistError {
    File(ScanFileError),
    Store(VoomError),
}

impl Display for ScanPersistError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(err) => Display::fmt(err, f),
            Self::Store(err) => Display::fmt(err, f),
        }
    }
}

impl Error for ScanPersistError {}

impl From<ScanFileError> for ScanPersistError {
    fn from(value: ScanFileError) -> Self {
        Self::File(value)
    }
}

impl From<VoomError> for ScanPersistError {
    fn from(value: VoomError) -> Self {
        Self::Store(value)
    }
}

/// Verify that the worker probed the exact bytes the scanner hashed before
/// dispatch. Persistence treats any mismatch as content drift and leaves no
/// durable identity or snapshot rows for the changed file.
pub fn verify_probe_facts(
    candidate: &ObservedCandidateFacts,
    result: &ProbeFileResult,
) -> Result<(), ScanFileError> {
    if result.pre_probe.size_bytes == candidate.size_bytes
        && result.post_probe.size_bytes == candidate.size_bytes
        && result.pre_probe.content_hash == candidate.content_hash
        && result.post_probe.content_hash == candidate.content_hash
    {
        return Ok(());
    }
    Err(ScanFileError::content_drift())
}

/// Persist the identity rows and media snapshot produced by one successful
/// scan file probe.
///
/// # Errors
/// Returns [`ScanPersistError::File`] when worker probe facts drifted from
/// the original candidate facts, and [`ScanPersistError::Store`] for durable
/// store conflicts or database failures.
pub async fn persist_scanned_media_snapshot(
    control_plane: &ControlPlane,
    worker_id: WorkerId,
    canonical_path: &Path,
    candidate: &ObservedCandidateFacts,
    result: &ProbeFileResult,
) -> Result<PersistedScan, ScanPersistError> {
    verify_probe_facts(candidate, result)?;
    let location_value = canonical_path_value(canonical_path)?;

    let now = control_plane.clock().now();
    let mut tx = control_plane
        .pool
        .begin()
        .await
        .map_err(|e| VoomError::Database(format!("begin: {e}")))?;

    ensure_worker_live_in_tx(&mut tx, worker_id).await?;

    let outcome = control_plane
        .identity
        .record_discovered_file_in_tx(
            &mut tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value,
                content_hash: candidate.content_hash.clone(),
                size_bytes: candidate.size_bytes,
                observed_at: now,
                proof: None,
            },
            None,
        )
        .await?;
    let IngestedIds(file_asset_id, file_version_id, file_location_id) =
        emit_ingest_events(control_plane, &mut tx, &outcome, now).await?;
    let snapshot = control_plane
        .identity
        .record_media_snapshot_in_tx(
            &mut tx,
            NewMediaSnapshot {
                file_version_id,
                probed_by: Some(worker_id),
                probed_at: now,
                payload: result.snapshot.clone(),
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        &mut tx,
        SubjectType::MediaSnapshot,
        Some(snapshot.id.0),
        now,
        Event::MediaSnapshotRecorded(MediaSnapshotRecordedPayload {
            media_snapshot_id: snapshot.id.0,
            file_version_id: snapshot.file_version_id.0,
            probed_by_worker_id: snapshot.probed_by.map(|w| w.0),
            probed_at: snapshot.probed_at,
        }),
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("commit: {e}")))?;

    Ok(PersistedScan {
        file_asset_id,
        file_version_id,
        file_location_id,
        media_snapshot_id: snapshot.id,
    })
}

struct IngestedIds(FileAssetId, FileVersionId, FileLocationId);

#[expect(
    clippy::too_many_lines,
    reason = "mirrors the existing identity use-case event chain for one atomic scan transaction"
)]
async fn emit_ingest_events(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    outcome: &IngestOutcome,
    observed_at: time::OffsetDateTime,
) -> Result<IngestedIds, VoomError> {
    match outcome {
        IngestOutcome::NewFileAsset {
            file_asset_id,
            file_version_id,
            file_location_id,
            hash_match_evidence,
            path_rule_evidence,
        } => {
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileAsset,
                Some(file_asset_id.0),
                observed_at,
                Event::FileAssetCreated(FileAssetCreatedPayload {
                    file_asset_id: file_asset_id.0,
                }),
            )
            .await?;
            let version = control_plane
                .identity
                .get_file_version_in_tx(tx, *file_version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan persist: file_version {file_version_id} vanished"
                    ))
                })?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileVersion,
                Some(version.id.0),
                observed_at,
                Event::FileVersionCreated(FileVersionCreatedPayload {
                    file_version_id: version.id.0,
                    file_asset_id: version.file_asset_id.0,
                    content_hash: version.content_hash.clone(),
                    size_bytes: version.size_bytes,
                    produced_by: version.produced_by.as_str().to_owned(),
                    produced_from_version_id: version.produced_from_version_id.map(|id| id.0),
                }),
            )
            .await?;
            let location = control_plane
                .identity
                .get_file_location_in_tx(tx, *file_location_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan persist: file_location {file_location_id} vanished"
                    ))
                })?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileLocation,
                Some(location.id.0),
                observed_at,
                Event::FileLocationRecorded(FileLocationRecordedPayload {
                    file_location_id: location.id.0,
                    file_version_id: location.file_version_id.0,
                    kind: location.kind.as_str().to_owned(),
                    value: location.value,
                }),
            )
            .await?;
            for ev_id in [hash_match_evidence, path_rule_evidence]
                .into_iter()
                .flatten()
            {
                let evidence = control_plane
                    .identity
                    .get_identity_evidence_in_tx(tx, *ev_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!("scan persist: evidence {ev_id} vanished"))
                    })?;
                append_event(
                    &control_plane.events,
                    tx,
                    SubjectType::IdentityEvidence,
                    Some(evidence.id.0),
                    evidence.observed_at,
                    Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
                        evidence_id: evidence.id.0,
                        target_type: evidence.target_type.as_str().to_owned(),
                        target_id: evidence.target_id,
                        assertion_type: evidence.assertion_type.as_str().to_owned(),
                        provider: evidence.provider,
                        provider_version: evidence.provider_version,
                        confidence: evidence.confidence,
                        observed_at: evidence.observed_at,
                    }),
                )
                .await?;
            }
            Ok(IngestedIds(
                *file_asset_id,
                *file_version_id,
                *file_location_id,
            ))
        }
        IngestOutcome::AliasAttached {
            file_version_id,
            new_file_location_id,
        } => {
            let location = control_plane
                .identity
                .get_file_location_in_tx(tx, *new_file_location_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan persist: alias location {new_file_location_id} vanished"
                    ))
                })?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileLocation,
                Some(location.id.0),
                observed_at,
                Event::FileLocationAliased(FileLocationAliasedPayload {
                    file_location_id: location.id.0,
                    file_version_id: file_version_id.0,
                    kind: location.kind.as_str().to_owned(),
                    value: location.value,
                }),
            )
            .await?;
            let version = control_plane
                .identity
                .get_file_version_in_tx(tx, *file_version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan persist: file_version {file_version_id} vanished"
                    ))
                })?;
            Ok(IngestedIds(
                version.file_asset_id,
                *file_version_id,
                *new_file_location_id,
            ))
        }
    }
}

async fn ensure_worker_live_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
) -> Result<(), VoomError> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM workers WHERE id = ?")
        .bind(worker_id_as_i64(worker_id)?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("scan persist worker reload: {e}")))?;
    match status.as_deref() {
        Some("retired") | None => Err(VoomError::Conflict(format!(
            "scan persist rejected worker {worker_id}: missing or retired"
        ))),
        Some(_) => Ok(()),
    }
}

fn worker_id_as_i64(worker_id: WorkerId) -> Result<i64, VoomError> {
    i64::try_from(worker_id.0)
        .map_err(|_| VoomError::Internal(format!("worker id out of sqlite range: {worker_id}")))
}

fn canonical_path_value(path: &Path) -> Result<String, VoomError> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        VoomError::Config(format!(
            "scan path is not valid UTF-8 and cannot be stored losslessly: {}",
            path.display()
        ))
    })
}

#[cfg(test)]
#[path = "persist_test.rs"]
mod tests;
