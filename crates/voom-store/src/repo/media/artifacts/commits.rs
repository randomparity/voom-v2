use super::{
    ArtifactCommitFailure, ArtifactCommitRecord, ArtifactCommitRecordId, ArtifactCommitState,
    ArtifactHandleId, ArtifactVerificationId, ArtifactVerificationStatus, FileAssetId,
    FileLocationId, FileVersionId, JsonValue, NewArtifactCommitRecord, NewSidecarArtifactCommit,
    OffsetDateTime, SidecarArtifactCommit, SqliteArtifactRepo, VoomError, parse_error_code,
    parse_failure_class,
};
use sqlx::Row;

use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

const SELECT_ARTIFACT_COMMIT_RECORD_COLS: &str = "SELECT c.id, c.artifact_handle_id, \
    c.source_file_version_id, c.verification_id, c.target_path, c.result_file_version_id, \
    c.result_file_location_id, c.state, c.failure_class, c.error_code, c.message, \
    c.recovery_reason, c.temp_path, c.report, c.started_at, c.promotion_started_at, \
    c.finished_at";

type CommitVerificationRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    Option<String>,
    Option<i64>,
);

struct CommitVerificationFacts {
    verification_id: i64,
    verification_handle_id: i64,
    status: String,
    verification_path: String,
    location_kind: String,
    location_value: String,
    retired_at: Option<String>,
    location_handle_id: i64,
    handle_file_version_id: Option<i64>,
    source_retired_at: Option<String>,
    latest_successful_id: Option<i64>,
}

impl From<CommitVerificationRow> for CommitVerificationFacts {
    fn from(row: CommitVerificationRow) -> Self {
        let (
            verification_id,
            verification_handle_id,
            status,
            verification_path,
            location_kind,
            location_value,
            retired_at,
            location_handle_id,
            handle_file_version_id,
            source_retired_at,
            latest_successful_id,
        ) = row;
        Self {
            verification_id,
            verification_handle_id,
            status,
            verification_path,
            location_kind,
            location_value,
            retired_at,
            location_handle_id,
            handle_file_version_id,
            source_retired_at,
            latest_successful_id,
        }
    }
}

async fn validate_commit_verification(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewArtifactCommitRecord,
) -> Result<(), VoomError> {
    let row: Option<CommitVerificationRow> = sqlx::query_as(
        "SELECT v.id, v.artifact_handle_id, v.status, v.path, l.kind, l.value, l.retired_at, \
                l.artifact_handle_id, h.file_version_id, fv.retired_at, \
                (SELECT MAX(v2.id) \
                   FROM artifact_verifications v2 \
                  WHERE v2.artifact_handle_id = v.artifact_handle_id \
                    AND v2.artifact_location_id = v.artifact_location_id \
                    AND v2.status = 'succeeded') AS latest_successful_id \
         FROM artifact_verifications v \
         JOIN artifact_locations l ON l.id = v.artifact_location_id \
         JOIN artifact_handles h ON h.id = v.artifact_handle_id \
         LEFT JOIN file_versions fv ON fv.id = h.file_version_id \
         WHERE v.id = ?",
    )
    .bind(i64_from_u64(
        input.verification_id.0,
        concat!(module_path!(), ": ", stringify!(input.verification_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("artifact_verifications commit lookup", e))?;
    let Some(row) = row else {
        return Err(VoomError::NotFound(format!(
            "artifact_verifications {} missing",
            input.verification_id
        )));
    };
    let facts = CommitVerificationFacts::from(row);
    if !is_successful_live_staging_verification(&facts, input)? {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: verification {} is not a successful live staging \
             verification for artifact_handle {}",
            input.verification_id, input.artifact_handle_id
        )));
    }
    let source_file_version_id = i64_from_u64(
        input.source_file_version_id.0,
        "artifact_handles.file_version_id",
    )?;
    if facts.handle_file_version_id != Some(source_file_version_id) {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} does not match \
             artifact_handle {} file_version_id",
            input.source_file_version_id, input.artifact_handle_id
        )));
    }
    let verification_id = i64_from_u64(input.verification_id.0, "artifact_verifications.id")?;
    if facts.verification_id != verification_id || facts.source_retired_at.is_some() {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records: source_file_version_id {} is not live",
            input.source_file_version_id
        )));
    }
    Ok(())
}

