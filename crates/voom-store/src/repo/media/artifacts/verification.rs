use super::{
    ArtifactHandleId, ArtifactLocationId, ArtifactVerification, ArtifactVerificationId,
    ArtifactVerificationStatus, ErrorCode, FailureClass, JobId, LeaseId, NewArtifactVerification,
    SqliteArtifactRepo, TicketId, VoomError, WorkerId, parse_error_code, parse_failure_class,
};
use sqlx::Row;

use super::super::common::{
    i64_from_u64, iso8601, map_row_err, parse_iso8601, serialize_json, u64_from_i64,
};

pub(super) const SELECT_ARTIFACT_VERIFICATION_COLS: &str = "SELECT v.id, v.artifact_handle_id, \
    v.artifact_location_id, v.path, v.worker_id, v.workflow_ticket_id, v.workflow_lease_id, \
    v.status, v.expected_size_bytes, \
    v.expected_checksum, v.observed_size_bytes, v.observed_checksum, v.failure_class, \
    v.error_code, v.message, v.report, v.started_at, v.finished_at";

impl SqliteArtifactRepo {
    pub async fn record_verification_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewArtifactVerification,
    ) -> Result<ArtifactVerification, VoomError> {
        if input.workflow_ticket_id.is_some() != input.workflow_lease_id.is_some() {
            return Err(VoomError::Config(
                "artifact_verifications workflow ticket and lease must be both set or both absent"
                    .to_owned(),
            ));
        }
        validate_verification_location(tx, &input).await?;

        let report = serialize_json(&input.report, "artifact_verifications.report")?;
        let started_at = iso8601(input.started_at)?;
        let finished_at = iso8601(input.finished_at)?;
        let artifact_handle_id = i64_from_u64(
            input.artifact_handle_id.0,
            "artifact_verifications.artifact_handle_id",
        )?;
        let artifact_location_id = i64_from_u64(
            input.artifact_location_id.0,
            "artifact_verifications.artifact_location_id",
        )?;
        let worker_id = i64_from_u64(input.worker_id.0, "artifact_verifications.worker_id")?;
        let workflow_ticket_id = input
            .workflow_ticket_id
            .map(|id| i64_from_u64(id.0, "artifact_verifications.workflow_ticket_id"))
            .transpose()?;
        let workflow_lease_id = input
            .workflow_lease_id
            .map(|id| i64_from_u64(id.0, "artifact_verifications.workflow_lease_id"))
            .transpose()?;
        let expected_size_bytes = i64_from_u64(
            input.expected_size_bytes,
            "artifact_verifications.expected_size_bytes",
        )?;
        let observed_size_bytes = input
            .observed_size_bytes
            .map(|value| i64_from_u64(value, "artifact_verifications.observed_size_bytes"))
            .transpose()?;
        let failure_class = input.failure_class.map(FailureClass::as_str);
        let error_code = input.error_code.map(ErrorCode::as_str);
        let res = sqlx::query(
            "INSERT INTO artifact_verifications \
             (artifact_handle_id, artifact_location_id, path, worker_id, \
              workflow_ticket_id, workflow_lease_id, status, \
              expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
              failure_class, error_code, message, report, started_at, finished_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(artifact_handle_id)
        .bind(artifact_location_id)
        .bind(&input.path)
        .bind(worker_id)
        .bind(workflow_ticket_id)
        .bind(workflow_lease_id)
        .bind(input.status.as_str())
        .bind(expected_size_bytes)
        .bind(&input.expected_checksum)
        .bind(observed_size_bytes)
        .bind(&input.observed_checksum)
        .bind(failure_class)
        .bind(error_code)
        .bind(&input.message)
        .bind(report)
        .bind(&started_at)
        .bind(&finished_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::database_context("artifact_verifications insert", e))?;

        Ok(ArtifactVerification {
            id: ArtifactVerificationId(u64_from_i64(
                res.last_insert_rowid(),
                "artifact_verifications.id",
            )?),
            artifact_handle_id: input.artifact_handle_id,
            artifact_location_id: input.artifact_location_id,
            path: input.path,
            worker_id: input.worker_id,
            workflow_ticket_id: input.workflow_ticket_id,
            workflow_lease_id: input.workflow_lease_id,
            status: input.status,
            expected_size_bytes: input.expected_size_bytes,
            expected_checksum: input.expected_checksum,
            observed_size_bytes: input.observed_size_bytes,
            observed_checksum: input.observed_checksum,
            failure_class: input.failure_class,
            error_code: input.error_code,
            message: input.message,
            report: input.report,
            started_at: input.started_at,
            finished_at: input.finished_at,
        })
    }

    pub async fn latest_successful_verification_for_live_staging_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        handle_id: ArtifactHandleId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " \
             FROM artifact_verifications v \
             JOIN artifact_locations l ON l.id = v.artifact_location_id \
             WHERE v.artifact_handle_id = ? AND v.status = 'succeeded' \
               AND l.artifact_handle_id = v.artifact_handle_id \
               AND l.kind = 'staging' AND l.retired_at IS NULL \
             ORDER BY v.id DESC LIMIT 1";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_verifications latest", e))?;
        row.as_ref().map(row_to_verification).transpose()
    }

    pub async fn list_verifications(
        &self,
        handle_id: ArtifactHandleId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " \
             FROM artifact_verifications v \
             WHERE v.artifact_handle_id = ? ORDER BY v.id ASC";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                handle_id.0,
                concat!(module_path!(), ": ", stringify!(handle_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("artifact_verifications list", e))?;
        rows.iter().map(row_to_verification).collect()
    }

    pub async fn verification_for_workflow_lease(
        &self,
        lease_id: LeaseId,
    ) -> Result<Option<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " FROM artifact_verifications v WHERE v.workflow_lease_id = ?";
        let row = sqlx::query(&sql)
            .bind(i64_from_u64(
                lease_id.0,
                concat!(module_path!(), ": ", stringify!(lease_id.0)),
            )?)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| {
                VoomError::database_context("artifact_verifications workflow lease lookup", err)
            })?;
        row.as_ref().map(row_to_verification).transpose()
    }

    pub async fn verifications_for_workflow_job(
        &self,
        job_id: JobId,
    ) -> Result<Vec<ArtifactVerification>, VoomError> {
        let sql = SELECT_ARTIFACT_VERIFICATION_COLS.to_owned()
            + " FROM artifact_verifications v \
               WHERE v.workflow_ticket_id IN (SELECT id FROM tickets WHERE job_id = ?) \
                  OR v.id IN ( \
                      SELECT artifact_verification_id \
                      FROM workflow_file_phase_summaries \
                      WHERE job_id = ? AND artifact_verification_id IS NOT NULL \
                  ) \
               ORDER BY v.id";
        let rows = sqlx::query(&sql)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .bind(i64_from_u64(
                job_id.0,
                concat!(module_path!(), ": ", stringify!(job_id.0)),
            )?)
            .fetch_all(&self.pool)
            .await
            .map_err(|err| {
                VoomError::database_context("artifact_verifications workflow job lookup", err)
            })?;
        rows.iter().map(row_to_verification).collect()
    }
}

