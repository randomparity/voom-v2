//! Completion publication from agreed evidence (ADR 0077).
//!
//! Relocated DB-only logic from the transitional control-plane persist path:
//! same-address replay, hardlink attach by `(dev, ino)` facts, fresh ingest
//! with events and media snapshots, sidecar bundle membership, and inode scan
//! facts. Inputs are decoded `ScanObservationEvidence` payloads — never bytes,
//! never filesystem paths. Runs inside the completion transaction before
//! location retirement.

use std::path::Path;

use serde_json::Value;
use sqlx::Sqlite;
use time::OffsetDateTime;
use voom_core::{
    BundleId, FileAssetId, FileLocationId, FileVersionId, ProviderRelativeLocator,
    ScanObservationEvidence, ScanSessionId, ScanSidecarEvidence, StorageRootId, VoomError,
};
use voom_events::payload::{
    AssetBundleMemberAddedPayload, FileAssetCreatedPayload, FileLocationRootedAliasedPayload,
    FileLocationRootedRecordedPayload, FileVersionCreatedPayload, IdentityEvidenceRecordedPayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::media::bundles::{BundleMemberRole, NewBundleMember};
use voom_store::repo::media::identity::{
    DiscoveredFile, FileLocationRepo, FileVersionRepo, IdentityEvidenceRepo, IngestOutcome,
    IngestRepo, NewMediaSnapshot,
};
use voom_store::repo::media::scan_facts::{
    find_live_hardlink_location_in_tx, find_live_scanned_address_in_tx, record_scan_fact_in_tx,
};
use voom_store::repo::scan::sessions::{ScanObservation, SqliteScanSessionRepo};

use crate::ControlPlane;
use crate::cases::append_event;

/// What one completion published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PublicationSummary {
    pub published: u64,
    pub hardlinked: u64,
}

/// Publish identity for every evidence-bearing observation of a session.
///
/// Evidence-less observations record existence only: they protect their locator
/// from retirement but publish nothing here.
pub(super) async fn publish_session_evidence_in_tx(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    scan_session_id: ScanSessionId,
    storage_root_id: StorageRootId,
    now: OffsetDateTime,
) -> Result<PublicationSummary, VoomError> {
    let observations =
        SqliteScanSessionRepo::session_observations_in_tx(tx, scan_session_id).await?;
    let mut summary = PublicationSummary {
        published: 0,
        hardlinked: 0,
    };
    for observation in observations {
        let Some(evidence) = observation.evidence.clone() else {
            continue;
        };
        let hardlink = publish_one(
            control_plane,
            tx,
            &observation,
            &evidence,
            storage_root_id,
            now,
        )
        .await?;
        summary.published += 1;
        if hardlink {
            summary.hardlinked += 1;
        }
    }
    Ok(summary)
}

/// The identity facts publication resolves on: exactly the agreed evidence set.
struct PublishedFacts {
    content_hash: String,
    size_bytes: u64,
    dev: Option<u64>,
    ino: Option<u64>,
    nlink: Option<u64>,
}

impl PublishedFacts {
    fn from_evidence(evidence: &ScanObservationEvidence) -> Self {
        Self {
            content_hash: evidence.content_hash.clone(),
            size_bytes: evidence.size_bytes,
            dev: evidence.file_key.as_ref().map(|key| key.dev),
            ino: evidence.file_key.as_ref().map(|key| key.ino),
            nlink: evidence.file_key.as_ref().map(|key| key.nlink),
        }
    }
}

/// Publish one observation. Returns whether it attached as a hardlink.
async fn publish_one(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    observation: &ScanObservation,
    evidence: &ScanObservationEvidence,
    storage_root_id: StorageRootId,
    now: OffsetDateTime,
) -> Result<bool, VoomError> {
    let candidate = PublishedFacts::from_evidence(evidence);
    let provider_relative_locator = observation.provider_relative_locator.clone();
    // Display-only stand-in for the removed canonical path: bundles name
    // themselves from the primary's file stem (DB string only).
    let display_path = Path::new(provider_relative_locator.as_str()).to_path_buf();

    let resolved =
        match resolve_same_address(tx, &candidate, storage_root_id, &provider_relative_locator)
            .await?
        {
            Some(existing) => existing,
            None => {
                match resolve_hardlink(
                    control_plane,
                    tx,
                    &candidate,
                    storage_root_id,
                    &provider_relative_locator,
                    now,
                )
                .await?
                {
                    Some(hardlink) => hardlink,
                    None => {
                        ingest_new_scanned_file(
                            control_plane,
                            tx,
                            NewScanIdentity {
                                storage_root_id,
                                provider_relative_locator,
                                candidate: &candidate,
                                snapshot_payload: evidence.probe_snapshot.clone(),
                                now,
                            },
                        )
                        .await?
                    }
                }
            }
        };

    persist_bundle_sidecars(
        control_plane,
        tx,
        resolved.file_version_id,
        &display_path,
        storage_root_id,
        sidecars_from_evidence(&evidence.sidecars)?,
        now,
    )
    .await?;

    Ok(resolved.hardlink)
}

