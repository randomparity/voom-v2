use super::{
    ArtifactCommitEvidence, ArtifactCommitRecordId, ArtifactCommitState, ArtifactHandle,
    ArtifactHandleAccessMode, ArtifactHandleId, ArtifactLocation, ArtifactLocationKind,
    ArtifactVerification, ArtifactVerificationEvidence, ArtifactVerificationId,
    ArtifactVerificationStatus, CommittedTicketEvidence, FileAssetId, FileLocationId,
    FileVersionId, JobId, JsonValue, LeaseId, MediaSnapshotId, NewArtifactHandle,
    NewArtifactLocation, OffsetDateTime, PolicyArtifactResolution, PolicyArtifactTarget,
    ResultLeaseEvidence, SqliteArtifactRepo, TicketId, VerifiedTicketEvidence, VoomError,
    checked_sqlite_id,
};
use sqlx::Row;

use super::super::common::{i64_from_u64, parse_iso8601, serialize_json, u64_from_i64};
use super::handles::{row_to_handle, row_to_location};
use super::verification::{SELECT_ARTIFACT_VERIFICATION_COLS, row_to_verification};
use crate::repo::execution::leases::LeaseState;

#[derive(Debug)]
struct PolicyFileVersion {
    id: FileVersionId,
    content_hash: String,
    size_bytes: u64,
}

#[derive(Debug)]
struct PolicyFileLocation {
    id: FileLocationId,
    value: String,
}

const COMMITTED_TICKET_EVIDENCE_SQL: &str = "WITH ticket_results AS ( \
         SELECT t.id, t.job_id, t.payload, t.result AS raw_result, \
                CASE WHEN json_valid(t.result) THEN t.result ELSE '{}' END AS safe_result \
         FROM tickets t WHERE t.id IN (SELECT value FROM json_each(?)) \
           AND t.state = 'succeeded' AND t.result IS NOT NULL \
     ), expanded_results AS ( \
         SELECT t.id, t.job_id, t.payload, t.raw_result, \
                COALESCE(CAST(output.key AS INTEGER), 0) AS result_ordinal, \
                CASE WHEN json_type(t.safe_result, '$.outputs') = 'array' \
                           AND json_array_length(t.safe_result, '$.outputs') > 0 \
                     THEN json_patch(t.safe_result, output.value) ELSE t.safe_result END AS result \
         FROM ticket_results t LEFT JOIN json_each(t.safe_result, '$.outputs') AS output \
           ON json_type(t.safe_result, '$.outputs') = 'array' \
          AND json_array_length(t.safe_result, '$.outputs') > 0 \
     ) SELECT t.id AS ticket_id, t.job_id AS ticket_job_id, \
              t.payload AS ticket_payload, t.raw_result, t.result, \
              c.id AS commit_id, c.artifact_handle_id AS commit_artifact_handle_id, \
              c.source_file_version_id AS commit_source_file_version_id, \
              c.verification_id AS commit_verification_id, \
              c.result_file_version_id AS commit_result_file_version_id, \
              c.result_file_location_id AS commit_result_file_location_id, \
              c.state AS commit_state, c.report AS commit_report, \
              c.started_at AS commit_started_at, \
              c.promotion_started_at AS commit_promotion_started_at, \
              c.finished_at AS commit_finished_at, \
              v.artifact_handle_id AS verification_artifact_handle_id, \
              v.workflow_ticket_id AS verification_ticket_id, \
              v.workflow_lease_id AS verification_lease_id, \
              v.status AS verification_status, v.report AS verification_report, \
              v.started_at AS verification_started_at, \
              v.finished_at AS verification_finished_at, \
              rl.ticket_id AS result_lease_ticket_id, rl.state AS result_lease_state, \
              rl.acquired_at AS result_lease_acquired_at, \
              rl.expires_at AS result_lease_expires_at, \
              rl.last_heartbeat_at AS result_lease_last_heartbeat_at, \
              rl.released_at AS result_lease_released_at, \
              sfv.file_asset_id AS source_file_asset_id, \
              fv.file_asset_id AS result_file_asset_id, \
              fl.file_version_id AS location_file_version_id, \
              ms.file_version_id AS snapshot_file_version_id \
       FROM expanded_results t \
       LEFT JOIN artifact_commit_records c \
         ON c.id = json_extract(t.result, '$.commit_record_id') \
       LEFT JOIN artifact_verifications v ON v.id = c.verification_id \
       LEFT JOIN leases rl ON rl.id = json_extract(t.result, '$.lease_id') \
       LEFT JOIN file_versions sfv \
         ON sfv.id = json_extract(t.result, '$.source_file_version_id') \
       LEFT JOIN file_versions fv \
         ON fv.id = json_extract(t.result, '$.result_file_version_id') \
       LEFT JOIN file_locations fl \
         ON fl.id = json_extract(t.result, '$.result_file_location_id') \
       LEFT JOIN media_snapshots ms \
         ON ms.id = json_extract(t.result, '$.result_media_snapshot_id') \
       ORDER BY t.id, t.result_ordinal";