fn is_successful_live_staging_verification(
    facts: &CommitVerificationFacts,
    input: &NewArtifactCommitRecord,
) -> Result<bool, VoomError> {
    Ok(u64_from_i64(
        facts.verification_handle_id,
        "artifact_verifications.artifact_handle_id",
    )? == input.artifact_handle_id.0
        && u64_from_i64(
            facts.location_handle_id,
            "artifact_locations.artifact_handle_id",
        )? == input.artifact_handle_id.0
        && facts.status == ArtifactVerificationStatus::Succeeded.as_str()
        && facts.verification_path == facts.location_value
        && facts.location_kind == "staging"
        && facts.retired_at.is_none()
        && facts.latest_successful_id
            == Some(i64_from_u64(
                input.verification_id.0,
                "artifact_verifications.id",
            )?))
}

async fn validate_committed_result(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: ArtifactCommitRecordId,
    result_file_version_id: FileVersionId,
    result_file_location_id: FileLocationId,
) -> Result<(), VoomError> {
    let target = pending_commit_target(tx, commit_id).await?;
    validate_result_version(tx, result_file_version_id, target.source_version_id).await?;
    validate_result_location(tx, result_file_location_id, result_file_version_id, &target).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRootedTarget {
    storage_root_id: u64,
    provider_relative_locator: String,
}

struct PendingCommitTarget {
    source_version_id: u64,
    target_path: String,
    storage_root_id: super::StorageRootId,
    provider_relative_locator: super::ProviderRelativeLocator,
}

async fn pending_commit_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_id: ArtifactCommitRecordId,
) -> Result<PendingCommitTarget, VoomError> {
    // Accept `recovery_required` as well as `pending`: the recovery entrypoint
    // finalizes a re-driven commit on the existing (recovery_required) record.
    let pending_row: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT source_file_version_id, target_path, report FROM artifact_commit_records \
         WHERE id = ? AND state IN ('pending', 'recovery_required')",
    )
    .bind(i64_from_u64(
        commit_id.0,
        concat!(module_path!(), ": ", stringify!(commit_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("artifact_commit_records pending lookup", e))?;
    let (source_version_id, target_path, report) = pending_row.ok_or_else(|| {
        VoomError::Conflict(format!(
            "artifact_commit_records commit: id={commit_id} not pending or recovery_required"
        ))
    })?;
    let report: JsonValue = serde_json::from_str(&report).map_err(|error| {
        VoomError::database_context("artifact_commit_records.report decode", error)
    })?;
    let rooted = report.get("rooted_target").ok_or_else(|| {
        VoomError::database(format!(
            "artifact_commit_records {commit_id} report missing rooted_target"
        ))
    })?;
    let rooted: PersistedRootedTarget =
        serde_json::from_value(rooted.clone()).map_err(|error| {
            VoomError::database_context(
                format!("artifact_commit_records {commit_id} rooted_target decode"),
                error,
            )
        })?;
    if rooted.storage_root_id == 0 || rooted.storage_root_id > i64::MAX.unsigned_abs() {
        return Err(VoomError::database(format!(
            "artifact_commit_records {commit_id} rooted_target.storage_root_id {} is not a \
             valid SQLite ID",
            rooted.storage_root_id
        )));
    }
    Ok(PendingCommitTarget {
        source_version_id: u64_from_i64(
            source_version_id,
            "artifact_commit_records.source_file_version_id",
        )?,
        target_path,
        storage_root_id: super::StorageRootId(rooted.storage_root_id),
        provider_relative_locator: super::ProviderRelativeLocator::parse_database(
            "artifact_commit_records.report.rooted_target.provider_relative_locator",
            &rooted.provider_relative_locator,
        )?,
    })
}

async fn validate_result_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    result_file_version_id: FileVersionId,
    source_version_id: u64,
) -> Result<(), VoomError> {
    let version_row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT produced_by, produced_from_version_id, retired_at FROM file_versions WHERE id = ?",
    )
    .bind(i64_from_u64(
        result_file_version_id.0,
        concat!(module_path!(), ": ", stringify!(result_file_version_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("file_versions commit-result lookup", e))?;
    let Some((produced_by, produced_from_version_id, result_retired_at)) = version_row else {
        return Err(VoomError::NotFound(format!(
            "file_versions {result_file_version_id} missing"
        )));
    };
    let produced_from_version_id = produced_from_version_id
        .map(|value| u64_from_i64(value, "file_versions.produced_from_version_id"))
        .transpose()?;
    if produced_by != "staged_commit"
        || produced_from_version_id != Some(source_version_id)
        || result_retired_at.is_some()
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records commit: result version {result_file_version_id} \
             is not a staged_commit child of source version {}",
            FileVersionId(source_version_id)
        )));
    }
    Ok(())
}

