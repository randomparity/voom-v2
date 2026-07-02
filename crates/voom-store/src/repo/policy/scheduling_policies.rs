//! `SqliteSchedulingPolicyRepo` — durable scheduling policy records (Sprint 17,
//! T12, #281).
//!
//! Scheduling policy is named, slug-keyed configuration a future daemon reads
//! rather than invents (design doc -> Policy Model). It has no reader yet; this
//! repo provides CRUD only. Shape and rationale: `docs/adr/0028`.

use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use voom_core::VoomError;

use super::Repository;
use super::common::{iso8601, parse_iso8601, u32_from_i64, u64_from_i64};

/// Version stamped into every row this binary writes. The daemon-era reader
/// treats a row whose version differs as stale (fail-closed); scheduling policy
/// has no reader yet, but the column is populated for forward compatibility with
/// the safety-policy staleness contract (ADR 0028).
pub const SCHEDULING_POLICY_SCHEMA_VERSION: u32 = 1;

/// Job-ordering preference. Mirrors the `scheduling_policies.priority` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulePriority {
    NewestFirst,
    OldestFirst,
    SmallestFirst,
    LargestFirst,
}

impl SchedulePriority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NewestFirst => "newest_first",
            Self::OldestFirst => "oldest_first",
            Self::SmallestFirst => "smallest_first",
            Self::LargestFirst => "largest_first",
        }
    }

    /// All variants, for CLI value enumeration and exhaustive tests.
    pub const ALL: &'static [Self] = &[
        Self::NewestFirst,
        Self::OldestFirst,
        Self::SmallestFirst,
        Self::LargestFirst,
    ];

    #[must_use]
    pub fn from_wire(token: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.as_str() == token)
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        Self::from_wire(s).ok_or_else(|| {
            VoomError::database(format!("scheduling_policies.priority {s:?} not in vocab"))
        })
    }
}

/// Mutable fields of a scheduling policy, supplied on create and full-replace
/// update. `slug` is the stable key; update matches on it and never renames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSchedulingPolicy {
    pub slug: String,
    pub display_name: String,
    pub priority: SchedulePriority,
    pub copy_window: Option<String>,
    pub large_jobs_night_only: bool,
    pub pause_on_degraded_node: bool,
}

/// A durable scheduling policy row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulingPolicy {
    pub id: u64,
    pub slug: String,
    pub display_name: String,
    pub schema_version: u32,
    pub priority: SchedulePriority,
    pub copy_window: Option<String>,
    pub large_jobs_night_only: bool,
    pub pause_on_degraded_node: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct SqliteSchedulingPolicyRepo {
    pool: SqlitePool,
}

impl SqliteSchedulingPolicyRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteSchedulingPolicyRepo {}

const COLS: &str = "id, slug, display_name, schema_version, priority, copy_window, \
    large_jobs_night_only, pause_on_degraded_node, created_at, updated_at";

impl SqliteSchedulingPolicyRepo {
    /// Insert a new scheduling policy. Rejects a duplicate `slug` with
    /// [`VoomError::Conflict`] and an invalid `copy_window` with
    /// [`VoomError::Config`].
    pub async fn create(
        &self,
        input: NewSchedulingPolicy,
        now: OffsetDateTime,
    ) -> Result<SchedulingPolicy, VoomError> {
        validate_copy_window(input.copy_window.as_deref())?;
        let ts = iso8601(now)?;
        let res = sqlx::query(
            "INSERT INTO scheduling_policies \
             (slug, display_name, schema_version, priority, copy_window, \
              large_jobs_night_only, pause_on_degraded_node, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&input.slug)
        .bind(&input.display_name)
        .bind(i64::from(SCHEDULING_POLICY_SCHEMA_VERSION))
        .bind(input.priority.as_str())
        .bind(input.copy_window.as_deref())
        .bind(i64::from(input.large_jobs_night_only))
        .bind(i64::from(input.pause_on_degraded_node))
        .bind(&ts)
        .bind(&ts)
        .execute(&self.pool)
        .await;
        match res {
            Ok(res) => Ok(SchedulingPolicy {
                id: u64_from_i64(res.last_insert_rowid()),
                slug: input.slug,
                display_name: input.display_name,
                schema_version: SCHEDULING_POLICY_SCHEMA_VERSION,
                priority: input.priority,
                copy_window: input.copy_window,
                large_jobs_night_only: input.large_jobs_night_only,
                pause_on_degraded_node: input.pause_on_degraded_node,
                created_at: now,
                updated_at: now,
            }),
            Err(err) => Err(self.classify_insert_error(&input.slug, err).await),
        }
    }

    async fn classify_insert_error(&self, slug: &str, err: sqlx::Error) -> VoomError {
        match self.get_by_slug(slug).await {
            Ok(Some(_)) => VoomError::Conflict(format!(
                "scheduling policy slug {slug:?} already exists"
            )),
            _ => VoomError::database_context("scheduling_policies create", err),
        }
    }

    pub async fn get_by_slug(&self, slug: &str) -> Result<Option<SchedulingPolicy>, VoomError> {
        let row = sqlx::query(&format!("SELECT {COLS} FROM scheduling_policies WHERE slug = ?"))
            .bind(slug)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("scheduling_policies get_by_slug", e))?;
        row.as_ref().map(row_to_scheduling_policy).transpose()
    }