/// The evidence sidecar-role vocabulary is exactly the bundle roles a scan may
/// mint; anything else is a config error, never a silent default.
fn role_from_evidence(role: &str) -> Result<BundleMemberRole, VoomError> {
    match role {
        "external_subtitle" => Ok(BundleMemberRole::ExternalSubtitle),
        "nfo" => Ok(BundleMemberRole::Nfo),
        "poster" => Ok(BundleMemberRole::Poster),
        "trailer" => Ok(BundleMemberRole::Trailer),
        other => Err(VoomError::Config(format!(
            "sidecar evidence role {other:?} not in sidecar role vocab"
        ))),
    }
}

/// Map strict sidecar evidence onto the bundle-membership input.
fn sidecars_from_evidence(
    sidecars: &[ScanSidecarEvidence],
) -> Result<Vec<PublishedSidecar>, VoomError> {
    sidecars
        .iter()
        .map(|sidecar| {
            Ok(PublishedSidecar {
                role: role_from_evidence(&sidecar.role)?,
                provider_relative_locator: ProviderRelativeLocator::new(
                    sidecar.provider_relative_locator.clone(),
                )?,
                content_hash: format!("blake3:{}", sidecar.blake3_hex),
                size_bytes: sidecar.size_bytes,
            })
        })
        .collect()
}

struct PublishedSidecar {
    role: BundleMemberRole,
    provider_relative_locator: ProviderRelativeLocator,
    content_hash: String,
    size_bytes: u64,
}

async fn persist_bundle_sidecars(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    file_version_id: FileVersionId,
    display_path: &Path,
    storage_root_id: StorageRootId,
    sidecars: Vec<PublishedSidecar>,
    now: OffsetDateTime,
) -> Result<(), VoomError> {
    if sidecars.is_empty() {
        return Ok(());
    }
    let bundle_id = control_plane
        .resolve_or_create_primary_bundle_in_tx(tx, file_version_id, display_path, now)
        .await?
        .bundle_id;
    for sidecar in sidecars {
        persist_sidecar(control_plane, tx, bundle_id, storage_root_id, sidecar, now).await?;
    }
    Ok(())
}

async fn resolve_same_address(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    candidate: &PublishedFacts,
    storage_root_id: StorageRootId,
    provider_relative_locator: &ProviderRelativeLocator,
) -> Result<Option<ResolvedIdentity>, VoomError> {
    let Some(existing) =
        find_live_scanned_address_in_tx(tx, storage_root_id, provider_relative_locator).await?
    else {
        return Ok(None);
    };
    if existing.content_hash != candidate.content_hash
        || existing.size_bytes != candidate.size_bytes
    {
        return Err(VoomError::Conflict(format!(
            "scan address ({storage_root_id}, {}) already records different bytes",
            provider_relative_locator.as_str()
        )));
    }
    Ok(Some(ResolvedIdentity {
        file_version_id: existing.file_version_id,
        hardlink: false,
    }))
}