type CommitResultLocationRow = (i64, String, Option<i64>, Option<String>, Option<String>);

async fn validate_result_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    result_file_location_id: FileLocationId,
    result_file_version_id: FileVersionId,
    target: &PendingCommitTarget,
) -> Result<(), VoomError> {
    let location_row: Option<CommitResultLocationRow> = sqlx::query_as(
        "SELECT file_version_id, address_state, storage_root_id, \
                    provider_relative_locator, retired_at \
             FROM file_locations fl \
             WHERE fl.id = ?",
    )
    .bind(i64_from_u64(
        result_file_location_id.0,
        concat!(module_path!(), ": ", stringify!(result_file_location_id.0)),
    )?)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("file_locations commit-result lookup", e))?;
    let (location_version_id, address_state, storage_root_id, relative_locator, retired_at) =
        location_row.ok_or_else(|| {
            VoomError::NotFound(format!("file_locations {result_file_location_id} missing"))
        })?;
    let rooted_address_is_valid = match (storage_root_id, relative_locator) {
        (Some(storage_root_id), Some(relative_locator)) => {
            super::StorageRootId(u64_from_i64(
                storage_root_id,
                "file_locations.storage_root_id",
            )?) == target.storage_root_id
                && super::ProviderRelativeLocator::parse_database(
                    "file_locations.provider_relative_locator",
                    &relative_locator,
                )? == target.provider_relative_locator
        }
        _ => false,
    };
    if u64_from_i64(
        location_version_id,
        concat!(module_path!(), ": ", stringify!(location_version_id)),
    )? != result_file_version_id.0
        || address_state != "rooted"
        || !rooted_address_is_valid
        || retired_at.is_some()
    {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records commit: result location {result_file_location_id} \
             is not a live rooted location for committed target {:?} on \
             file_version {result_file_version_id}",
            target.target_path
        )));
    }
    Ok(())
}

async fn get_active_commit_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
) -> Result<ArtifactCommitRecord, VoomError> {
    let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
        + " FROM artifact_commit_records c \
           WHERE c.id = ? AND c.state IN ('pending', 'recovery_required')";
    let row = sqlx::query(&sql)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records pending get", e))?;
    row.as_ref()
        .map(row_to_commit_record)
        .transpose()?
        .ok_or_else(|| {
            VoomError::Conflict(format!(
                "artifact_commit_records sidecar commit: id={id} is not active"
            ))
        })
}

fn map_commit_insert_err(
    err: &sqlx::Error,
    artifact_handle_id: ArtifactHandleId,
    target_path: &str,
) -> VoomError {
    if is_unique_violation(err) {
        VoomError::Conflict(format!(
            "artifact_commit_records: artifact_handle {artifact_handle_id} or target_path \
             {target_path:?} already has an active owner"
        ))
    } else {
        VoomError::database(format!("artifact_commit_records insert: {err}"))
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => db_err.is_unique_violation(),
        _ => false,
    }
}

