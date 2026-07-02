//! `SqliteSafetyPolicyRepo` — durable safety policy records (Sprint 17, T12,
//! #281).
//!
//! The safety policy is the fail-closed gate every future automation decision
//! consults (design doc -> Security And Safety). `compliance execute` reads it
//! at the hooks that already exist; the daemon will read the same rows. Shape,
//! field semantics, and the fail-closed staleness contract: `docs/adr/0028`.
//!
//! `auto_execute_operations` and `allowed_commit_modes` are stored as JSON arrays
//! of scalar enum wire-strings. The typed `New`/row structs make an unknown
//! token unrepresentable on write; the row decoder rejects one on read
//! (fail-loud) so the DB never yields an invalid value.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::{OperationKind, VoomError};

use super::Repository;
use super::common::{iso8601, parse_iso8601, u32_from_i64, u64_from_i64};

/// Version stamped into every row this binary writes. The safety gate treats a
/// row whose `schema_version` differs from this as stale and blocks (fail-closed,
/// ADR 0028): a field a newer binary requires can never be silently defaulted.
pub const SAFETY_POLICY_SCHEMA_VERSION: u32 = 1;

/// A commit mode a policy may permit. `add_only` is the only mode the execute
/// path produces today; replace/delete/archive are reserved for the destructive
/// automation the safety model gates (design doc -> Security And Safety).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    AddOnly,
    Replace,
    Delete,
    Archive,
}

impl CommitMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AddOnly => "add_only",
            Self::Replace => "replace",
            Self::Delete => "delete",
            Self::Archive => "archive",
        }
    }

    pub const ALL: &'static [Self] = &[Self::AddOnly, Self::Replace, Self::Delete, Self::Archive];

    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.as_str() == token)
    }
}

/// Verification level a policy requires before commit. V1 enforcement is
/// presence-only (`none` vs. any) because plan verify nodes do not yet carry a
/// level; the three values are kept for spec fidelity and forward compatibility
/// (ADR 0028).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationLevel {
    None,
    QuickDecode,
    Full,
}

impl VerificationLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::QuickDecode => "quick_decode",
            Self::Full => "full",
        }
    }

    pub const ALL: &'static [Self] = &[Self::None, Self::QuickDecode, Self::Full];

    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == token)
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        Self::from_wire(s).ok_or_else(|| {
            VoomError::database(format!(
                "safety_policies.verification_level {s:?} not in vocab"
            ))
        })
    }
}

/// Mutable fields of a safety policy, supplied on create and full-replace
/// update. `slug` is the stable key; update matches on it and never renames.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four booleans are independent spec-mandated safety toggles (backup/approval \
              required, block on failed/recovery-required records), not a state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSafetyPolicy {
    pub slug: String,
    pub display_name: String,
    pub auto_execute_operations: Vec<OperationKind>,
    pub backup_required: bool,
    pub approval_required: bool,
    pub allowed_commit_modes: Vec<CommitMode>,
    pub verification_level: VerificationLevel,
    pub block_on_failed_records: bool,
    pub block_on_recovery_required_records: bool,
}

/// A durable safety policy row.
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors NewSafetyPolicy: four independent spec-mandated safety toggles, not a \
              state machine"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyPolicy {
    pub id: u64,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub auto_execute_operations: Vec<OperationKind>,
    pub backup_required: bool,
    pub approval_required: bool,
    pub allowed_commit_modes: Vec<CommitMode>,
    pub verification_level: VerificationLevel,
    pub block_on_failed_records: bool,
    pub block_on_recovery_required_records: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl SafetyPolicy {
    /// `true` when this row's `schema_version` matches the reading binary's
    /// current version. A stale row is fail-closed by the safety gate.
    #[must_use]
    pub const fn is_current_schema(&self) -> bool {
        self.schema_version == SAFETY_POLICY_SCHEMA_VERSION
    }

    /// `true` when `operation` is in this policy's auto-execute allowlist.
    #[must_use]
    pub fn allows_auto_execute(&self, operation: OperationKind) -> bool {
        self.auto_execute_operations.contains(&operation)
    }

    /// `true` when `mode` is in this policy's allowed commit modes.
    #[must_use]
    pub fn allows_commit_mode(&self, mode: CommitMode) -> bool {
        self.allowed_commit_modes.contains(&mode)
    }
}

#[derive(Debug, Clone)]
pub struct SqliteSafetyPolicyRepo {
    pool: SqlitePool,
}

impl SqliteSafetyPolicyRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSafetyPolicyRepo {}

