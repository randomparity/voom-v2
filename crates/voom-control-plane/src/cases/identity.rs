//! Identity-layer use cases. Each method composes one or more
//! `IdentityRepo` `_in_tx` writes with the matching event appends in
//! the same transaction so a successful return means both the durable
//! row and its event have committed.

use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use voom_core::{EvidenceId, FileVersionId, VoomError, WorkerId};
use voom_events::payload::{
    FileAssetCreatedPayload, FileLocationAliasedPayload, FileLocationRecordedByMovePayload,
    FileLocationRecordedPayload, FileLocationRetiredByMovePayload, FileVersionCreatedPayload,
    IdentityEvidenceAcceptedPayload, IdentityEvidenceRecordedPayload,
    IdentityEvidenceSupersededPayload, MediaSnapshotRecordedPayload, MediaVariantCreatedPayload,
    MediaWorkCreatedPayload, UseLeaseReanchoredByMovePayload,
};
use voom_events::{Event, SubjectType};
use voom_store::repo::identity::{
    AcceptedPin, AliasProof, DiscoveredFile, FileAsset, FileVersion, IdentityEvidence,
    IdentityEvidenceTarget, IdentityRepo, IngestOutcome, MediaSnapshot, MediaVariant, MediaWork,
    NewFileLocation, NewFileVersion, NewIdentityEvidence, NewMediaSnapshot, NewMediaVariant,
    NewMediaWork, ObservedBytes, RenameProof, RenameReconciledOutcome,
};
use voom_store::repo::use_leases::UseLeaseRepo;

use crate::ControlPlane;

use super::{append_event, begin_tx, commit_tx};