fn checked_sqlite_id(value: u64, context: &str) -> Result<i64, VoomError> {
    i64::try_from(value)
        .map_err(|error| VoomError::Internal(format!("{context} exceeds SQLite integer: {error}")))
}

impl SqliteArtifactRepo {
    pub async fn create_pending_commit_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactCommitRecord,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        validate_commit_verification(tx, &input).await?;
        let report = serialize_json(&input.report, "artifact_commit_records.report")?;
        let started_at = iso8601(input.started_at)?;
        let res = sqlx::query(
            "INSERT INTO artifact_commit_records \
             (artifact_handle_id, source_file_version_id, verification_id, target_path, \
              state, temp_path, report, started_at) \
             VALUES (?, ?, ?, ?, 'pending', ?, ?, ?)",
        )
        .bind(i64_from_u64(
            input.artifact_handle_id.0,
            concat!(module_path!(), ": ", stringify!(input.artifact_handle_id.0)),
        )?)
        .bind(i64_from_u64(
            input.source_file_version_id.0,
            concat!(
                module_path!(),
                ": ",
                stringify!(input.source_file_version_id.0)
            ),
        )?)
        .bind(i64_from_u64(
            input.verification_id.0,
            concat!(module_path!(), ": ", stringify!(input.verification_id.0)),
        )?)
        .bind(&input.target_path)
        .bind(&input.temp_path)
        .bind(report)
        .bind(&started_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| map_commit_insert_err(&e, input.artifact_handle_id, &input.target_path))?;

