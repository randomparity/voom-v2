//! Durable exclusive ownership of a local accelerator device.

use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{VoomError, WorkerId};

use super::super::common::{i64_from_u64, iso8601, parse_iso8601, u32_from_i64, u64_from_i64};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAcceleratorClaim {
    pub hardware_token: String,
    pub worker_id: WorkerId,
    pub boot_id: String,
    pub supervisor_pid: u32,
    pub supervisor_start_ticks: u64,
    pub process_group_id: u32,
    pub capacity: u32,
    pub claimed_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceleratorClaim {
    pub hardware_token: String,
    pub worker_id: WorkerId,
    pub boot_id: String,
    pub supervisor_pid: u32,
    pub supervisor_start_ticks: u64,
    pub process_group_id: u32,
    pub capacity: u32,
    pub claimed_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteAcceleratorClaimRepo {
    pub(crate) pool: SqlitePool,
}

impl SqliteAcceleratorClaimRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Exclusively claims one stable hardware token inside the caller's
    /// registration transaction.
    ///
    /// # Errors
    /// Returns `Conflict` when another worker owns the token, or a database
    /// error for malformed input and storage failures.
    pub async fn claim_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        input: NewAcceleratorClaim,
    ) -> Result<AcceleratorClaim, VoomError> {
        let claimed_at = iso8601(input.claimed_at)?;
        let result = sqlx::query(
            "INSERT INTO accelerator_claims \
             (hardware_token, backend, worker_id, boot_id, supervisor_pid, \
              supervisor_start_ticks, process_group_id, capacity, claimed_at) \
             VALUES (?, 'nvidia', ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.hardware_token)
        .bind(i64_from_u64(input.worker_id.0))
        .bind(&input.boot_id)
        .bind(i64::from(input.supervisor_pid))
        .bind(i64_from_u64(input.supervisor_start_ticks))
        .bind(i64::from(input.process_group_id))
        .bind(i64::from(input.capacity))
        .bind(claimed_at)
        .execute(&mut **tx)
        .await;
        if let Err(error) = result {
            if self.get_in_tx(tx, &input.hardware_token).await?.is_some() {
                return Err(VoomError::Conflict(format!(
                    "accelerator `{}` is already claimed",
                    input.hardware_token
                )));
            }
            return Err(VoomError::database_context(
                "accelerator_claims insert",
                error,
            ));
        }
        Ok(AcceleratorClaim {
            hardware_token: input.hardware_token,
            worker_id: input.worker_id,
            boot_id: input.boot_id,
            supervisor_pid: input.supervisor_pid,
            supervisor_start_ticks: input.supervisor_start_ticks,
            process_group_id: input.process_group_id,
            capacity: input.capacity,
            claimed_at: input.claimed_at,
        })
    }

    pub async fn get(&self, hardware_token: &str) -> Result<Option<AcceleratorClaim>, VoomError> {
        let row = sqlx::query(
            "SELECT hardware_token, worker_id, boot_id, supervisor_pid, \
             supervisor_start_ticks, process_group_id, capacity, claimed_at \
             FROM accelerator_claims WHERE hardware_token = ?",
        )
        .bind(hardware_token)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| VoomError::database_context("accelerator_claims get", error))?;
        row.as_ref().map(row_to_claim).transpose()
    }

    async fn get_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        hardware_token: &str,
    ) -> Result<Option<AcceleratorClaim>, VoomError> {
        let row = sqlx::query(
            "SELECT hardware_token, worker_id, boot_id, supervisor_pid, \
             supervisor_start_ticks, process_group_id, capacity, claimed_at \
             FROM accelerator_claims WHERE hardware_token = ?",
        )
        .bind(hardware_token)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| VoomError::database_context("accelerator_claims get", error))?;
        row.as_ref().map(row_to_claim).transpose()
    }
}

fn row_to_claim(row: &sqlx::sqlite::SqliteRow) -> Result<AcceleratorClaim, VoomError> {
    let claimed_at: String = row
        .try_get("claimed_at")
        .map_err(|error| VoomError::database_context("accelerator_claims.claimed_at", error))?;
    let worker_id: i64 = row
        .try_get("worker_id")
        .map_err(|error| VoomError::database_context("accelerator_claims.worker_id", error))?;
    let supervisor_pid: i64 = row
        .try_get("supervisor_pid")
        .map_err(|error| VoomError::database_context("accelerator_claims.supervisor_pid", error))?;
    let start_ticks: i64 = row.try_get("supervisor_start_ticks").map_err(|error| {
        VoomError::database_context("accelerator_claims.supervisor_start_ticks", error)
    })?;
    let process_group_id: i64 = row.try_get("process_group_id").map_err(|error| {
        VoomError::database_context("accelerator_claims.process_group_id", error)
    })?;
    let capacity: i64 = row
        .try_get("capacity")
        .map_err(|error| VoomError::database_context("accelerator_claims.capacity", error))?;
    Ok(AcceleratorClaim {
        hardware_token: row.try_get("hardware_token").map_err(|error| {
            VoomError::database_context("accelerator_claims.hardware_token", error)
        })?,
        worker_id: WorkerId(u64_from_i64(worker_id)),
        boot_id: row
            .try_get("boot_id")
            .map_err(|error| VoomError::database_context("accelerator_claims.boot_id", error))?,
        supervisor_pid: u32_from_i64(supervisor_pid)?,
        supervisor_start_ticks: u64_from_i64(start_ticks),
        process_group_id: u32_from_i64(process_group_id)?,
        capacity: u32_from_i64(capacity)?,
        claimed_at: parse_iso8601(&claimed_at)?,
    })
}

#[cfg(test)]
#[path = "accelerator_claims_test.rs"]
mod tests;
