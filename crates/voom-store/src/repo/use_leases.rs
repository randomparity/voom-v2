//! `UseLeaseRepo` — owns `asset_use_leases` (M3 Phase 1).
//!
//! Lifecycle per sprint-1 design §9.2. Each write method comes in two
//! forms (`_in_tx` primitive + bare wrapper); event emission belongs
//! at the `ControlPlane` layer (`crates/voom-control-plane/src/cases/use_leases.rs`).
//!
//! The pending-commit-lock query in `acquire_in_tx` is deferred to M3
//! Phase 2 (the lock targets `commit_intent_scope_members`, which has
//! no rows until the commit safety gate writes them). Marked inline
//! with `// TODO(m3-phase-2)` below.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};
use time::{Duration, OffsetDateTime};
use voom_core::{BundleId, FileAssetId, FileLocationId, FileVersionId, UseLeaseId, VoomError};

use super::Repository;
use super::common::{i64_from_u64, map_row_err, parse_iso8601, u64_from_i64};

// ============================================================================
// Domain enums (CHECK-constraint mirrors)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseLeaseKind {
    Playback,
    Scan,
    Copy,
    ManualLock,
    ExternalLock,
    WorkerOperation,
}

impl UseLeaseKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Playback => "playback",
            Self::Scan => "scan",
            Self::Copy => "copy",
            Self::ManualLock => "manual_lock",
            Self::ExternalLock => "external_lock",
            Self::WorkerOperation => "worker_operation",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "playback" => Ok(Self::Playback),
            "scan" => Ok(Self::Scan),
            "copy" => Ok(Self::Copy),
            "manual_lock" => Ok(Self::ManualLock),
            "external_lock" => Ok(Self::ExternalLock),
            "worker_operation" => Ok(Self::WorkerOperation),
            other => Err(VoomError::Database(format!(
                "asset_use_leases.kind {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssuerKind {
    User,
    ControlPlane,
    Worker,
    ExternalSystem,
}

impl IssuerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::ControlPlane => "control_plane",
            Self::Worker => "worker",
            Self::ExternalSystem => "external_system",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "user" => Ok(Self::User),
            "control_plane" => Ok(Self::ControlPlane),
            "worker" => Ok(Self::Worker),
            "external_system" => Ok(Self::ExternalSystem),
            other => Err(VoomError::Database(format!(
                "asset_use_leases.issuer_kind {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockingMode {
    Blocking,
    Advisory,
}

impl BlockingMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Advisory => "advisory",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "blocking" => Ok(Self::Blocking),
            "advisory" => Ok(Self::Advisory),
            other => Err(VoomError::Database(format!(
                "asset_use_leases.blocking_mode {other:?} not in vocab"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseLeaseReleaseReason {
    Released,
    Expired,
    IssuerLost,
    Superseded,
    ForceReleased,
}

impl UseLeaseReleaseReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Expired => "expired",
            Self::IssuerLost => "issuer_lost",
            Self::Superseded => "superseded",
            Self::ForceReleased => "force_released",
        }
    }

    fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "released" => Ok(Self::Released),
            "expired" => Ok(Self::Expired),
            "issuer_lost" => Ok(Self::IssuerLost),
            "superseded" => Ok(Self::Superseded),
            "force_released" => Ok(Self::ForceReleased),
            other => Err(VoomError::Database(format!(
                "asset_use_leases.release_reason {other:?} not in vocab"
            ))),
        }
    }
}

// ============================================================================
// `LeaseScope` enum — Rust-side mirror of the four `scope_*_id` columns
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseScope {
    Asset(FileAssetId),
    Bundle(BundleId),
    Version(FileVersionId),
    Location(FileLocationId),
}

impl LeaseScope {
    /// Wire-format string for events and the CLI. Mirrors §6.1 / §11.1.
    #[must_use]
    pub const fn type_str(self) -> &'static str {
        match self {
            Self::Asset(_) => "asset",
            Self::Bundle(_) => "bundle",
            Self::Version(_) => "version",
            Self::Location(_) => "location",
        }
    }

    #[must_use]
    pub const fn id_u64(self) -> u64 {
        match self {
            Self::Asset(id) => id.0,
            Self::Bundle(id) => id.0,
            Self::Version(id) => id.0,
            Self::Location(id) => id.0,
        }
    }
}

// ============================================================================
// Input/output structs
// ============================================================================

#[derive(Debug, Clone)]
pub struct NewUseLease {
    pub kind: UseLeaseKind,
    pub scope: LeaseScope,
    pub issuer_kind: IssuerKind,
    pub issuer_ref: String,
    pub blocking_mode: BlockingMode,
    /// `Some(d)` for TTL-bound leases; `None` for manual locks.
    pub ttl: Option<Duration>,
    pub acquired_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct UseLease {
    pub id: UseLeaseId,
    pub kind: UseLeaseKind,
    pub scope: LeaseScope,
    pub issuer_kind: IssuerKind,
    pub issuer_ref: String,
    pub blocking_mode: BlockingMode,
    pub ttl_bound: bool,
    pub acquired_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
    pub last_heartbeat_at: Option<OffsetDateTime>,
    pub release_reason: Option<UseLeaseReleaseReason>,
    pub released_at: Option<OffsetDateTime>,
    pub epoch: u64,
}

impl UseLease {
    /// Returns `true` for leases not yet in a terminal `release_reason` state.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.release_reason.is_none()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExpireReport {
    pub expired: Vec<UseLeaseId>,
}

#[derive(Debug, Clone, Default)]
pub struct ReanchorReport {
    pub reanchored: Vec<UseLeaseId>,
}

// ============================================================================
// `UseLeaseRepo` trait
// ============================================================================

#[async_trait]
pub trait UseLeaseRepo: Repository {
    // --- write methods (one pair per lifecycle entry) ----------------------

    async fn acquire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewUseLease,
    ) -> Result<UseLease, VoomError>;

    async fn acquire(&self, input: NewUseLease) -> Result<UseLease, VoomError>;

    async fn heartbeat_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn heartbeat(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        reason: UseLeaseReleaseReason,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn release(
        &self,
        lease_id: UseLeaseId,
        reason: UseLeaseReleaseReason,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn force_release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn force_release(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn expire_due_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError>;

    async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError>;

    async fn recover_stale_issuer_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn recover_stale_issuer(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError>;

    async fn reanchor_on_move_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        retired: FileLocationId,
        new: FileLocationId,
        now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError>;

    async fn reanchor_on_move(
        &self,
        retired: FileLocationId,
        new: FileLocationId,
        now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError>;

    // --- read methods -----------------------------------------------------

    async fn get(&self, id: UseLeaseId) -> Result<Option<UseLease>, VoomError>;

    async fn list_for_scope(&self, scope: LeaseScope) -> Result<Vec<UseLease>, VoomError>;
}

// ============================================================================
// `SqliteUseLeaseRepo`
// ============================================================================

#[derive(Debug, Clone)]
pub struct SqliteUseLeaseRepo {
    pool: SqlitePool,
}

impl SqliteUseLeaseRepo {
    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl Repository for SqliteUseLeaseRepo {}

// ----- shared helpers -------------------------------------------------------

// Tasks 6–12 call begin_tx / commit_tx in their bare-wrapper bodies.
#[expect(dead_code, reason = "used by Tasks 6-12 write-method wrappers")]
async fn begin_tx(pool: &SqlitePool) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::Database(format!("begin: {e}")))
}

#[expect(dead_code, reason = "used by Tasks 6-12 write-method wrappers")]
async fn commit_tx(tx: sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("commit: {e}")))
}

/// Decode a single `asset_use_leases` row into `UseLease`. Used by every
/// read path and every `_in_tx` post-write re-read.
fn row_to_use_lease(row: &sqlx::sqlite::SqliteRow) -> Result<UseLease, VoomError> {
    let id: i64 = row
        .try_get("id")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let kind: String = row
        .try_get("kind")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let scope_asset: Option<i64> = row
        .try_get("scope_asset_id")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let scope_bundle: Option<i64> = row
        .try_get("scope_bundle_id")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let scope_version: Option<i64> = row
        .try_get("scope_version_id")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let scope_location: Option<i64> = row
        .try_get("scope_location_id")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let scope = match (scope_asset, scope_bundle, scope_version, scope_location) {
        (Some(v), None, None, None) => LeaseScope::Asset(FileAssetId(u64_from_i64(v))),
        (None, Some(v), None, None) => LeaseScope::Bundle(BundleId(u64_from_i64(v))),
        (None, None, Some(v), None) => LeaseScope::Version(FileVersionId(u64_from_i64(v))),
        (None, None, None, Some(v)) => LeaseScope::Location(FileLocationId(u64_from_i64(v))),
        _ => {
            return Err(VoomError::Database(
                "asset_use_leases row violates one-of scope_*_id invariant".to_owned(),
            ));
        }
    };
    let issuer_kind: String = row
        .try_get("issuer_kind")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let issuer_ref: String = row
        .try_get("issuer_ref")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let blocking_mode: String = row
        .try_get("blocking_mode")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let ttl_bound_int: i64 = row
        .try_get("ttl_bound")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let acquired_at: String = row
        .try_get("acquired_at")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let expires_at: Option<String> = row
        .try_get("expires_at")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let last_heartbeat_at: Option<String> = row
        .try_get("last_heartbeat_at")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let release_reason: Option<String> = row
        .try_get("release_reason")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let released_at: Option<String> = row
        .try_get("released_at")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;
    let epoch: i64 = row
        .try_get("epoch")
        .map_err(|e| map_row_err("asset_use_leases", &e))?;

    Ok(UseLease {
        id: UseLeaseId(u64_from_i64(id)),
        kind: UseLeaseKind::parse(&kind)?,
        scope,
        issuer_kind: IssuerKind::parse(&issuer_kind)?,
        issuer_ref,
        blocking_mode: BlockingMode::parse(&blocking_mode)?,
        ttl_bound: ttl_bound_int != 0,
        acquired_at: parse_iso8601(&acquired_at)?,
        expires_at: expires_at.as_deref().map(parse_iso8601).transpose()?,
        last_heartbeat_at: last_heartbeat_at
            .as_deref()
            .map(parse_iso8601)
            .transpose()?,
        release_reason: release_reason
            .as_deref()
            .map(UseLeaseReleaseReason::parse)
            .transpose()?,
        released_at: released_at.as_deref().map(parse_iso8601).transpose()?,
        epoch: u64_from_i64(epoch),
    })
}

/// `(scope_asset, scope_bundle, scope_version, scope_location)` tuple for
/// binding the four FK columns from a `LeaseScope`.
const fn scope_bind_columns(
    scope: LeaseScope,
) -> (Option<i64>, Option<i64>, Option<i64>, Option<i64>) {
    match scope {
        LeaseScope::Asset(id) => (Some(i64_from_u64(id.0)), None, None, None),
        LeaseScope::Bundle(id) => (None, Some(i64_from_u64(id.0)), None, None),
        LeaseScope::Version(id) => (None, None, Some(i64_from_u64(id.0)), None),
        LeaseScope::Location(id) => (None, None, None, Some(i64_from_u64(id.0))),
    }
}

// ============================================================================
// `UseLeaseRepo` impl — read methods only in this task; writes follow in
// Tasks 6–12.
// ============================================================================

#[async_trait]
impl UseLeaseRepo for SqliteUseLeaseRepo {
    // Read methods (implemented in this task) ---------------------------------

    async fn get(&self, id: UseLeaseId) -> Result<Option<UseLease>, VoomError> {
        let row = sqlx::query(
            "SELECT id, kind, scope_asset_id, scope_bundle_id, scope_version_id, \
                    scope_location_id, issuer_kind, issuer_ref, blocking_mode, \
                    ttl_bound, acquired_at, expires_at, last_heartbeat_at, \
                    release_reason, released_at, epoch \
             FROM asset_use_leases WHERE id = ?",
        )
        .bind(i64_from_u64(id.0))
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases get: {e}")))?;
        row.as_ref().map(row_to_use_lease).transpose()
    }

    async fn list_for_scope(&self, scope: LeaseScope) -> Result<Vec<UseLease>, VoomError> {
        let (a, b, v, l) = scope_bind_columns(scope);
        let rows = sqlx::query(
            "SELECT id, kind, scope_asset_id, scope_bundle_id, scope_version_id, \
                    scope_location_id, issuer_kind, issuer_ref, blocking_mode, \
                    ttl_bound, acquired_at, expires_at, last_heartbeat_at, \
                    release_reason, released_at, epoch \
             FROM asset_use_leases \
             WHERE (? IS NOT NULL AND scope_asset_id    = ?) \
                OR (? IS NOT NULL AND scope_bundle_id   = ?) \
                OR (? IS NOT NULL AND scope_version_id  = ?) \
                OR (? IS NOT NULL AND scope_location_id = ?) \
             ORDER BY id ASC",
        )
        .bind(a)
        .bind(a)
        .bind(b)
        .bind(b)
        .bind(v)
        .bind(v)
        .bind(l)
        .bind(l)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases list_for_scope: {e}")))?;
        rows.iter().map(row_to_use_lease).collect()
    }

    // Write methods — STUBS until Tasks 6–12 implement them. Each stub
    // returns `VoomError::Internal("not implemented in Phase 1 Task N")`
    // so the compiler sees the trait as satisfied. The corresponding
    // task body replaces the stub.

    async fn acquire_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _input: NewUseLease,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "acquire_in_tx not implemented yet (Task 6)".to_owned(),
        ))
    }

    async fn acquire(&self, _input: NewUseLease) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "acquire not implemented yet (Task 6)".to_owned(),
        ))
    }

    async fn heartbeat_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "heartbeat_in_tx not implemented yet (Task 7)".to_owned(),
        ))
    }

    async fn heartbeat(
        &self,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "heartbeat not implemented yet (Task 7)".to_owned(),
        ))
    }

    async fn release_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _lease_id: UseLeaseId,
        _reason: UseLeaseReleaseReason,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "release_in_tx not implemented yet (Task 8)".to_owned(),
        ))
    }

    async fn release(
        &self,
        _lease_id: UseLeaseId,
        _reason: UseLeaseReleaseReason,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "release not implemented yet (Task 8)".to_owned(),
        ))
    }

    async fn force_release_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "force_release_in_tx not implemented yet (Task 9)".to_owned(),
        ))
    }

    async fn force_release(
        &self,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "force_release not implemented yet (Task 9)".to_owned(),
        ))
    }

    async fn expire_due_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError> {
        Err(VoomError::Internal(
            "expire_due_in_tx not implemented yet (Task 10)".to_owned(),
        ))
    }

    async fn expire_due(&self, _now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        Err(VoomError::Internal(
            "expire_due not implemented yet (Task 10)".to_owned(),
        ))
    }

    async fn recover_stale_issuer_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "recover_stale_issuer_in_tx not implemented yet (Task 11)".to_owned(),
        ))
    }

    async fn recover_stale_issuer(
        &self,
        _lease_id: UseLeaseId,
        _now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        Err(VoomError::Internal(
            "recover_stale_issuer not implemented yet (Task 11)".to_owned(),
        ))
    }

    async fn reanchor_on_move_in_tx<'tx>(
        &self,
        _tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        _retired: FileLocationId,
        _new: FileLocationId,
        _now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError> {
        Err(VoomError::Internal(
            "reanchor_on_move_in_tx not implemented yet (Task 12)".to_owned(),
        ))
    }

    async fn reanchor_on_move(
        &self,
        _retired: FileLocationId,
        _new: FileLocationId,
        _now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError> {
        Err(VoomError::Internal(
            "reanchor_on_move not implemented yet (Task 12)".to_owned(),
        ))
    }
}

#[cfg(test)]
#[path = "use_leases_test.rs"]
mod tests;