        Ok(ArtifactCommitRecord {
            id: ArtifactCommitRecordId(u64_from_i64(
                res.last_insert_rowid(),
                concat!(module_path!(), ": ", stringify!(res.last_insert_rowid())),
            )?),
            artifact_handle_id: input.artifact_handle_id,
            source_file_version_id: input.source_file_version_id,
            verification_id: input.verification_id,
            target_path: input.target_path,
            result_file_version_id: None,
            result_file_location_id: None,
            state: ArtifactCommitState::Pending,
            failure_class: None,
            error_code: None,
            message: None,
            recovery_reason: None,
            temp_path: input.temp_path,
            report: input.report,
            started_at: input.started_at,
            promotion_started_at: None,
            finished_at: None,
        })
    }

    pub async fn mark_commit_committed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        result_file_version_id: FileVersionId,
        result_file_location_id: FileLocationId,
        promotion_started_at: OffsetDateTime,
        finished_at: OffsetDateTime,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        validate_committed_result(tx, id, result_file_version_id, result_file_location_id).await?;
        let promotion_started_at = iso8601(promotion_started_at)?;
        let finished_at = iso8601(finished_at)?;
        // `recovery_required` is allowed alongside `pending` so the recovery
        // entrypoint can finalize a re-driven commit on the existing record;
        // `committed`/`failed` remain terminal and are still rejected.
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'committed', result_file_version_id = ?, result_file_location_id = ?, \
                 promotion_started_at = ?, finished_at = ?, \
                 failure_class = NULL, error_code = NULL, message = NULL, recovery_reason = NULL \
             WHERE id = ? AND state IN ('pending', 'recovery_required')",
        )
        .bind(i64_from_u64(
            result_file_version_id.0,
            concat!(module_path!(), ": ", stringify!(result_file_version_id.0)),
        )?)
        .bind(i64_from_u64(
            result_file_location_id.0,
            concat!(module_path!(), ": ", stringify!(result_file_location_id.0)),
        )?)
        .bind(&promotion_started_at)
        .bind(&finished_at)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records commit", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "commit").await
    }

    pub async fn mark_commit_failed_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let finished_at = iso8601(failure.finished_at)?;
        let failure_class = failure.failure_class.as_str();
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'failed', failure_class = ?, error_code = ?, message = ?, finished_at = ? \
             WHERE id = ? AND state = 'pending'",
        )
        .bind(failure_class)
        .bind(failure.error_code.as_str())
        .bind(&failure.message)
        .bind(&finished_at)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records fail", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "fail").await
    }

    pub async fn mark_commit_recovery_required_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        failure: ArtifactCommitFailure,
        recovery_reason: String,
    ) -> Result<ArtifactCommitRecord, VoomError> {
        let finished_at = iso8601(failure.finished_at)?;
        let failure_class = failure.failure_class.as_str();
        let res = sqlx::query(
            "UPDATE artifact_commit_records \
             SET state = 'recovery_required', failure_class = ?, error_code = ?, message = ?, \
                 recovery_reason = ?, finished_at = ? \
             WHERE id = ? AND state IN ('pending', 'recovery_required')",
        )
        .bind(failure_class)
        .bind(failure.error_code.as_str())
        .bind(&failure.message)
        .bind(&recovery_reason)
        .bind(&finished_at)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records recovery_required", e))?;
        changed_commit_record(tx, id, res.rows_affected(), "recovery_required").await
    }

    /// Replace a pending commit report in the caller's transaction.
    pub async fn update_pending_commit_report_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: ArtifactCommitRecordId,
        report: &JsonValue,
    ) -> Result<(), VoomError> {
        let report = serialize_json(report, "artifact_commit_records.report")?;
        let result = sqlx::query(
            "UPDATE artifact_commit_records SET report = ? WHERE id = ? AND state = 'pending'",
        )
        .bind(report)
        .bind(checked_sqlite_id(id.0, "artifact commit id")?)
        .execute(&mut **tx)
        .await
        .map_err(|error| {
            VoomError::database_context("artifact_commit_records report update", error)
        })?;
        if result.rows_affected() != 1 {
            return Err(VoomError::Conflict(format!(
                "artifact_commit_records report update: id={id} not pending"
            )));
        }
        Ok(())
    }

    pub async fn get_commit_record(
        &self,
        id: ArtifactCommitRecordId,
    ) -> Result<Option<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " FROM artifact_commit_records c WHERE c.id = ?";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(
                id.0,
                concat!(module_path!(), ": ", stringify!(id.0)),
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_records get", e))?;
        row.as_ref().map(row_to_commit_record).transpose()
    }

    pub async fn list_commit_records(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactCommitRecord>, VoomError> {
        let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
            + " \
             FROM artifact_commit_records c \
             WHERE c.artifact_handle_id = ? ORDER BY c.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_commit_records list", e))?;
        rows.iter().map(row_to_commit_record).collect()
    }

    /// `true` when any commit record for `source_file_version_id` is currently
    /// in the recovery-required state — a durable "unrecovered prior mutation"
    /// signal the safety gate consults (ADR 0028). Recovery-required is a live
    /// state, so a later recovered/committed record clears it.
    pub async fn has_recovery_required_for_source_version(
        &self,
        source_file_version_id: FileVersionId,
    ) -> Result<bool, VoomError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM artifact_commit_records \
             WHERE source_file_version_id = ? AND state = 'recovery_required'",
        )
        .bind(i64_from_u64(
            source_file_version_id.0,
            concat!(module_path!(), ": ", stringify!(source_file_version_id.0)),
        )?)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            VoomError::database_context("artifact_commit_records recovery_required count", e)
        })?;
        Ok(count > 0)
    }

    pub async fn record_verified_sidecar_commit_rows_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewSidecarArtifactCommit,
    ) -> Result<SidecarArtifactCommit, VoomError> {
        let pending = validate_sidecar_commit_input(tx, &input).await?;

        let created_at = iso8601(input.observed_at)?;
        let finished_at = iso8601(input.finished_at)?;
        let size_i64 = i64::try_from(input.size_bytes).map_err(|_| {
            VoomError::Config(format!(
                "file_versions: size_bytes {} overflows i64",
                input.size_bytes
            ))
        })?;

        let asset_res = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
            .bind(&created_at)
            .execute(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("file_assets sidecar insert", e))?;
        let file_asset_id = FileAssetId(u64_from_i64(
            asset_res.last_insert_rowid(),
            "file_assets.id",
        )?);

        let version_res = sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, \
              produced_from_version_id, created_at) \
             VALUES (?, ?, ?, 'staged_commit', ?, ?)",
        )
        .bind(i64_from_u64(
            file_asset_id.0,
            "file_versions.file_asset_id",
        )?)
        .bind(&input.content_hash)
        .bind(size_i64)
        .bind(i64_from_u64(
            pending.source_file_version_id.0,
            "file_versions.produced_from_version_id",
        )?)
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("file_versions sidecar insert", e))?;
        let file_version_id = FileVersionId(u64_from_i64(
            version_res.last_insert_rowid(),
            "file_versions.id",
        )?);

        let location_res = sqlx::query(
            "INSERT INTO file_locations \
             (file_version_id, address_state, storage_root_id, provider_relative_locator, \
              proof_kind, proof_value, observed_at) \
             VALUES (?, 'rooted', ?, ?, NULL, NULL, ?)",
        )
        .bind(i64_from_u64(
            file_version_id.0,
            "file_locations.file_version_id",
        )?)
        .bind(i64_from_u64(
            input.storage_root_id.0,
            "file_locations.storage_root_id",
        )?)
        .bind(input.provider_relative_locator.as_str())
        .bind(&created_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("file_locations sidecar insert", e))?;
        let file_location_id = FileLocationId(u64_from_i64(
            location_res.last_insert_rowid(),
            "file_locations.id",
        )?);

        let commit_record = finalize_sidecar_commit_record_in_tx(
            tx,
            input.commit_record_id,
            file_version_id,
            file_location_id,
            &finished_at,
        )
        .await?;

        Ok(SidecarArtifactCommit {
            commit_record,
            file_asset_id,
            file_version_id,
            file_location_id,
        })
    }
}