const COLS: &str = "id, slug, display_name, schema_version, auto_execute_operations, \
    backup_required, approval_required, allowed_commit_modes, verification_level, \
    block_on_failed_records, block_on_recovery_required_records, created_at, updated_at";

impl SqliteSafetyPolicyRepo {
    /// Insert a new safety policy. Rejects a duplicate `slug` with
    /// [`VoomError::Conflict`].
    pub async fn create(
        &self,
        input: NewSafetyPolicy,
        now: OffsetDateTime,
    ) -> Result<SafetyPolicy, VoomError> {
        let ts = iso8601(now)?;
        let operations = encode_operations(&input.auto_execute_operations)?;
        let modes = encode_commit_modes(&input.allowed_commit_modes)?;
        let res = sqlx::query(
            "INSERT INTO safety_policies \
             (slug, display_name, schema_version, auto_execute_operations, backup_required, \
              approval_required, allowed_commit_modes, verification_level, \
              block_on_failed_records, block_on_recovery_required_records, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.slug)
        .bind(&input.display_name)
        .bind(i64::from(SAFETY_POLICY_SCHEMA_VERSION))
        .bind(&operations)
        .bind(i64::from(input.backup_required))
        .bind(i64::from(input.approval_required))
        .bind(&modes)
        .bind(input.verification_level.as_str())
        .bind(i64::from(input.block_on_failed_records))
        .bind(i64::from(input.block_on_recovery_required_records))
        .bind(&ts)
        .bind(&ts)
        .execute(&self.pool)
        .await;
        match res {
            Ok(res) => Ok(row_from_input(
                u64_from_i64(res.last_insert_rowid()),
                input,
                now,
            )),
            Err(err) => Err(self.classify_insert_error(&input.slug, err).await),
        }
    }

    async fn classify_insert_error(&self, slug: &str, err: sqlx::Error) -> VoomError {
        match self.get_by_slug(slug).await {
            Ok(Some(_)) => {
                VoomError::Conflict(format!("safety policy slug {slug:?} already exists"))
            }
            _ => VoomError::database_context("safety_policies create", err),
        }
    }

    pub async fn get_by_slug(&self, slug: &str) -> Result<Option<SafetyPolicy>, VoomError> {
        let row = sqlx::query(&format!(
            "SELECT {COLS} FROM safety_policies WHERE slug = ?"
        ))
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("safety_policies get_by_slug", e))?;
        row.as_ref().map(row_to_safety_policy).transpose()
    }

    pub async fn list(&self) -> Result<Vec<SafetyPolicy>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM safety_policies ORDER BY slug ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("safety_policies list", e))?;
        rows.iter().map(row_to_safety_policy).collect()
    }

    /// Full-replace update keyed by `input.slug`. Re-stamps `schema_version` and
    /// `updated_at`, preserves `id` / `created_at`. Returns `None` when no row
    /// has that slug.
    pub async fn update(
        &self,
        input: NewSafetyPolicy,
        now: OffsetDateTime,
    ) -> Result<Option<SafetyPolicy>, VoomError> {
        let ts = iso8601(now)?;
        let operations = encode_operations(&input.auto_execute_operations)?;
        let modes = encode_commit_modes(&input.allowed_commit_modes)?;
        let affected = sqlx::query(
            "UPDATE safety_policies \
             SET display_name = ?, schema_version = ?, auto_execute_operations = ?, \
                 backup_required = ?, approval_required = ?, allowed_commit_modes = ?, \
                 verification_level = ?, block_on_failed_records = ?, \
                 block_on_recovery_required_records = ?, updated_at = ? \
             WHERE slug = ?",
        )
        .bind(&input.display_name)
        .bind(i64::from(SAFETY_POLICY_SCHEMA_VERSION))
        .bind(&operations)
        .bind(i64::from(input.backup_required))
        .bind(i64::from(input.approval_required))
        .bind(&modes)
        .bind(input.verification_level.as_str())
        .bind(i64::from(input.block_on_failed_records))
        .bind(i64::from(input.block_on_recovery_required_records))
        .bind(&ts)
        .bind(&input.slug)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("safety_policies update", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_by_slug(&input.slug).await
    }

    /// Delete by slug. Returns `true` when a row was removed.
    pub async fn delete(&self, slug: &str) -> Result<bool, VoomError> {
        let affected = sqlx::query("DELETE FROM safety_policies WHERE slug = ?")
            .bind(slug)
            .execute(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("safety_policies delete", e))?
            .rows_affected();
        Ok(affected > 0)
    }
}