/// Resolve to an existing physical file via matching `(dev, ino)` inode facts.
/// A `(dev, ino)` match with different bytes is a recycled inode or an in-place
/// edit, not a hardlink — fall through to fresh ingest rather than aliasing
/// mismatched bytes onto a stale version.
async fn resolve_hardlink(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    candidate: &PublishedFacts,
    storage_root_id: StorageRootId,
    provider_relative_locator: &ProviderRelativeLocator,
    now: OffsetDateTime,
) -> Result<Option<ResolvedIdentity>, VoomError> {
    let (Some(dev), Some(ino)) = (candidate.dev, candidate.ino) else {
        return Ok(None);
    };
    let Some(matched) =
        find_live_hardlink_location_in_tx(tx, dev, ino, storage_root_id, provider_relative_locator)
            .await?
    else {
        return Ok(None);
    };
    if matched.content_hash != candidate.content_hash || matched.size_bytes != candidate.size_bytes
    {
        return Ok(None);
    }
    let new_location_id = control_plane
        .identity
        .attach_local_hardlink_location_in_tx(
            tx,
            matched.file_version_id,
            storage_root_id,
            provider_relative_locator,
            now,
        )
        .await?;
    record_inode_fact(tx, new_location_id, candidate, now).await?;
    append_event(
        &control_plane.events,
        tx,
        SubjectType::FileLocation,
        Some(new_location_id.0),
        now,
        Event::FileLocationRootedAliased(FileLocationRootedAliasedPayload {
            file_location_id: new_location_id,
            file_version_id: matched.file_version_id,
            storage_root_id,
            provider_relative_locator: provider_relative_locator.clone(),
        }),
    )
    .await?;
    Ok(Some(ResolvedIdentity {
        file_version_id: matched.file_version_id,
        hardlink: true,
    }))
}

async fn ingest_new_scanned_file(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    input: NewScanIdentity<'_>,
) -> Result<ResolvedIdentity, VoomError> {
    let NewScanIdentity {
        storage_root_id,
        provider_relative_locator,
        candidate,
        snapshot_payload,
        now,
    } = input;
    let outcome = control_plane
        .identity
        .record_discovered_file_in_tx(
            tx,
            DiscoveredFile {
                storage_root_id,
                provider_relative_locator,
                content_hash: candidate.content_hash.clone(),
                size_bytes: candidate.size_bytes,
                observed_at: now,
                proof: None,
            },
            None,
        )
        .await?;
    let ingested = emit_ingest_events(control_plane, tx, &outcome, now).await?;
    record_inode_fact(tx, ingested.location, candidate, now).await?;
    // The probe ran on the owner node under a worker identity the control plane
    // does not attribute per-file; provenance stays with the session's batches.
    crate::media_snapshot::record_with_event_in_tx(
        control_plane,
        tx,
        NewMediaSnapshot {
            file_version_id: ingested.version,
            probed_by: None,
            probed_at: now,
            payload: normalize_snapshot_stream_ids(snapshot_payload)?,
        },
    )
    .await?;
    Ok(ResolvedIdentity {
        file_version_id: ingested.version,
        hardlink: false,
    })
}

struct ResolvedIdentity {
    file_version_id: FileVersionId,
    hardlink: bool,
}

struct NewScanIdentity<'a> {
    storage_root_id: StorageRootId,
    provider_relative_locator: ProviderRelativeLocator,
    candidate: &'a PublishedFacts,
    snapshot_payload: Value,
    now: OffsetDateTime,
}

async fn record_inode_fact(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    file_location_id: FileLocationId,
    candidate: &PublishedFacts,
    now: OffsetDateTime,
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

/// Ensure every published sidecar is a member of its primary's bundle.
async fn persist_sidecar(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    bundle_id: BundleId,
    storage_root_id: StorageRootId,
    sidecar: PublishedSidecar,
    observed_at: OffsetDateTime,
) -> Result<(), VoomError> {
    if let Some(existing) =
        find_live_scanned_address_in_tx(tx, storage_root_id, &sidecar.provider_relative_locator)
            .await?
    {
        if existing.content_hash != sidecar.content_hash
            || existing.size_bytes != sidecar.size_bytes
        {
            return Err(VoomError::Conflict(format!(
                "scan sidecar address ({storage_root_id}, {}) already records different bytes",
                sidecar.provider_relative_locator.as_str()
            )));
        }
        return require_sidecar_membership(
            control_plane,
            tx,
            bundle_id,
            &sidecar,
            existing.file_asset_id,
            observed_at,
        )
        .await;
    }
    let outcome = control_plane
        .identity
        .record_discovered_file_in_tx(
            tx,
            DiscoveredFile {
                storage_root_id,
                provider_relative_locator: sidecar.provider_relative_locator.clone(),
                content_hash: sidecar.content_hash.clone(),
                size_bytes: sidecar.size_bytes,
                observed_at,
                proof: None,
            },
            None,
        )
        .await?;
    let ingested = emit_ingest_events(control_plane, tx, &outcome, observed_at).await?;
    require_sidecar_membership(
        control_plane,
        tx,
        bundle_id,
        &sidecar,
        ingested.asset,
        observed_at,
    )
    .await
}

async fn require_sidecar_membership(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    bundle_id: BundleId,
    sidecar: &PublishedSidecar,
    file_asset_id: FileAssetId,
    observed_at: OffsetDateTime,
) -> Result<(), VoomError> {
    let role = sidecar.role;
    if let Some(member) = control_plane
        .bundles
        .get_member_by_file_asset_in_tx(tx, file_asset_id)
        .await?
    {
        if member.bundle_id == bundle_id && member.role == role {
            return Ok(());
        }
        return Err(VoomError::Conflict(format!(
            "scan sidecar asset {file_asset_id} is already in bundle {} as {:?}",
            member.bundle_id, member.role
        )));
    }

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
            bundle_id,
            file_asset_id,
            role: role.as_str().to_owned(),
        }),
    )
    .await
}