async fn finalize_sidecar_commit_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    commit_record_id: ArtifactCommitRecordId,
    file_version_id: FileVersionId,
    file_location_id: FileLocationId,
    finished_at: &str,
) -> Result<ArtifactCommitRecord, VoomError> {
    let res = sqlx::query(
        "UPDATE artifact_commit_records \
         SET state = 'committed', result_file_version_id = ?, result_file_location_id = ?, \
             promotion_started_at = NULL, finished_at = ?, failure_class = NULL, \
             error_code = NULL, message = NULL, recovery_reason = NULL \
         WHERE id = ? AND state IN ('pending', 'recovery_required')",
    )
    .bind(i64_from_u64(
        file_version_id.0,
        "artifact_commit_records.result_file_version_id",
    )?)
    .bind(i64_from_u64(
        file_location_id.0,
        "artifact_commit_records.result_file_location_id",
    )?)
    .bind(finished_at)
    .bind(i64_from_u64(
        commit_record_id.0,
        "artifact_commit_records.id",
    )?)
    .execute(&mut **tx)
    .await
    .map_err(|e| VoomError::database_context("artifact_commit_records sidecar commit", e))?;
    changed_commit_record(tx, commit_record_id, res.rows_affected(), "sidecar_commit").await
}

async fn validate_sidecar_commit_input(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewSidecarArtifactCommit,
) -> Result<ArtifactCommitRecord, VoomError> {
    let pending = get_active_commit_record_in_tx(tx, input.commit_record_id).await?;
    if pending.target_path != input.target_path {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records sidecar commit: \
             target_path {:?} does not match pending target {:?}",
            input.target_path, pending.target_path
        )));
    }
    if input.target_path.is_empty() {
        return Err(VoomError::Config(
            "artifact_commit_records sidecar commit: target_path is empty".to_owned(),
        ));
    }
    validate_commit_verification(
        tx,
        &NewArtifactCommitRecord {
            artifact_handle_id: pending.artifact_handle_id,
            source_file_version_id: pending.source_file_version_id,
            verification_id: pending.verification_id,
            target_path: pending.target_path.clone(),
            temp_path: pending.temp_path.clone(),
            report: pending.report.clone(),
            started_at: pending.started_at,
        },
    )
    .await?;
    Ok(pending)
}