fn encode_operations(operations: &[OperationKind]) -> Result<String, VoomError> {
    let tokens: Vec<&str> = operations.iter().map(|o| o.as_str()).collect();
    serde_json::to_string(&tokens)
        .map_err(|e| VoomError::Internal(format!("encode auto_execute_operations: {e}")))
}

fn encode_commit_modes(modes: &[CommitMode]) -> Result<String, VoomError> {
    let tokens: Vec<&str> = modes.iter().map(|m| m.as_str()).collect();
    serde_json::to_string(&tokens)
        .map_err(|e| VoomError::Internal(format!("encode allowed_commit_modes: {e}")))
}

fn decode_operations(json: &str) -> Result<Vec<OperationKind>, VoomError> {
    let tokens: Vec<String> = serde_json::from_str(json).map_err(|e| {
        VoomError::database(format!(
            "safety_policies.auto_execute_operations decode: {e}"
        ))
    })?;
    tokens
        .iter()
        .map(|t| {
            OperationKind::from_wire(t).ok_or_else(|| {
                VoomError::database(format!(
                    "safety_policies.auto_execute_operations {t:?} not in vocab"
                ))
            })
        })
        .collect()
}

fn decode_commit_modes(json: &str) -> Result<Vec<CommitMode>, VoomError> {
    let tokens: Vec<String> = serde_json::from_str(json).map_err(|e| {
        VoomError::database(format!("safety_policies.allowed_commit_modes decode: {e}"))
    })?;
    tokens
        .iter()
        .map(|t| {
            CommitMode::from_wire(t).ok_or_else(|| {
                VoomError::database(format!(
                    "safety_policies.allowed_commit_modes {t:?} not in vocab"
                ))
            })
        })
        .collect()
}

/// Build the in-memory row a successful insert/update produced, without a
/// re-read: the durable columns are exactly the validated input plus the id,
/// current schema version, and timestamps.
fn row_from_input(id: u64, input: NewSafetyPolicy, now: OffsetDateTime) -> SafetyPolicy {
    SafetyPolicy {
        id,
        slug: input.slug,
        display_name: input.display_name,
        schema_version: SAFETY_POLICY_SCHEMA_VERSION,
        auto_execute_operations: input.auto_execute_operations,
        backup_required: input.backup_required,
        approval_required: input.approval_required,
        allowed_commit_modes: input.allowed_commit_modes,
        verification_level: input.verification_level,
        block_on_failed_records: input.block_on_failed_records,
        block_on_recovery_required_records: input.block_on_recovery_required_records,
        created_at: now,
        updated_at: now,
    }
}

fn row_to_safety_policy(row: &SqliteRow) -> Result<SafetyPolicy, VoomError> {
    let t = "safety_policies";
    let map = |field: &'static str| {
        move |e: sqlx::Error| VoomError::database_context(format!("{t}.{field}"), e)
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let schema_version: i64 = row
        .try_get("schema_version")
        .map_err(map("schema_version"))?;
    let operations: String = row
        .try_get("auto_execute_operations")
        .map_err(map("auto_execute_operations"))?;
    let modes: String = row
        .try_get("allowed_commit_modes")
        .map_err(map("allowed_commit_modes"))?;
    let verification_level: String = row
        .try_get("verification_level")
        .map_err(map("verification_level"))?;
    let backup_required: i64 = row
        .try_get("backup_required")
        .map_err(map("backup_required"))?;
    let approval_required: i64 = row
        .try_get("approval_required")
        .map_err(map("approval_required"))?;
    let block_on_failed_records: i64 = row
        .try_get("block_on_failed_records")
        .map_err(map("block_on_failed_records"))?;
    let block_on_recovery_required_records: i64 = row
        .try_get("block_on_recovery_required_records")
        .map_err(map("block_on_recovery_required_records"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let updated_at: String = row.try_get("updated_at").map_err(map("updated_at"))?;
    Ok(SafetyPolicy {
        id: u64_from_i64(id),
        slug: row.try_get("slug").map_err(map("slug"))?,
        display_name: row.try_get("display_name").map_err(map("display_name"))?,
        schema_version: u32_from_i64(schema_version)?,
        auto_execute_operations: decode_operations(&operations)?,
        backup_required: backup_required != 0,
        approval_required: approval_required != 0,
        allowed_commit_modes: decode_commit_modes(&modes)?,
        verification_level: VerificationLevel::parse(&verification_level)?,
        block_on_failed_records: block_on_failed_records != 0,
        block_on_recovery_required_records: block_on_recovery_required_records != 0,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "safety_policies_test.rs"]
mod tests;