#[expect(
    clippy::too_many_lines,
    reason = "mirrors the existing identity use-case event chain for one atomic scan transaction"
)]
async fn emit_ingest_events(
    control_plane: &ControlPlane,
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    outcome: &IngestOutcome,
    observed_at: OffsetDateTime,
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
                    file_asset_id: *file_asset_id,
                }),
            )
            .await?;
            let version = control_plane
                .identity
                .get_file_version_in_tx(tx, *file_version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan publication: file_version {file_version_id} vanished"
                    ))
                })?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileVersion,
                Some(version.id.0),
                observed_at,
                Event::FileVersionCreated(FileVersionCreatedPayload {
                    file_version_id: version.id,
                    file_asset_id: version.file_asset_id,
                    content_hash: version.content_hash.clone(),
                    size_bytes: version.size_bytes,
                    produced_by: version.produced_by.as_str().to_owned(),
                    produced_from_version_id: version.produced_from_version_id,
                }),
            )
            .await?;
            let location = control_plane
                .identity
                .get_file_location_in_tx(tx, *file_location_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan publication: file_location {file_location_id} vanished"
                    ))
                })?;
            let (storage_root_id, provider_relative_locator) = location.rooted_address()?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileLocation,
                Some(location.id.0),
                observed_at,
                Event::FileLocationRootedRecorded(FileLocationRootedRecordedPayload {
                    file_location_id: location.id,
                    file_version_id: location.file_version_id,
                    storage_root_id,
                    provider_relative_locator: provider_relative_locator.clone(),
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
                        VoomError::Internal(format!("scan publication: evidence {ev_id} vanished"))
                    })?;
                append_event(
                    &control_plane.events,
                    tx,
                    SubjectType::IdentityEvidence,
                    Some(evidence.id.0),
                    evidence.observed_at,
                    Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
                        evidence_id: evidence.id,
                        target_type: evidence.target_type.as_str().to_owned(),
                        target_id: evidence.target_id,
                        assertion_type: evidence.assertion_type,
                        provider: evidence.provider,
                        provider_version: evidence.provider_version,
                        confidence: evidence.confidence,
                        observed_at: evidence.observed_at,
                    }),
                )
                .await?;
            }
            Ok(IngestedIds {
                asset: *file_asset_id,
                version: *file_version_id,
                location: *file_location_id,
            })
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
                        "scan publication: alias location {new_file_location_id} vanished"
                    ))
                })?;
            let (storage_root_id, provider_relative_locator) = location.rooted_address()?;
            append_event(
                &control_plane.events,
                tx,
                SubjectType::FileLocation,
                Some(location.id.0),
                observed_at,
                Event::FileLocationRootedAliased(FileLocationRootedAliasedPayload {
                    file_location_id: location.id,
                    file_version_id: *file_version_id,
                    storage_root_id,
                    provider_relative_locator: provider_relative_locator.clone(),
                }),
            )
            .await?;
            let version = control_plane
                .identity
                .get_file_version_in_tx(tx, *file_version_id)
                .await?
                .ok_or_else(|| {
                    VoomError::Internal(format!(
                        "scan publication: file_version {file_version_id} vanished"
                    ))
                })?;
            Ok(IngestedIds {
                asset: version.file_asset_id,
                version: *file_version_id,
                location: *new_file_location_id,
            })
        }
    }
}

/// Identity ids one ingest path resolved to.
struct IngestedIds {
    asset: FileAssetId,
    version: FileVersionId,
    location: FileLocationId,
}

/// Guarantee every persisted snapshot stream carries an id, mirroring the
/// transitional persist path's normalization of ffprobe output.
fn normalize_snapshot_stream_ids(snapshot: Value) -> Result<Value, VoomError> {
    let mut normalized = snapshot;
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

#[cfg(test)]
#[path = "publish_test.rs"]
mod tests;