async fn validate_verification_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    input: &NewArtifactVerification,
) -> Result<(), VoomError> {
    let location_id = i64_from_u64(input.artifact_location_id.0, "artifact_locations.id")?;
    let owner: Option<(i64, String)> =
        sqlx::query_as("SELECT artifact_handle_id, value FROM artifact_locations WHERE id = ?")
            .bind(location_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| VoomError::database_context("artifact_locations owner lookup", e))?;
    let (owner_id, location_value) = owner.ok_or_else(|| {
        VoomError::NotFound(format!(
            "artifact_locations {} missing",
            input.artifact_location_id
        ))
    })?;
    let owner_id = u64_from_i64(owner_id, "artifact_locations.artifact_handle_id")?;
    if owner_id != input.artifact_handle_id.0 {
        return Err(VoomError::Conflict(format!(
            "artifact_verifications: location {} belongs to artifact_handle {}",
            input.artifact_location_id,
            ArtifactHandleId(owner_id)
        )));
    }
    if input.path != location_value {
        return Err(VoomError::Conflict(format!(
            "artifact_verifications: path {:?} does not match artifact_location {} value {:?}",
            input.path, input.artifact_location_id, location_value
        )));
    }
    Ok(())
}

pub(super) fn row_to_verification(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ArtifactVerification, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let artifact_handle_id: i64 = row
        .try_get("artifact_handle_id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let artifact_location_id: i64 = row
        .try_get("artifact_location_id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let path: String = row
        .try_get("path")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let workflow_ticket_id: Option<i64> = row
        .try_get("workflow_ticket_id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let workflow_lease_id: Option<i64> = row
        .try_get("workflow_lease_id")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let status: String = row
        .try_get("status")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let expected_size_bytes: i64 = row
        .try_get("expected_size_bytes")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let expected_checksum: String = row
        .try_get("expected_checksum")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let observed_size_bytes: Option<i64> = row
        .try_get("observed_size_bytes")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let observed_checksum: Option<String> = row
        .try_get("observed_checksum")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let failure_class: Option<String> = row
        .try_get("failure_class")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let error_code: Option<String> = row
        .try_get("error_code")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let message: Option<String> = row
        .try_get("message")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let report: String = row
        .try_get("report")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let started_at: String = row
        .try_get("started_at")
        .map_err(|e| map_row_err("artifact_verifications", e))?;
    let finished_at: String = row
        .try_get("finished_at")
        .map_err(|e| map_row_err("artifact_verifications", e))?;

    Ok(ArtifactVerification {
        id: ArtifactVerificationId(u64_from_i64(id, "artifact_verifications.id")?),
        artifact_handle_id: ArtifactHandleId(u64_from_i64(
            artifact_handle_id,
            "artifact_verifications.artifact_handle_id",
        )?),
        artifact_location_id: ArtifactLocationId(u64_from_i64(
            artifact_location_id,
            "artifact_verifications.artifact_location_id",
        )?),
        path,
        worker_id: WorkerId(u64_from_i64(worker_id, "artifact_verifications.worker_id")?),
        workflow_ticket_id: workflow_ticket_id
            .map(|id| u64_from_i64(id, "artifact_verifications.workflow_ticket_id").map(TicketId))
            .transpose()?,
        workflow_lease_id: workflow_lease_id
            .map(|id| u64_from_i64(id, "artifact_verifications.workflow_lease_id").map(LeaseId))
            .transpose()?,
        status: ArtifactVerificationStatus::parse(&status)?,
        expected_size_bytes: u64_from_i64(
            expected_size_bytes,
            "artifact_verifications.expected_size_bytes",
        )?,
        expected_checksum,
        observed_size_bytes: observed_size_bytes
            .map(|value| u64_from_i64(value, "artifact_verifications.observed_size_bytes"))
            .transpose()?,
        observed_checksum,
        failure_class: failure_class
            .as_deref()
            .map(|value| parse_failure_class(value, "artifact_verifications.failure_class"))
            .transpose()?,
        error_code: error_code
            .as_deref()
            .map(|value| parse_error_code(value, "artifact_verifications.error_code"))
            .transpose()?,
        message,
        report: serde_json::from_str(&report)
            .map_err(|e| VoomError::database_context("artifact_verifications report", e))?,
        started_at: parse_iso8601(&started_at)?,
        finished_at: parse_iso8601(&finished_at)?,
    })
}