impl ControlPlane {
    /// Watcher-side ingest entry point. Composes
    /// `IdentityRepo::record_discovered_file_in_tx` with the matching
    /// `file_asset.created` / `file_version.created` /
    /// `file_location.recorded` / `file_location.aliased` events plus
    /// any auto-recorded `identity_evidence.recorded` rows.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    #[expect(
        clippy::too_many_lines,
        reason = "the NewFileAsset / AliasAttached event branches read cleanest inline; \
                  splitting them would scatter the spec §8.7 event chain across helpers"
    )]
    pub async fn record_discovered_file(
        &self,
        discovered: DiscoveredFile,
        alias_proof: Option<AliasProof>,
    ) -> Result<IngestOutcome, VoomError> {
        let observed_at = discovered.observed_at;
        let mut tx = begin_tx(&self.pool).await?;
        let outcome = self
            .identity
            .record_discovered_file_in_tx(&mut tx, discovered, alias_proof)
            .await?;
        match &outcome {
            IngestOutcome::NewFileAsset {
                file_asset_id,
                file_version_id,
                file_location_id,
                hash_match_evidence,
                path_rule_evidence,
            } => {
                append_event(
                    &self.events,
                    &mut tx,
                    SubjectType::FileAsset,
                    Some(file_asset_id.0),
                    observed_at,
                    Event::FileAssetCreated(FileAssetCreatedPayload {
                        file_asset_id: file_asset_id.0,
                    }),
                )
                .await?;
                let version = self
                    .identity
                    .get_file_version_in_tx(&mut tx, *file_version_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "record_discovered_file: file_version {file_version_id} vanished",
                        ))
                    })?;
                append_event(
                    &self.events,
                    &mut tx,
                    SubjectType::FileVersion,
                    Some(version.id.0),
                    observed_at,
                    Event::FileVersionCreated(FileVersionCreatedPayload {
                        file_version_id: version.id.0,
                        file_asset_id: version.file_asset_id.0,
                        content_hash: version.content_hash,
                        size_bytes: version.size_bytes,
                        produced_by: version.produced_by.as_str().to_owned(),
                        produced_from_version_id: version.produced_from_version_id.map(|v| v.0),
                    }),
                )
                .await?;
                let location = self
                    .identity
                    .get_file_location_in_tx(&mut tx, *file_location_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "record_discovered_file: file_location {file_location_id} vanished",
                        ))
                    })?;
                append_event(
                    &self.events,
                    &mut tx,
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
                // Both evidence kinds can fire on the same call (per
                // spec §8.7: an alias-proof mismatch produces a
                // path_rule_match row, and if the hash also matches an
                // existing version, a separate hash_match row is
                // written against that prior asset). Emit one event
                // per Some, in insertion order so the events table
                // mirrors the repo write order.
                for ev_id in [hash_match_evidence, path_rule_evidence]
                    .into_iter()
                    .flatten()
                {
                    let e = self
                        .identity
                        .get_identity_evidence_in_tx(&mut tx, *ev_id)
                        .await?
                        .ok_or_else(|| {
                            VoomError::Internal(format!(
                                "record_discovered_file: evidence {ev_id} vanished"
                            ))
                        })?;
                    append_event(
                        &self.events,
                        &mut tx,
                        SubjectType::IdentityEvidence,
                        Some(e.id.0),
                        e.observed_at,
                        Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
                            evidence_id: e.id.0,
                            target_type: e.target_type.as_str().to_owned(),
                            target_id: e.target_id,
                            assertion_type: e.assertion_type.as_str().to_owned(),
                            provider: e.provider,
                            provider_version: e.provider_version,
                            confidence: e.confidence,
                            observed_at: e.observed_at,
                        }),
                    )
                    .await?;
                }
            }
            IngestOutcome::AliasAttached {
                file_version_id,
                new_file_location_id,
            } => {
                let location = self
                    .identity
                    .get_file_location_in_tx(&mut tx, *new_file_location_id)
                    .await?
                    .ok_or_else(|| {
                        VoomError::Internal(format!(
                            "record_discovered_file: alias location {new_file_location_id} vanished",
                        ))
                    })?;
                append_event(
                    &self.events,
                    &mut tx,
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
            }
        }
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Reconcile a same-physical-object rename. Composes
    /// `IdentityRepo::reconcile_rename_in_tx` with the matching
    /// `file_location.retired_by_move`, `file_location.recorded_by_move`,
    /// and `identity_evidence.recorded` (`path_rule_match`) events.
    /// Any live `Location`-scoped use leases on the retired location
    /// are re-anchored in the same transaction and each emits a
    /// `use_lease.reanchored_by_move` event (sprint-1 design §9.2).
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    #[expect(
        clippy::too_many_lines,
        reason = "rename + evidence + reanchor compose one atomic §9.2 sequence; \
                  splitting the helper would scatter the event chain"
    )]
    pub async fn reconcile_rename(
        &self,
        proof: RenameProof,
        observed: ObservedBytes,
        observed_at: OffsetDateTime,
    ) -> Result<RenameReconciledOutcome, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let outcome = self
            .identity
            .reconcile_rename_in_tx(&mut tx, proof, observed, observed_at)
            .await?;
        let new_location = self
            .identity
            .get_file_location_in_tx(&mut tx, outcome.new_file_location_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "reconcile_rename: new location {} vanished",
                    outcome.new_file_location_id
                ))
            })?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::FileLocation,
            Some(outcome.retired_location_id.0),
            observed_at,
            Event::FileLocationRetiredByMove(FileLocationRetiredByMovePayload {
                file_location_id: outcome.retired_location_id.0,
                file_version_id: outcome.file_version_id.0,
                retired_at: observed_at,
            }),
        )
        .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::FileLocation,
            Some(new_location.id.0),
            observed_at,
            Event::FileLocationRecordedByMove(FileLocationRecordedByMovePayload {
                retired_file_location_id: outcome.retired_location_id.0,
                new_file_location_id: new_location.id.0,
                file_version_id: outcome.file_version_id.0,
                kind: new_location.kind.as_str().to_owned(),
                value: new_location.value,
                observed_at,
            }),
        )
        .await?;
        // The repo already wrote one path_rule_match evidence row;
        // emit its identity_evidence.recorded event.
        let evidence_rows = self
            .identity
            .list_identity_evidence_by_target_in_tx(
                &mut tx,
                IdentityEvidenceTarget::FileLocation,
                new_location.id.0,
            )
            .await?;
        for e in &evidence_rows {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::IdentityEvidence,
                Some(e.id.0),
                e.observed_at,
                Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
                    evidence_id: e.id.0,
                    target_type: e.target_type.as_str().to_owned(),
                    target_id: e.target_id,
                    assertion_type: e.assertion_type.as_str().to_owned(),
                    provider: e.provider.clone(),
                    provider_version: e.provider_version.clone(),
                    confidence: e.confidence,
                    observed_at: e.observed_at,
                }),
            )
            .await?;
        }
        // Per sprint-1 design §9.2: any live `Location`-scoped use leases
        // attached to the retired `FileLocation` must be re-anchored to
        // its replacement inside the same transaction as the rename, so
        // a destructive commit targeting the new location still sees the
        // protecting lease.
        let reanchor = self
            .use_leases
            .reanchor_on_move_in_tx(
                &mut tx,
                outcome.retired_location_id,
                outcome.new_file_location_id,
                observed_at,
            )
            .await?;
        for &lease_id in &reanchor.reanchored {
            append_event(
                &self.events,
                &mut tx,
                SubjectType::AssetUseLease,
                Some(lease_id.0),
                observed_at,
                Event::UseLeaseReanchoredByMove(UseLeaseReanchoredByMovePayload {
                    lease_id: lease_id.0,
                    retired_location_id: outcome.retired_location_id.0,
                    new_location_id: outcome.new_file_location_id.0,
                    reanchored_at: observed_at,
                }),
            )
            .await?;
        }
        commit_tx(tx).await?;
        Ok(outcome)
    }

    /// Stamp a `terminal` acceptance on an existing `identity_evidence`
    /// row. Emits `identity_evidence.accepted`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn accept_identity_evidence(
        &self,
        id: EvidenceId,
        actor: Option<String>,
        accepted_at: OffsetDateTime,
        pinned: AcceptedPin,
    ) -> Result<IdentityEvidence, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let updated = self
            .identity
            .accept_identity_evidence_in_tx(&mut tx, id, actor.clone(), accepted_at, pinned)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::IdentityEvidence,
            Some(updated.id.0),
            accepted_at,
            Event::IdentityEvidenceAccepted(IdentityEvidenceAcceptedPayload {
                evidence_id: updated.id.0,
                target_type: updated.target_type.as_str().to_owned(),
                target_id: updated.target_id,
                accepted_user_id: updated.accepted_user_id.clone(),
                accepted_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(updated)
    }

    /// Supersede an existing evidence row with a new one.
    /// Emits `identity_evidence.recorded` (for the new row) and
    /// `identity_evidence.superseded` (for the old row).
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn supersede_identity_evidence(
        &self,
        old_id: EvidenceId,
        new_input: NewIdentityEvidence,
        superseded_at: OffsetDateTime,
    ) -> Result<IdentityEvidence, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let new = self
            .identity
            .supersede_identity_evidence_in_tx(&mut tx, old_id, new_input, superseded_at)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::IdentityEvidence,
            Some(new.id.0),
            new.observed_at,
            Event::IdentityEvidenceRecorded(IdentityEvidenceRecordedPayload {
                evidence_id: new.id.0,
                target_type: new.target_type.as_str().to_owned(),
                target_id: new.target_id,
                assertion_type: new.assertion_type.as_str().to_owned(),
                provider: new.provider.clone(),
                provider_version: new.provider_version.clone(),
                confidence: new.confidence,
                observed_at: new.observed_at,
            }),
        )
        .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::IdentityEvidence,
            Some(old_id.0),
            superseded_at,
            Event::IdentityEvidenceSuperseded(IdentityEvidenceSupersededPayload {
                superseded_evidence_id: old_id.0,
                superseded_by_evidence_id: new.id.0,
                target_type: new.target_type.as_str().to_owned(),
                target_id: new.target_id,
                superseded_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(new)
    }

    /// Record a media snapshot. Emits `media_snapshot.recorded`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn record_media_snapshot(
        &self,
        file_version_id: FileVersionId,
        probed_by: Option<WorkerId>,
        payload: JsonValue,
        probed_at: OffsetDateTime,
    ) -> Result<MediaSnapshot, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let snap = self
            .identity
            .record_media_snapshot_in_tx(
                &mut tx,
                NewMediaSnapshot {
                    file_version_id,
                    probed_by,
                    probed_at,
                    payload,
                },
            )
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::MediaSnapshot,
            Some(snap.id.0),
            probed_at,
            Event::MediaSnapshotRecorded(MediaSnapshotRecordedPayload {
                media_snapshot_id: snap.id.0,
                file_version_id: snap.file_version_id.0,
                probed_by_worker_id: snap.probed_by.map(|w| w.0),
                probed_at,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(snap)
    }

    /// Create a `MediaWork`. Emits `media_work.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_media_work(&self, input: NewMediaWork) -> Result<MediaWork, VoomError> {
        let created_at = input.created_at;
        let mut tx = begin_tx(&self.pool).await?;
        let mw = self
            .identity
            .create_media_work_in_tx(&mut tx, input)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::MediaWork,
            Some(mw.id.0),
            created_at,
            Event::MediaWorkCreated(MediaWorkCreatedPayload {
                media_work_id: mw.id.0,
                kind: mw.kind.as_str().to_owned(),
                display_title: mw.display_title.clone(),
                provisional: mw.provisional,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(mw)
    }

    /// Create a `MediaVariant`. Emits `media_variant.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_media_variant(
        &self,
        input: NewMediaVariant,
    ) -> Result<MediaVariant, VoomError> {
        let created_at = input.created_at;
        let mut tx = begin_tx(&self.pool).await?;
        let mv = self
            .identity
            .create_media_variant_in_tx(&mut tx, input)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::MediaVariant,
            Some(mv.id.0),
            created_at,
            Event::MediaVariantCreated(MediaVariantCreatedPayload {
                media_variant_id: mv.id.0,
                media_work_id: mv.media_work_id.0,
                label: mv.label.clone(),
                provisional: mv.provisional,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(mv)
    }

    /// Create a `FileAsset` directly (typically tests; production flow
    /// goes through `record_discovered_file`). Emits `file_asset.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_file_asset(
        &self,
        created_at: OffsetDateTime,
    ) -> Result<FileAsset, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let asset = self
            .identity
            .create_file_asset_in_tx(&mut tx, created_at)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::FileAsset,
            Some(asset.id.0),
            created_at,
            Event::FileAssetCreated(FileAssetCreatedPayload {
                file_asset_id: asset.id.0,
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(asset)
    }

    /// Create a `FileVersion`. Emits `file_version.created`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_file_version(
        &self,
        input: NewFileVersion,
    ) -> Result<FileVersion, VoomError> {
        let observed_at = input.created_at;
        let mut tx = begin_tx(&self.pool).await?;
        let v = self
            .identity
            .create_file_version_in_tx(&mut tx, input)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::FileVersion,
            Some(v.id.0),
            observed_at,
            Event::FileVersionCreated(FileVersionCreatedPayload {
                file_version_id: v.id.0,
                file_asset_id: v.file_asset_id.0,
                content_hash: v.content_hash.clone(),
                size_bytes: v.size_bytes,
                produced_by: v.produced_by.as_str().to_owned(),
                produced_from_version_id: v.produced_from_version_id.map(|p| p.0),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(v)
    }

    /// Create a `FileLocation`. Emits `file_location.recorded`.
    ///
    /// # Errors
    /// Propagates repo and event-append errors.
    pub async fn create_file_location(
        &self,
        input: NewFileLocation,
    ) -> Result<voom_store::repo::identity::FileLocation, VoomError> {
        let observed_at = input.observed_at;
        let mut tx = begin_tx(&self.pool).await?;
        let loc = self
            .identity
            .create_file_location_in_tx(&mut tx, input)
            .await?;
        append_event(
            &self.events,
            &mut tx,
            SubjectType::FileLocation,
            Some(loc.id.0),
            observed_at,
            Event::FileLocationRecorded(FileLocationRecordedPayload {
                file_location_id: loc.id.0,
                file_version_id: loc.file_version_id.0,
                kind: loc.kind.as_str().to_owned(),
                value: loc.value.clone(),
            }),
        )
        .await?;
        commit_tx(tx).await?;
        Ok(loc)
    }
}

#[cfg(test)]
#[path = "identity_test.rs"]
mod tests;