    pub async fn list(&self) -> Result<Vec<SchedulingPolicy>, VoomError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLS} FROM scheduling_policies ORDER BY slug ASC"
        ))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("scheduling_policies list", e))?;
        rows.iter().map(row_to_scheduling_policy).collect()
    }

    /// Full-replace update keyed by `input.slug`. Re-stamps `schema_version` and
    /// `updated_at`, preserves `id` / `created_at`. Returns `None` when no row
    /// has that slug. Rejects an invalid `copy_window` with [`VoomError::Config`].
    pub async fn update(
        &self,
        input: NewSchedulingPolicy,
        now: OffsetDateTime,
    ) -> Result<Option<SchedulingPolicy>, VoomError> {
        validate_copy_window(input.copy_window.as_deref())?;
        let ts = iso8601(now)?;
        let affected = sqlx::query(
            "UPDATE scheduling_policies \
             SET display_name = ?, schema_version = ?, priority = ?, copy_window = ?, \
                 large_jobs_night_only = ?, pause_on_degraded_node = ?, updated_at = ? \
             WHERE slug = ?",
        )
        .bind(&input.display_name)
        .bind(i64::from(SCHEDULING_POLICY_SCHEMA_VERSION))
        .bind(input.priority.as_str())
        .bind(input.copy_window.as_deref())
        .bind(i64::from(input.large_jobs_night_only))
        .bind(i64::from(input.pause_on_degraded_node))
        .bind(&ts)
        .bind(&input.slug)
        .execute(&self.pool)
        .await
        .map_err(|e| VoomError::database_context("scheduling_policies update", e))?
        .rows_affected();
        if affected == 0 {
            return Ok(None);
        }
        self.get_by_slug(&input.slug).await
    }

    /// Delete by slug. Returns `true` when a row was removed.
    pub async fn delete(&self, slug: &str) -> Result<bool, VoomError> {
        let affected = sqlx::query("DELETE FROM scheduling_policies WHERE slug = ?")
            .bind(slug)
            .execute(&self.pool)
            .await
            .map_err(|e| VoomError::database_context("scheduling_policies delete", e))?
            .rows_affected();
        Ok(affected > 0)
    }
}

/// Validate an `HH:MM-HH:MM` copy window. `None` is always valid (no window).
fn validate_copy_window(window: Option<&str>) -> Result<(), VoomError> {
    let Some(window) = window else {
        return Ok(());
    };
    let reject = || {
        VoomError::Config(format!(
            "copy_window {window:?} must be HH:MM-HH:MM (24-hour), e.g. 00:00-08:00"
        ))
    };
    let (start, end) = window.split_once('-').ok_or_else(reject)?;
    validate_hh_mm(start).ok_or_else(reject)?;
    validate_hh_mm(end).ok_or_else(reject)?;
    Ok(())
}

/// `Some(())` when `s` is a 24-hour `HH:MM` clock time.
fn validate_hh_mm(s: &str) -> Option<()> {
    let (hh, mm) = s.split_once(':')?;
    if hh.len() != 2 || mm.len() != 2 {
        return None;
    }
    let hours: u8 = hh.parse().ok()?;
    let minutes: u8 = mm.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(())
}

fn row_to_scheduling_policy(row: &SqliteRow) -> Result<SchedulingPolicy, VoomError> {
    let t = "scheduling_policies";
    let map = |field: &'static str| {
        move |e: sqlx::Error| VoomError::database_context(format!("{t}.{field}"), e)
    };
    let id: i64 = row.try_get("id").map_err(map("id"))?;
    let schema_version: i64 = row.try_get("schema_version").map_err(map("schema_version"))?;
    let priority: String = row.try_get("priority").map_err(map("priority"))?;
    let large_jobs_night_only: i64 = row
        .try_get("large_jobs_night_only")
        .map_err(map("large_jobs_night_only"))?;
    let pause_on_degraded_node: i64 = row
        .try_get("pause_on_degraded_node")
        .map_err(map("pause_on_degraded_node"))?;
    let created_at: String = row.try_get("created_at").map_err(map("created_at"))?;
    let updated_at: String = row.try_get("updated_at").map_err(map("updated_at"))?;
    Ok(SchedulingPolicy {
        id: u64_from_i64(id),
        slug: row.try_get("slug").map_err(map("slug"))?,
        display_name: row.try_get("display_name").map_err(map("display_name"))?,
        schema_version: u32_from_i64(schema_version)?,
        priority: SchedulePriority::parse(&priority)?,
        copy_window: row.try_get("copy_window").map_err(map("copy_window"))?,
        large_jobs_night_only: large_jobs_night_only != 0,
        pause_on_degraded_node: pause_on_degraded_node != 0,
        created_at: parse_iso8601(&created_at)?,
        updated_at: parse_iso8601(&updated_at)?,
    })
}

#[cfg(test)]
#[path = "scheduling_policies_test.rs"]
mod tests;