async fn changed_commit_record(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
    rows_affected: u64,
    operation: &str,
) -> Result<ArtifactCommitRecord, VoomError> {
    if rows_affected != 1 {
        return Err(VoomError::Conflict(format!(
            "artifact_commit_records {operation}: id={id} not pending"
        )));
    }
    get_commit_record_in_tx(tx, id).await?.ok_or_else(|| {
        VoomError::Internal(format!(
            "artifact_commit_records post-{operation} get vanished: {id}"
        ))
    })
}

async fn get_commit_record_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    id: ArtifactCommitRecordId,
) -> Result<Option<ArtifactCommitRecord>, VoomError> {
    let sql = SELECT_ARTIFACT_COMMIT_RECORD_COLS.to_owned()
        + " FROM artifact_commit_records c WHERE c.id = ?";
    let row = sqlx::query(&sql)
        .bind(i64_from_u64(
            id.0,
            concat!(module_path!(), ": ", stringify!(id.0)),
        )?)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_commit_records get", e))?;
    row.as_ref().map(row_to_commit_record).transpose()
}

fn row_to_commit_record(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactCommitRecord, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let artifact_handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let source_file_version_id: i64 = row
        .try_get("source_file_version_id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let verification_id: i64 = row
        .try_get("verification_id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let target_path: String = row
        .try_get("target_path")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let result_file_version_id: Option<i64> = row
        .try_get("result_file_version_id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let result_file_location_id: Option<i64> = row
        .try_get("result_file_location_id")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let state: String = row
        .try_get("state")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let error_code: Option<String> = row
        .try_get("error_code")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let message: Option<String> = row
        .try_get("message")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let recovery_reason: Option<String> = row
        .try_get("recovery_reason")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let temp_path: Option<String> = row
        .try_get("temp_path")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let report: String = row
        .try_get("report")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let started_at: String = row
        .try_get("started_at")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let promotion_started_at: Option<String> = row
        .try_get("promotion_started_at")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;
    let finished_at: Option<String> = row
        .try_get("finished_at")
        .map_err(|e| map_row_err("artifact_commit_records", e))?;

    Ok(ArtifactCommitRecord {
        id: ArtifactCommitRecordId(u64_from_i64(
            id,
            concat!(module_path!(), ": ", stringify!(id)),
        )?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            artifact_handle_id,
            concat!(module_path!(), ": ", stringify!(artifact_handle_id)),
        )?),
        source_file_version_id: FileVersionId(u64_from_i64(
            source_file_version_id,
            concat!(module_path!(), ": ", stringify!(source_file_version_id)),
        )?),
        verification_id: ArtifactVerificationId(u64_from_i64(
            verification_id,
            concat!(module_path!(), ": ", stringify!(verification_id)),
        )?),
        target_path,
        result_file_version_id: result_file_version_id
            .map(|v| {
                u64_from_i64(v, concat!(module_path!(), ": ", stringify!(v))).map(FileVersionId)
            })
            .transpose()?,
        result_file_location_id: result_file_location_id
            .map(|v| {
                u64_from_i64(v, concat!(module_path!(), ": ", stringify!(v))).map(FileLocationId)
            })
            .transpose()?,
        state: ArtifactCommitState::parse(&state)?,
        failure_class: failure_class
            .as_deref()
            .map(|value| parse_failure_class(value, "artifact_commit_records.failure_class"))
            .transpose()?,
        error_code: error_code
            .as_deref()
            .map(|value| parse_error_code(value, "artifact_commit_records.error_code"))
            .transpose()?,
        message,
        recovery_reason,
        temp_path,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::database_context("artifact_commit_records report", e))?,
        started_at: parse_iso8601(&started_at)?,
        promotion_started_at: promotion_started_at
            .map(|s| parse_iso8601(&s))
            .transpose()?,
        finished_at: finished_at.map(|s| parse_iso8601(&s)).transpose()?,
    })
}