impl SqliteArtifactRepo {
    pub async fn committed_ticket_evidence(
        &self,
        ticket_ids: &[TicketId],
    ) -> Result<Vec<CommittedTicketEvidence>, VoomError> {
        if ticket_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = ticket_ids
            .iter()
            .map(|id| checked_sqlite_id(id.0, "committed evidence ticket id"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids = serialize_json(&ids, "committed evidence ticket ids")?;
        let rows = sqlx::query(COMMITTED_TICKET_EVIDENCE_SQL)
            .bind(ids)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("committed ticket evidence", error))?;
        rows.iter().map(decode_committed_ticket_evidence).collect()
    }

    pub async fn verified_ticket_evidence(
        &self,
        _ticket_id: TicketId,
        verification_id: ArtifactVerificationId,
        _handle_id: ArtifactHandleId,
        location_id: FileLocationId,
    ) -> Result<Option<VerifiedTicketEvidence>, VoomError> {
        let sql = format!(
            "{SELECT_ARTIFACT_VERIFICATION_COLS}, \
             fl.file_version_id AS selected_file_version_id, \
             fl.value AS selected_location_value, \
             l.ticket_id AS selected_lease_ticket_id \
             FROM artifact_verifications v \
             LEFT JOIN leases l ON l.id = v.workflow_lease_id \
             LEFT JOIN file_locations fl ON fl.id = ? AND fl.retired_at IS NULL \
             WHERE v.id = ?"
        );
        let row = sqlx::query(&sql)
            .bind(checked_sqlite_id(
                location_id.0,
                "verified file location id",
            )?)
            .bind(checked_sqlite_id(
                verification_id.0,
                "verified artifact verification id",
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| VoomError::database_context("verified ticket evidence", error))?;
        row.as_ref()
            .map(decode_verified_ticket_evidence)
            .transpose()
    }
}

impl SqliteArtifactRepo {
    pub async fn resolve_policy_artifact_target_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        file_version_id: FileVersionId,
        file_location_id: Option<FileLocationId>,
        now: OffsetDateTime,
    ) -> Result<PolicyArtifactResolution, VoomError> {
        let version = require_active_policy_file_version(tx, file_version_id).await?;
        let location = select_policy_file_location(tx, file_version_id, file_location_id).await?;
        let media_snapshot_id = latest_policy_media_snapshot(tx, file_version_id).await?;
        let (handle, created_handle) = self
            .resolve_policy_artifact_handle(tx, &version, &location, now)
            .await?;
        let (artifact_location, created_location) = self
            .resolve_policy_artifact_location(tx, handle.id, &location, now)
            .await?;

        Ok(PolicyArtifactResolution {
            target: PolicyArtifactTarget {
                artifact_handle_id: handle.id,
                artifact_location_id: artifact_location.id,
                file_version_id,
                file_location_id: location.id,
                media_snapshot_id,
                path: location.value,
                size_bytes: version.size_bytes,
                checksum: version.content_hash,
            },
            created_handle,
            created_location,
        })
    }

    async fn resolve_policy_artifact_handle(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        version: &PolicyFileVersion,
        location: &PolicyFileLocation,
        now: OffsetDateTime,
    ) -> Result<(ArtifactHandle, Option<ArtifactHandle>), VoomError> {
        let committed = policy_committed_handles(tx, version.id).await?;
        match committed.as_slice() {
            [] => {}
            [handle] => {
                require_policy_handle_content_facts(tx, handle.id, version).await?;
                return Ok((handle.clone(), None));
            }
            _ => {
                return Err(VoomError::Conflict(format!(
                    "file_version {} has {} committed artifact handles",
                    version.id,
                    committed.len()
                )));
            }
        }
        let handle = policy_canonical_handle(tx, version.id).await?;
        if let Some(handle) = handle {
            require_policy_handle_facts(tx, handle.id, version).await?;
            return Ok((handle, None));
        }

        let handle = self
            .create_handle_in_tx(
                tx,
                NewArtifactHandle {
                    size_bytes: Some(i64_from_u64(
                        version.size_bytes,
                        concat!(module_path!(), ": ", stringify!(version.size_bytes)),
                    )?),
                    checksum: Some(version.content_hash.clone()),
                    privacy_class: "internal".to_owned(),
                    durability_class: "active".to_owned(),
                    allowed_access_modes: vec![ArtifactHandleAccessMode::LocalPath],
                    mutability: "immutable".to_owned(),
                    source_lineage: Some(serde_json::json!({
                        "kind": "policy_verification",
                        "file_version_id": version.id.0,
                        "file_location_id": location.id.0,
                    })),
                    file_version_id: Some(version.id),
                    created_at: now,
                },
            )
            .await?;
        Ok((handle.clone(), Some(handle)))
    }

    async fn resolve_policy_artifact_location(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
        location: &PolicyFileLocation,
        now: OffsetDateTime,
    ) -> Result<(ArtifactLocation, Option<ArtifactLocation>), VoomError> {
        let rows = sqlx::query(
            "SELECT id, artifact_handle_id, kind, value, observed_at, retired_at \
             FROM artifact_locations \
             WHERE artifact_handle_id = ? AND kind = 'local_path' \
               AND value = ? AND retired_at IS NULL \
             ORDER BY id",
        )
        .bind(i64_from_u64(
            handle_id.0,
            concat!(module_path!(), ": ", stringify!(handle_id.0)),
        )?)
        .bind(&location.value)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy artifact location lookup", err))?;
        match rows.as_slice() {
            [] => {
                let created = self
                    .record_location_in_tx(
                        tx,
                        NewArtifactLocation {
                            artifact_handle_id: handle_id,
                            kind: ArtifactLocationKind::LocalPath,
                            value: location.value.clone(),
                            observed_at: now,
                        },
                    )
                    .await?;
                Ok((created.clone(), Some(created)))
            }
            [row] => Ok((row_to_location(row)?, None)),
            _ => Err(VoomError::Conflict(format!(
                "artifact_handle {handle_id} has {} live local_path locations for {:?}",
                rows.len(),
                location.value
            ))),
        }
    }
}

async fn require_active_policy_file_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: FileVersionId,
) -> Result<PolicyFileVersion, VoomError> {
    let row: Option<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT v.id, v.content_hash, v.size_bytes, \
                (SELECT MAX(current.id) FROM file_versions current \
                 WHERE current.file_asset_id = v.file_asset_id \
                   AND current.retired_at IS NULL) \
         FROM file_versions v \
         WHERE v.id = ? AND v.retired_at IS NULL",
    )
    .bind(i64_from_u64(
        id.0,
        concat!(module_path!(), ": ", stringify!(id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy file version lookup", err))?;
    let Some((row_id, content_hash, size_bytes, active_id)) = row else {
        return Err(VoomError::NotFound(format!(
            "active file_version {id} missing"
        )));
    };
    let row_id = u64_from_i64(row_id, "file_versions.id")?;
    let active_id = u64_from_i64(active_id, "file_versions.active_id")?;
    if row_id != active_id {
        return Err(VoomError::Conflict(format!(
            "file_version {id} was superseded by {}",
            FileVersionId(active_id)
        )));
    }
    Ok(PolicyFileVersion {
        id,
        content_hash,
        size_bytes: u64_from_i64(
            size_bytes,
            concat!(module_path!(), ": ", stringify!(size_bytes)),
        )?,
    })
}

async fn select_policy_file_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
    selected_id: Option<FileLocationId>,
) -> Result<PolicyFileLocation, VoomError> {
    let mut rows: Vec<(i64, i64, String, String)> = if let Some(location_id) = selected_id {
        sqlx::query_as(
            "SELECT id, file_version_id, kind, value \
             FROM file_locations \
             WHERE id = ? AND retired_at IS NULL",
        )
        .bind(i64_from_u64(
            location_id.0,
            concat!(module_path!(), ": ", stringify!(location_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy file location lookup", err))?
    } else {
        sqlx::query_as(
            "SELECT id, file_version_id, kind, value \
             FROM file_locations \
             WHERE file_version_id = ? AND kind = 'local_path' \
               AND retired_at IS NULL ORDER BY id",
        )
        .bind(i64_from_u64(
            version_id.0,
            concat!(module_path!(), ": ", stringify!(version_id.0)),
        )?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|err| VoomError::database_context("policy local path lookup", err))?
    };
    let [row] = rows.as_mut_slice() else {
        return Err(VoomError::Config(format!(
            "file_version {version_id} must have exactly one selected live local_path; found {}",
            rows.len()
        )));
    };
    let location_id = FileLocationId(u64_from_i64(
        row.0,
        concat!(module_path!(), ": ", stringify!(row.0)),
    )?);
    let location_version_id = u64_from_i64(row.1, "file_locations.file_version_id")?;
    if location_version_id != version_id.0 {
        return Err(VoomError::Conflict(format!(
            "file_location {location_id} belongs to file_version {}, not {version_id}",
            FileVersionId(location_version_id)
        )));
    }
    if row.2 != "local_path" {
        return Err(VoomError::Config(format!(
            "file_location {location_id} must be kind local_path"
        )));
    }
    Ok(PolicyFileLocation {
        id: location_id,
        value: std::mem::take(&mut row.3),
    })
}

async fn latest_policy_media_snapshot(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<MediaSnapshotId, VoomError> {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM media_snapshots WHERE file_version_id = ?")
            .bind(i64_from_u64(
                version_id.0,
                concat!(module_path!(), ": ", stringify!(version_id.0)),
            )?)
            .fetch_one(&mut **tx)
            .await
            .map_err(|err| VoomError::database_context("policy media snapshot lookup", err))?;
    id.map(|value| {
        u64_from_i64(value, concat!(module_path!(), ": ", stringify!(value))).map(MediaSnapshotId)
    })
    .transpose()?
    .ok_or_else(|| {
        VoomError::Config(format!(
            "file_version {version_id} has no media snapshot for verification"
        ))
    })
}

async fn policy_committed_handles(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<Vec<ArtifactHandle>, VoomError> {
    let rows = sqlx::query(
        "SELECT h.id, h.file_version_id, h.privacy_class, h.durability_class, \
                h.allowed_access_modes, h.mutability, h.created_at \
         FROM artifact_commit_records c \
         JOIN artifact_handles h ON h.id = c.artifact_handle_id \
         WHERE c.state = 'committed' AND c.result_file_version_id = ? \
         ORDER BY h.id",
    )
    .bind(i64_from_u64(
        version_id.0,
        concat!(module_path!(), ": ", stringify!(version_id.0)),
    )?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy committed artifact lookup", err))?;
    rows.iter().map(row_to_handle).collect()
}

async fn policy_canonical_handle(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    version_id: FileVersionId,
) -> Result<Option<ArtifactHandle>, VoomError> {
    let rows = sqlx::query(
        "SELECT id, file_version_id, privacy_class, durability_class, allowed_access_modes, \
                mutability, created_at \
         FROM artifact_handles \
         WHERE file_version_id = ? AND durability_class = 'active' \
           AND json_extract(source_lineage, '$.kind') = 'policy_verification' \
         ORDER BY id",
    )
    .bind(i64_from_u64(
        version_id.0,
        concat!(module_path!(), ": ", stringify!(version_id.0)),
    )?)
    .fetch_all(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy canonical artifact lookup", err))?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => row_to_handle(row).map(Some),
        _ => Err(VoomError::Conflict(format!(
            "file_version {version_id} has {} canonical verification handles",
            rows.len()
        ))),
    }
}

async fn require_policy_handle_facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    version: &PolicyFileVersion,
) -> Result<(), VoomError> {
    let file_version_id = require_policy_handle_content_facts(tx, handle_id, version).await?;
    if file_version_id != Some(version.id.0) {
        return Err(policy_handle_facts_conflict(handle_id, version.id));
    }
    Ok(())
}

async fn require_policy_handle_content_facts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    handle_id: ArtifactHandleId,
    version: &PolicyFileVersion,
) -> Result<Option<u64>, VoomError> {
    let row: Option<(Option<i64>, Option<String>, Option<i64>)> = sqlx::query_as(
        "SELECT size_bytes, checksum, file_version_id \
         FROM artifact_handles WHERE id = ?",
    )
    .bind(i64_from_u64(
        handle_id.0,
        concat!(module_path!(), ": ", stringify!(handle_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|err| VoomError::database_context("policy artifact facts lookup", err))?;
    let Some((size_bytes, checksum, file_version_id)) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_handle {handle_id} missing"
        )));
    };
    if size_bytes
        != Some(i64_from_u64(
            version.size_bytes,
            concat!(module_path!(), ": ", stringify!(version.size_bytes)),
        )?)
        || checksum.as_deref() != Some(version.content_hash.as_str())
    {
        return Err(policy_handle_facts_conflict(handle_id, version.id));
    }
    file_version_id
        .map(|value| u64_from_i64(value, "artifact_handles.file_version_id"))
        .transpose()
}

fn policy_handle_facts_conflict(
    handle_id: ArtifactHandleId,
    version_id: FileVersionId,
) -> VoomError {
    VoomError::Conflict(format!(
        "artifact_handle {handle_id} facts do not match file_version {version_id}"
    ))
}

fn decode_committed_ticket_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<CommittedTicketEvidence, VoomError> {
    let ticket_id = TicketId(evidence_u64(row, "ticket_id")?);
    let ticket_payload = evidence_json(row, "ticket_payload", ticket_id)?;
    let raw_result: String = evidence_value(row, "raw_result")?;
    serde_json::from_str::<JsonValue>(&raw_result).map_err(|error| {
        VoomError::database_context(format!("committed ticket {ticket_id} ticket result"), error)
    })?;
    let result = evidence_json(row, "result", ticket_id)?;
    Ok(CommittedTicketEvidence {
        ticket_id,
        ticket_job_id: evidence_optional_id(row, "ticket_job_id", JobId)?,
        ticket_payload,
        result,
        commit: decode_artifact_commit_evidence(row)?,
        verification: decode_artifact_verification_evidence(row)?,
        result_lease: decode_result_lease_evidence(row)?,
        source_file_asset_id: evidence_optional_id(row, "source_file_asset_id", FileAssetId)?,
        result_file_asset_id: evidence_optional_id(row, "result_file_asset_id", FileAssetId)?,
        location_file_version_id: evidence_optional_id(
            row,
            "location_file_version_id",
            FileVersionId,
        )?,
        snapshot_file_version_id: evidence_optional_id(
            row,
            "snapshot_file_version_id",
            FileVersionId,
        )?,
    })
}

fn decode_artifact_commit_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ArtifactCommitEvidence>, VoomError> {
    let Some(id) = evidence_optional_id(row, "commit_id", ArtifactCommitRecordId)? else {
        return Ok(None);
    };
    let state: String = evidence_value(row, "commit_state")?;
    Ok(Some(ArtifactCommitEvidence {
        id,
        artifact_handle_id: ArtifactHandleId(evidence_u64(row, "commit_artifact_handle_id")?),
        source_file_version_id: FileVersionId(evidence_u64(row, "commit_source_file_version_id")?),
        verification_id: ArtifactVerificationId(evidence_u64(row, "commit_verification_id")?),
        result_file_version_id: evidence_optional_id(
            row,
            "commit_result_file_version_id",
            FileVersionId,
        )?,
        result_file_location_id: evidence_optional_id(
            row,
            "commit_result_file_location_id",
            FileLocationId,
        )?,
        state: ArtifactCommitState::parse(&state)?,
        report: evidence_json_without_ticket(row, "commit_report")?,
        started_at: evidence_timestamp(row, "commit_started_at")?,
        promotion_started_at: evidence_optional_timestamp(row, "commit_promotion_started_at")?,
        finished_at: evidence_optional_timestamp(row, "commit_finished_at")?,
    }))
}

fn decode_artifact_verification_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ArtifactVerificationEvidence>, VoomError> {
    let Some(artifact_handle_id) =
        evidence_optional_id(row, "verification_artifact_handle_id", ArtifactHandleId)?
    else {
        return Ok(None);
    };
    let status: String = evidence_value(row, "verification_status")?;
    Ok(Some(ArtifactVerificationEvidence {
        artifact_handle_id,
        workflow_ticket_id: evidence_optional_id(row, "verification_ticket_id", TicketId)?,
        workflow_lease_id: evidence_optional_id(row, "verification_lease_id", LeaseId)?,
        status: ArtifactVerificationStatus::parse(&status)?,
        report: evidence_json_without_ticket(row, "verification_report")?,
        started_at: evidence_timestamp(row, "verification_started_at")?,
        finished_at: evidence_timestamp(row, "verification_finished_at")?,
    }))
}

fn decode_result_lease_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<Option<ResultLeaseEvidence>, VoomError> {
    let Some(ticket_id) = evidence_optional_id(row, "result_lease_ticket_id", TicketId)? else {
        return Ok(None);
    };
    let state: String = evidence_value(row, "result_lease_state")?;
    Ok(Some(ResultLeaseEvidence {
        ticket_id,
        state: LeaseState::parse(&state)?,
        acquired_at: evidence_timestamp(row, "result_lease_acquired_at")?,
        expires_at: evidence_timestamp(row, "result_lease_expires_at")?,
        last_heartbeat_at: evidence_timestamp(row, "result_lease_last_heartbeat_at")?,
        released_at: evidence_optional_timestamp(row, "result_lease_released_at")?,
    }))
}

fn decode_verified_ticket_evidence(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<VerifiedTicketEvidence, VoomError> {
    validate_verification_integer_fields(row)?;
    let verification = row_to_verification(row)?;
    validate_verification_lease_ticket(row, &verification)?;
    Ok(VerifiedTicketEvidence {
        verification,
        file_version_id: evidence_optional_id(row, "selected_file_version_id", FileVersionId)?,
        location_value: evidence_value(row, "selected_location_value")?,
    })
}

fn validate_verification_lease_ticket(
    row: &sqlx::sqlite::SqliteRow,
    verification: &ArtifactVerification,
) -> Result<(), VoomError> {
    let lease_ticket_id = evidence_optional_id(row, "selected_lease_ticket_id", TicketId)?;
    match (
        verification.workflow_ticket_id,
        verification.workflow_lease_id,
        lease_ticket_id,
    ) {
        (None, None, None) => Ok(()),
        (Some(ticket_id), Some(_), Some(lease_ticket_id)) if ticket_id == lease_ticket_id => Ok(()),
        _ => Err(VoomError::database(format!(
            "artifact verification {} workflow lease ticket mismatch",
            verification.id
        ))),
    }
}

fn validate_verification_integer_fields(row: &sqlx::sqlite::SqliteRow) -> Result<(), VoomError> {
    for field in [
        "id",
        "artifact_handle_id",
        "artifact_location_id",
        "worker_id",
        "expected_size_bytes",
    ] {
        evidence_u64(row, field)?;
    }
    for field in [
        "workflow_ticket_id",
        "workflow_lease_id",
        "observed_size_bytes",
    ] {
        let value: Option<i64> = evidence_value(row, field)?;
        if let Some(value) = value {
            u64::try_from(value).map_err(|error| {
                VoomError::database_context(format!("workflow evidence {field} is negative"), error)
            })?;
        }
    }
    Ok(())
}

fn evidence_value<T>(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<T, VoomError>
where
    for<'decode> T: sqlx::Decode<'decode, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
{
    row.try_get(field)
        .map_err(|error| VoomError::database_context(format!("workflow evidence {field}"), error))
}

fn evidence_u64(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<u64, VoomError> {
    let value: i64 = evidence_value(row, field)?;
    u64::try_from(value).map_err(|error| {
        VoomError::database_context(format!("workflow evidence {field} is negative"), error)
    })
}

fn evidence_optional_id<T>(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
    wrap: fn(u64) -> T,
) -> Result<Option<T>, VoomError> {
    let value: Option<i64> = evidence_value(row, field)?;
    value
        .map(|value| {
            u64::try_from(value).map(wrap).map_err(|error| {
                VoomError::database_context(format!("workflow evidence {field} is negative"), error)
            })
        })
        .transpose()
}

fn evidence_json(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
    ticket_id: TicketId,
) -> Result<JsonValue, VoomError> {
    let value: String = evidence_value(row, field)?;
    serde_json::from_str(&value).map_err(|error| {
        VoomError::database_context(
            format!("committed ticket {ticket_id} {field} decode"),
            error,
        )
    })
}

fn evidence_json_without_ticket(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<JsonValue, VoomError> {
    let value: String = evidence_value(row, field)?;
    serde_json::from_str(&value)
        .map_err(|error| VoomError::database_context(format!("workflow evidence {field}"), error))
}

fn evidence_timestamp(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<OffsetDateTime, VoomError> {
    let value: String = evidence_value(row, field)?;
    parse_iso8601(&value)
}

fn evidence_optional_timestamp(
    row: &sqlx::sqlite::SqliteRow,
    field: &str,
) -> Result<Option<OffsetDateTime>, VoomError> {
    let value: Option<String> = evidence_value(row, field)?;
    value.as_deref().map(parse_iso8601).transpose()
}
