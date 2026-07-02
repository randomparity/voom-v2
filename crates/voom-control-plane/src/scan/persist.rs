use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::Digest as _;
use voom_core::{
    BundleId, ErrorCode, FailureClass, FileAssetId, FileLocationId, FileVersionId, MediaSnapshotId,
    VoomError, WorkerId,
};
use voom_events::payload::{
    AssetBundleCreatedPayload, AssetBundleMemberAddedPayload, FileAssetCreatedPayload,
    FileLocationAliasedPayload, FileLocationRecordedPayload, FileVersionCreatedPayload,
    IdentityEvidenceRecordedPayload, MediaSnapshotRecordedPayload, MediaVariantCreatedPayload,
    MediaWorkCreatedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::{
    bundles::{BundleMemberRole, NewAssetBundle, NewBundleMember},
    identity::{
        DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome, MediaWorkKind,
        NewMediaSnapshot, NewMediaVariant, NewMediaWork,
    },
    scan_facts::{find_live_hardlink_location_in_tx, record_scan_fact_in_tx},
};
use voom_worker_protocol::ProbeFileResult;

use crate::ControlPlane;
use crate::cases::append_event;
use crate::scan::{
    ScanReportFileStatus,
    discovery::{SidecarCandidate, SidecarKind},
};

pub use super::hash::ObservedFileFacts as ObservedCandidateFacts;

const CONTENT_DRIFT_MESSAGE: &str = "file changed between hashing and probing";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedScan {
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    /// The snapshot recorded for this file's probe. `None` for a hardlink that
    /// resolved to an already-ingested physical file: no new snapshot is
    /// recorded because the existing `file_version` already carries one.
    pub media_snapshot_id: Option<MediaSnapshotId>,
    pub bundle_id: Option<BundleId>,
    pub bundle_member_role: Option<String>,
    pub sidecars: Vec<PersistedSidecar>,
    /// `true` when this path resolved to an existing physical file via matching
    /// `(dev, ino)` inode facts — a new live `file_location` on an existing
    /// `file_version`, not a fresh asset (#249).
    pub hardlink: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedSidecar {
    pub path: PathBuf,
    pub file_asset_id: FileAssetId,
    pub file_version_id: FileVersionId,
    pub file_location_id: FileLocationId,
    pub bundle_id: BundleId,
    pub bundle_member_role: String,
    pub content_hash: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedSidecar {
    path: PathBuf,
    role: BundleMemberRole,
    location_value: String,
    content_hash: String,
    size_bytes: u64,
}

const fn role_for_sidecar_kind(kind: SidecarKind) -> BundleMemberRole {
    match kind {
        SidecarKind::Subtitle => BundleMemberRole::ExternalSubtitle,
        SidecarKind::Nfo => BundleMemberRole::Nfo,
        SidecarKind::Poster => BundleMemberRole::Poster,
        SidecarKind::Trailer => BundleMemberRole::Trailer,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanFileError {
    status: ScanReportFileStatus,
    error_code: ErrorCode,
    failure_class: FailureClass,
    message: String,
}

impl ScanFileError {
    #[must_use]
    pub const fn status(&self) -> ScanReportFileStatus {
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
            status: ScanReportFileStatus::FailedContentDrift,
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
    sidecars: &[SidecarCandidate],
    candidate: &ObservedCandidateFacts,
    result: &ProbeFileResult,
) -> Result<PersistedScan, ScanPersistError> {
    verify_probe_facts(candidate, result)?;
    let snapshot_payload = snapshot_with_stream_ids(&result.snapshot)?;
    let location_value = canonical_path_value(canonical_path)?;
    let observed_sidecars = observe_sidecars(sidecars).await?;

    let now = control_plane.clock().now();
    let mut tx = control_plane
        .pool
        .begin()
        .await
        .map_err(|e| VoomError::database_context("begin", e))?;

    ensure_worker_live_in_tx(&mut tx, worker_id).await?;

    // Resolve identity. A candidate whose (dev, ino) matches a live prior local
    // location at a different path — and whose content matches that version — is
    // a hardlink to an already-ingested physical file (#249): attach the path to
    // the existing version, minting no new asset/version/snapshot. Otherwise
    // ingest a fresh asset. Either way the shared bundle/sidecar block below runs
    // against the resolved `file_asset_id`, so a hardlink path's own sidecars are
    // still attached to the owning bundle.
    let resolved =
        match resolve_hardlink(control_plane, &mut tx, candidate, &location_value, now).await? {
            Some(hardlink) => hardlink,
            None => {
                ingest_new_scanned_file(
                    control_plane,
                    &mut tx,
                    worker_id,
                    location_value,
                    candidate,
                    snapshot_payload,
                    now,
                )
                .await?
            }
        };
    let ResolvedScanIdentity {
        file_asset_id,
        file_version_id,
        file_location_id,
        media_snapshot_id,
        hardlink,
    } = resolved;

    let (bundle_id, bundle_member_role, persisted_sidecars) = if observed_sidecars.is_empty() {
        (None, None, Vec::new())
    } else {
        let bundle_id =
            ensure_primary_bundle(control_plane, &mut tx, file_asset_id, canonical_path, now)
                .await?;
        let mut persisted_sidecars = Vec::with_capacity(observed_sidecars.len());
        for sidecar in observed_sidecars {
            persisted_sidecars
                .push(persist_sidecar(control_plane, &mut tx, bundle_id, sidecar, now).await?);
        }
        (
            Some(bundle_id),
            Some(BundleMemberRole::PrimaryVideo.as_str().to_owned()),
            persisted_sidecars,
        )
    };

    tx.commit()
        .await
        .map_err(|e| VoomError::database_context("commit", e))?;

    Ok(PersistedScan {
        file_asset_id,
        file_version_id,
        file_location_id,
        media_snapshot_id,
        bundle_id,
        bundle_member_role,
        sidecars: persisted_sidecars,
        hardlink,
    })
}

/// The identity a scanned candidate resolved to: a fresh ingest or a hardlink to
/// an existing physical file. Shared by both branches so the bundle/sidecar and
/// report-building logic is written once.
struct ResolvedScanIdentity {
    file_asset_id: FileAssetId,
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
    /// `Some` for a fresh ingest (a snapshot was recorded); `None` for a
    /// hardlink (the existing version already carries one).
    media_snapshot_id: Option<MediaSnapshotId>,
    hardlink: bool,
}

/// Resolve a discovered candidate to an existing physical file via matching
/// `(dev, ino)` inode facts. Returns `Some` when the candidate is a hardlink to
/// an already-ingested file whose content still matches; `None` when there is no
/// inode data, no `(dev, ino)` match at a different path, or a match whose
/// content differs (a recycled inode or an in-place edit — treated as a fresh
/// ingest).
async fn resolve_hardlink(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    candidate: &ObservedCandidateFacts,
    location_value: &str,
    now: time::OffsetDateTime,
) -> Result<Option<ResolvedScanIdentity>, ScanPersistError> {
    let (Some(dev), Some(ino)) = (candidate.dev, candidate.ino) else {
        return Ok(None);
    };
    let Some(matched) = find_live_hardlink_location_in_tx(tx, dev, ino, location_value).await?
    else {
        return Ok(None);
    };
    // Integrity guard: a (dev, ino) match with different bytes is a recycled
    // inode or an in-place edit, not a hardlink — fall through to a fresh
    // ingest rather than aliasing mismatched bytes onto a stale version.
    if matched.content_hash != candidate.content_hash || matched.size_bytes != candidate.size_bytes
    {
        return Ok(None);
    }
    let version = control_plane
        .identity
        .get_file_version_in_tx(tx, matched.file_version_id)
        .await?
        .ok_or_else(|| {
            VoomError::Internal(format!(
                "scan hardlink: file_version {} vanished",
                matched.file_version_id
            ))
        })?;
    let new_location_id = control_plane
        .identity
        .attach_local_hardlink_location_in_tx(tx, matched.file_version_id, location_value, now)
        .await?;
    record_scan_fact(tx, new_location_id, candidate, now).await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::FileLocation,
        Some(new_location_id.0),
        now,
        Event::FileLocationAliased(FileLocationAliasedPayload {
            file_location_id: new_location_id.0,
            file_version_id: matched.file_version_id.0,
            kind: FileLocationKind::LocalPath.as_str().to_owned(),
            value: location_value.to_owned(),
        }),
    )
    .await?;
    Ok(Some(ResolvedScanIdentity {
        file_asset_id: version.file_asset_id,
        file_version_id: matched.file_version_id,
        file_location_id: new_location_id,
        media_snapshot_id: None,
        hardlink: true,
    }))
}

/// Ingest a fresh scanned file: record the discovered file, emit ingest events,
/// record its inode facts, and record + emit its media snapshot.
async fn ingest_new_scanned_file(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    worker_id: WorkerId,
    location_value: String,
    candidate: &ObservedCandidateFacts,
    snapshot_payload: Value,
    now: time::OffsetDateTime,
) -> Result<ResolvedScanIdentity, ScanPersistError> {
    let outcome = control_plane
        .identity
        .record_discovered_file_in_tx(
            tx,
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
        emit_ingest_events(control_plane, tx, &outcome, now).await?;
    record_scan_fact(tx, file_location_id, candidate, now).await?;
    let snapshot = control_plane
        .identity
        .record_media_snapshot_in_tx(
            tx,
            NewMediaSnapshot {
                file_version_id,
                probed_by: Some(worker_id),
                probed_at: now,
                payload: snapshot_payload,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
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
    Ok(ResolvedScanIdentity {
        file_asset_id,
        file_version_id,
        file_location_id,
        media_snapshot_id: Some(snapshot.id),
        hardlink: false,
    })
}

/// Record the candidate's inode facts against the given location, when the
/// platform exposed them.
async fn record_scan_fact(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_location_id: FileLocationId,
    candidate: &ObservedCandidateFacts,
    now: time::OffsetDateTime,
) -> Result<(), VoomError> {
    if let (Some(dev), Some(ino)) = (candidate.dev, candidate.ino) {
        record_scan_fact_in_tx(
            tx,
            file_location_id,
            dev,
            ino,
            candidate.nlink.unwrap_or(0),
            now,
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn snapshot_with_stream_ids(snapshot: &Value) -> Result<Value, VoomError> {
    let mut normalized = snapshot.clone();
    let Some(streams) = normalized.get_mut("streams") else {
        return Ok(normalized);
    };
    let Some(streams) = streams.as_array_mut() else {
        return Ok(normalized);
    };
    for stream in streams {
        let Some(stream) = stream.as_object_mut() else {
            return Err(VoomError::Config(
                "snapshot stream entries must be objects".to_owned(),
            ));
        };
        if stream.contains_key("id") {
            continue;
        }
        let Some(index) = stream.get("index").and_then(Value::as_u64) else {
            return Err(VoomError::Config(
                "snapshot stream without id must include numeric index".to_owned(),
            ));
        };
        stream.insert("id".to_owned(), Value::String(format!("stream-{index}")));
    }
    Ok(normalized)
}

async fn observe_sidecars(
    sidecars: &[SidecarCandidate],
) -> Result<Vec<ObservedSidecar>, VoomError> {
    let mut observed = Vec::with_capacity(sidecars.len());
    for sidecar in sidecars {
        let bytes = tokio::fs::read(&sidecar.path).await.map_err(|err| {
            VoomError::Config(format!("sidecar read {}: {err}", sidecar.path.display()))
        })?;
        let size_bytes = u64::try_from(bytes.len()).map_err(|_| {
            VoomError::Internal(format!("sidecar too large: {}", sidecar.path.display()))
        })?;
        let content_hash = format!("sha256:{:x}", sha2::Sha256::digest(&bytes));
        observed.push(ObservedSidecar {
            path: sidecar.path.clone(),
            role: role_for_sidecar_kind(sidecar.kind),
            location_value: canonical_path_value(&sidecar.path)?,
            content_hash,
            size_bytes,
        });
    }
    Ok(observed)
}

#[expect(
    clippy::too_many_lines,
    reason = "keeps provisional work, variant, bundle, and primary-member event writes in one readable transaction step"
)]
async fn ensure_primary_bundle(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    file_asset_id: FileAssetId,
    canonical_path: &Path,
    observed_at: time::OffsetDateTime,
) -> Result<BundleId, VoomError> {
    if let Some(member) = control_plane
        .bundles
        .get_member_by_file_asset_in_tx(tx, file_asset_id)
        .await?
    {
        if member.role != BundleMemberRole::PrimaryVideo {
            return Err(VoomError::Conflict(format!(
                "scan primary asset {file_asset_id} is already a {:?} bundle member",
                member.role
            )));
        }
        return Ok(member.bundle_id);
    }

    let display_name = display_name_from_path(canonical_path);
    let work = control_plane
        .identity
        .create_media_work_in_tx(
            tx,
            NewMediaWork {
                kind: MediaWorkKind::Unknown,
                display_title: display_name.clone(),
                provisional: true,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::MediaWork,
        Some(work.id.0),
        observed_at,
        Event::MediaWorkCreated(MediaWorkCreatedPayload {
            media_work_id: work.id.0,
            kind: work.kind.as_str().to_owned(),
            display_title: work.display_title.clone(),
            provisional: work.provisional,
        }),
    )
    .await?;

    let variant = control_plane
        .identity
        .create_media_variant_in_tx(
            tx,
            NewMediaVariant {
                media_work_id: work.id,
                label: "scan".to_owned(),
                provisional: true,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::MediaVariant,
        Some(variant.id.0),
        observed_at,
        Event::MediaVariantCreated(MediaVariantCreatedPayload {
            media_variant_id: variant.id.0,
            media_work_id: variant.media_work_id.0,
            label: variant.label.clone(),
            provisional: variant.provisional,
        }),
    )
    .await?;

    let bundle = control_plane
        .bundles
        .create_in_tx(
            tx,
            NewAssetBundle {
                media_variant_id: variant.id,
                display_name,
                created_at: observed_at,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::AssetBundle,
        Some(bundle.id.0),
        observed_at,
        Event::AssetBundleCreated(AssetBundleCreatedPayload {
            bundle_id: bundle.id.0,
            media_variant_id: bundle.media_variant_id.0,
            display_name: bundle.display_name.clone(),
        }),
    )
    .await?;
    add_bundle_member_event(
        control_plane,
        tx,
        bundle.id,
        file_asset_id,
        BundleMemberRole::PrimaryVideo,
        observed_at,
    )
    .await?;
    Ok(bundle.id)
}

async fn persist_sidecar(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    bundle_id: BundleId,
    sidecar: ObservedSidecar,
    observed_at: time::OffsetDateTime,
) -> Result<PersistedSidecar, VoomError> {
    let outcome = control_plane
        .identity
        .record_discovered_file_in_tx(
            tx,
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: sidecar.location_value.clone(),
                content_hash: sidecar.content_hash.clone(),
                size_bytes: sidecar.size_bytes,
                observed_at,
                proof: None,
            },
            None,
        )
        .await?;
    let IngestedIds(file_asset_id, file_version_id, file_location_id) =
        emit_ingest_events(control_plane, tx, &outcome, observed_at).await?;
    let role = sidecar.role;
    if let Some(member) = control_plane
        .bundles
        .get_member_by_file_asset_in_tx(tx, file_asset_id)
        .await?
    {
        if member.bundle_id == bundle_id && member.role == role {
            return Ok(persisted_sidecar_report(
                sidecar,
                file_asset_id,
                file_version_id,
                file_location_id,
                bundle_id,
            ));
        }
        return Err(VoomError::Conflict(format!(
            "scan sidecar asset {file_asset_id} is already in bundle {} as {:?}",
            member.bundle_id, member.role
        )));
    }

    add_bundle_member_event(
        control_plane,
        tx,
        bundle_id,
        file_asset_id,
        role,
        observed_at,
    )
    .await?;
    Ok(persisted_sidecar_report(
        sidecar,
        file_asset_id,
        file_version_id,
        file_location_id,
        bundle_id,
    ))
}

fn persisted_sidecar_report(
    sidecar: ObservedSidecar,
    file_asset_id: FileAssetId,
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
    bundle_id: BundleId,
) -> PersistedSidecar {
    PersistedSidecar {
        path: sidecar.path,
        file_asset_id,
        file_version_id,
        file_location_id,
        bundle_id,
        bundle_member_role: sidecar.role.as_str().to_owned(),
        content_hash: sidecar.content_hash,
        size_bytes: sidecar.size_bytes,
    }
}

async fn add_bundle_member_event(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    bundle_id: BundleId,
    file_asset_id: FileAssetId,
    role: BundleMemberRole,
    observed_at: time::OffsetDateTime,
) -> Result<(), VoomError> {
    control_plane
        .bundles
        .add_member_in_tx(
            tx,
            NewBundleMember {
                bundle_id,
                file_asset_id,
                role,
            },
        )
        .await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::AssetBundle,
        Some(bundle_id.0),
        observed_at,
        Event::AssetBundleMemberAdded(AssetBundleMemberAddedPayload {
            bundle_id: bundle_id.0,
            file_asset_id: file_asset_id.0,
            role: role.as_str().to_owned(),
        }),
    )
    .await
}

fn display_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map_or_else(|| path.display().to_string(), str::to_owned)
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
        .map_err(|e| VoomError::database_context("scan persist worker reload", e))?;
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
