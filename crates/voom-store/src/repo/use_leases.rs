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
use super::commit_safety_gate::consult_pending_commit_lock_in_tx;
use super::common::{i64_from_u64, iso8601, map_row_err, parse_iso8601, u64_from_i64};

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

/// Read-side view of an `asset_use_leases` row.
///
/// `clock_source` is intentionally omitted from this struct: Sprint 1
/// only writes the literal `'control_plane'` value (sprint-1 design §9.2),
/// so there is nothing for callers to vary. Future sprints that add
/// non-`'control_plane'` clocks can add a `ClockSource` field then.
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

async fn begin_tx(pool: &SqlitePool) -> Result<sqlx::Transaction<'_, sqlx::Sqlite>, VoomError> {
    pool.begin()
        .await
        .map_err(|e| VoomError::Database(format!("begin: {e}")))
}

async fn commit_tx(tx: sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<(), VoomError> {
    tx.commit()
        .await
        .map_err(|e| VoomError::Database(format!("commit: {e}")))
}

/// Diagnose a single-row lifecycle UPDATE that returned no row. Runs
/// one tiny existence probe (`SELECT 1 FROM asset_use_leases WHERE id = ?`)
/// to pick between `NotFound` and `Conflict(already terminal)`. Only
/// the error path pays this round-trip — the happy path stays single-RT.
async fn diagnose_use_lease_miss(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    lease_id: UseLeaseId,
) -> VoomError {
    match sqlx::query_scalar::<_, i64>("SELECT 1 FROM asset_use_leases WHERE id = ?")
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
    {
        Ok(Some(_)) => VoomError::Conflict(format!("use_lease {lease_id} already terminal")),
        Ok(None) => VoomError::NotFound(format!("use_lease {lease_id} not found")),
        Err(e) => VoomError::Database(format!("asset_use_leases probe: {e}")),
    }
}

/// Column list for `UPDATE asset_use_leases ... RETURNING <cols>` in
/// the lifecycle methods. Mirrors the SELECT projection used by `get`
/// / `list_for_scope` so `row_to_use_lease` can decode the row uniformly.
const USE_LEASE_RETURNING_COLS: &str = "id, kind, scope_asset_id, scope_bundle_id, scope_version_id, \
     scope_location_id, issuer_kind, issuer_ref, blocking_mode, \
     ttl_bound, acquired_at, expires_at, last_heartbeat_at, \
     release_reason, released_at, epoch";

/// Maximum rows touched by a single `expire_due_in_tx` /
/// `reanchor_on_move_in_tx` call. Bounds transaction size, memory
/// allocation, and lock-hold time. The Sprint 6+ daemon loops until
/// the report is empty; under steady state each tick stays well under
/// the cap. The chosen value is conservative; if production data shows
/// it's too small (or too large) the Sprint 6+ daemon spec can promote
/// it to a policy-driven configuration knob.
pub const USE_LEASE_BATCH_LIMIT: i64 = 1000;

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
// `UseLeaseRepo` impl
// ============================================================================

/// One-probe scope liveness lookup used by `acquire_in_tx`. Reads
/// `retired_at` directly (where the column exists) so the caller
/// can distinguish three outcomes from a single round-trip:
///
/// - `Ok(None)`            — row absent → `NotFound`
/// - `Ok(Some(None))`      — row exists and is live → proceed
/// - `Ok(Some(Some(_)))`   — row exists and is retired → `Conflict`
///
/// `asset_bundles` has no `retired_at` column in M2; existence
/// alone is liveness, so the bundle arm maps `Some(_)` to
/// `Some(None)` to keep the outer match exhaustive.
async fn probe_scope_liveness(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: LeaseScope,
) -> Result<Option<Option<String>>, VoomError> {
    let err =
        |e: sqlx::Error| VoomError::Database(format!("use_lease acquire liveness check: {e}"));
    let id_arg = i64_from_u64(scope.id_u64());
    let sql = match scope {
        LeaseScope::Asset(_) => "SELECT retired_at FROM file_assets WHERE id = ?",
        LeaseScope::Version(_) => "SELECT retired_at FROM file_versions WHERE id = ?",
        LeaseScope::Location(_) => "SELECT retired_at FROM file_locations WHERE id = ?",
        LeaseScope::Bundle(_) => {
            return sqlx::query_scalar::<_, i64>("SELECT 1 FROM asset_bundles WHERE id = ?")
                .bind(id_arg)
                .fetch_optional(&mut **tx)
                .await
                .map_err(err)
                .map(|opt| opt.map(|_| None));
        }
    };
    sqlx::query_scalar::<_, Option<String>>(sql)
        .bind(id_arg)
        .fetch_optional(&mut **tx)
        .await
        .map_err(err)
}

#[async_trait]
impl UseLeaseRepo for SqliteUseLeaseRepo {
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
        // Each scope value is bound twice (IS NOT NULL probe + equality match) — keep
        // the WHERE arms and the .bind() sequence in sync if you edit this.
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

    async fn acquire_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        input: NewUseLease,
    ) -> Result<UseLease, VoomError> {
        // 1) Validate TTL vs manual-lock invariant. (§9.2 acquire step 1.)
        let is_manual = matches!(input.kind, UseLeaseKind::ManualLock);
        match (is_manual, input.ttl) {
            (true, Some(_)) => {
                return Err(VoomError::Config(
                    "manual locks must not carry a TTL".to_owned(),
                ));
            }
            (false, None) => {
                return Err(VoomError::Config(format!(
                    "TTL-bound lease kind {:?} requires a positive TTL",
                    input.kind
                )));
            }
            (false, Some(ttl)) if ttl <= Duration::ZERO => {
                return Err(VoomError::Config(format!(
                    "TTL must be positive; got {ttl}"
                )));
            }
            _ => {}
        }

        // 2) Single-probe scope liveness check (see `probe_scope_liveness`).
        match probe_scope_liveness(tx, input.scope).await? {
            Some(None) => {}
            Some(Some(_)) => {
                return Err(VoomError::Conflict(format!(
                    "use_lease scope {} {} is retired",
                    input.scope.type_str(),
                    input.scope.id_u64()
                )));
            }
            None => {
                return Err(VoomError::NotFound(format!(
                    "use_lease scope {} {} not found",
                    input.scope.type_str(),
                    input.scope.id_u64()
                )));
            }
        }

        // Pending-commit lock (sprint-1 design §9.2, M3 Phase 2 commit 5):
        // reject if any in-flight `commit_intents` row (state IN
        // ('pending','authorized')) covers `input.scope`. Helper lives in
        // `commit_safety_gate` as the single source of truth so the
        // `AliasAttached` retrofit and the gate itself read against the
        // same column-driven query (M3 sequencing doc §5.1).
        if let Some((commit_id, offending_scope)) =
            consult_pending_commit_lock_in_tx(tx, &input.scope).await?
        {
            return Err(VoomError::Conflict(format!(
                "use_lease scope {} {} blocked by in-flight commit {} on offending scope {} {}",
                input.scope.type_str(),
                input.scope.id_u64(),
                commit_id,
                offending_scope.type_str(),
                offending_scope.id_u64(),
            )));
        }

        // 3) Insert. `clock_source = 'control_plane'` is the only Sprint 1
        //    value. Manual locks have NULL `expires_at`.
        let (sa, sb, sv, sl) = scope_bind_columns(input.scope);
        let acquired_iso = iso8601(input.acquired_at)?;
        let expires_iso = input
            .ttl
            .map(|ttl| iso8601(input.acquired_at + ttl))
            .transpose()?;
        let ttl_bound_int: i64 = i64::from(!is_manual);

        let res = sqlx::query(
            "INSERT INTO asset_use_leases ( \
                kind, scope_asset_id, scope_bundle_id, scope_version_id, scope_location_id, \
                issuer_kind, issuer_ref, blocking_mode, ttl_bound, acquired_at, expires_at, \
                clock_source \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'control_plane')",
        )
        .bind(input.kind.as_str())
        .bind(sa)
        .bind(sb)
        .bind(sv)
        .bind(sl)
        .bind(input.issuer_kind.as_str())
        .bind(&input.issuer_ref)
        .bind(input.blocking_mode.as_str())
        .bind(ttl_bound_int)
        .bind(&acquired_iso)
        .bind(&expires_iso)
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases insert: {e}")))?;

        // 4) Construct the return value directly (no post-write re-read needed —
        //    every field is deterministic from input + the rowid).
        Ok(UseLease {
            id: UseLeaseId(u64_from_i64(res.last_insert_rowid())),
            kind: input.kind,
            scope: input.scope,
            issuer_kind: input.issuer_kind,
            issuer_ref: input.issuer_ref,
            blocking_mode: input.blocking_mode,
            ttl_bound: !is_manual,
            acquired_at: input.acquired_at,
            expires_at: input.ttl.map(|ttl| input.acquired_at + ttl),
            last_heartbeat_at: None,
            release_reason: None,
            released_at: None,
            epoch: 0,
        })
    }

    async fn acquire(&self, input: NewUseLease) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self.acquire_in_tx(&mut tx, input).await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn heartbeat_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        // Read the row inside the same tx (per project rule
        // `_in_tx` re-reads use the tx handle).
        let row = sqlx::query(
            "SELECT id, kind, scope_asset_id, scope_bundle_id, scope_version_id, \
                    scope_location_id, issuer_kind, issuer_ref, blocking_mode, \
                    ttl_bound, acquired_at, expires_at, last_heartbeat_at, \
                    release_reason, released_at, epoch \
             FROM asset_use_leases WHERE id = ?",
        )
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases heartbeat read: {e}")))?
        .ok_or_else(|| VoomError::NotFound(format!("use_lease {lease_id} not found")))?;

        let existing = row_to_use_lease(&row)?;
        if existing.release_reason.is_some() {
            return Err(VoomError::Conflict(format!(
                "use_lease {lease_id} is already terminal"
            )));
        }
        if !existing.ttl_bound {
            return Err(VoomError::Conflict(
                "manual locks do not heartbeat".to_owned(),
            ));
        }
        // Derive TTL from the anchor that produced the current expires_at.
        // On a freshly-acquired lease the anchor is `acquired_at`; after each
        // heartbeat the anchor advances to `last_heartbeat_at`. Anchoring on
        // `acquired_at` directly would inflate the TTL on every heartbeat
        // (60s → 90s → 150s …) because `expires_at` already moved forward.
        let original_expires = existing.expires_at.ok_or_else(|| {
            VoomError::Database(
                "TTL-bound lease missing expires_at — schema CHECK should have caught this"
                    .to_owned(),
            )
        })?;
        let anchor = existing.last_heartbeat_at.unwrap_or(existing.acquired_at);
        let ttl = original_expires - anchor;
        let new_expires = now + ttl;

        let new_expires_iso = iso8601(new_expires)?;
        let now_iso = iso8601(now)?;

        let res = sqlx::query(
            "UPDATE asset_use_leases \
             SET last_heartbeat_at = ?, expires_at = ?, epoch = epoch + 1 \
             WHERE id = ? AND epoch = ? AND release_reason IS NULL",
        )
        .bind(&now_iso)
        .bind(&new_expires_iso)
        .bind(i64_from_u64(lease_id.0))
        .bind(i64_from_u64(existing.epoch))
        .execute(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases heartbeat update: {e}")))?;
        if res.rows_affected() == 0 {
            return Err(VoomError::Conflict(format!(
                "use_lease {lease_id} concurrent modification"
            )));
        }

        Ok(UseLease {
            last_heartbeat_at: Some(now),
            expires_at: Some(new_expires),
            epoch: existing.epoch + 1,
            ..existing
        })
    }

    async fn heartbeat(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self.heartbeat_in_tx(&mut tx, lease_id, now).await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        reason: UseLeaseReleaseReason,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        // §9.2: only the issuer-driven release reasons are accepted on
        // this path. The other terminal reasons have dedicated paths.
        if !matches!(
            reason,
            UseLeaseReleaseReason::Released | UseLeaseReleaseReason::Superseded
        ) {
            return Err(VoomError::Config(format!(
                "UseLeaseRepo::release accepts Released or Superseded only; got {reason:?}"
            )));
        }

        let now_iso = iso8601(now)?;
        let row = sqlx::query(&format!(
            "UPDATE asset_use_leases \
              SET release_reason = ?, released_at = ?, epoch = epoch + 1 \
              WHERE id = ? AND release_reason IS NULL \
            RETURNING {USE_LEASE_RETURNING_COLS}"
        ))
        .bind(reason.as_str())
        .bind(&now_iso)
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases release: {e}")))?;
        match row.as_ref().map(row_to_use_lease).transpose()? {
            Some(lease) => Ok(lease),
            None => Err(diagnose_use_lease_miss(tx, lease_id).await),
        }
    }

    async fn release(
        &self,
        lease_id: UseLeaseId,
        reason: UseLeaseReleaseReason,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self.release_in_tx(&mut tx, lease_id, reason, now).await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn force_release_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let now_iso = iso8601(now)?;
        let row = sqlx::query(&format!(
            "UPDATE asset_use_leases \
              SET release_reason = 'force_released', released_at = ?, epoch = epoch + 1 \
              WHERE id = ? AND release_reason IS NULL \
            RETURNING {USE_LEASE_RETURNING_COLS}"
        ))
        .bind(&now_iso)
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases force_release: {e}")))?;
        match row.as_ref().map(row_to_use_lease).transpose()? {
            Some(lease) => Ok(lease),
            None => Err(diagnose_use_lease_miss(tx, lease_id).await),
        }
    }

    async fn force_release(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self.force_release_in_tx(&mut tx, lease_id, now).await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn expire_due_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        now: OffsetDateTime,
    ) -> Result<ExpireReport, VoomError> {
        let now_iso = iso8601(now)?;
        // Bounded batch: pick up to `USE_LEASE_BATCH_LIMIT` overdue rows
        // per call so the transaction's lock-hold time and memory use
        // stay bounded under daemon-restart backlogs. The Sprint 6+
        // daemon loops until the report is empty.
        let rows = sqlx::query(
            "UPDATE asset_use_leases \
              SET release_reason = 'expired', released_at = ?, epoch = epoch + 1 \
              WHERE id IN ( \
                  SELECT id FROM asset_use_leases \
                   WHERE release_reason IS NULL \
                     AND ttl_bound = 1 \
                     AND expires_at < ? \
                   ORDER BY id ASC \
                   LIMIT ? \
              ) \
            RETURNING id",
        )
        .bind(&now_iso)
        .bind(&now_iso)
        .bind(USE_LEASE_BATCH_LIMIT)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases expire update: {e}")))?;
        let expired: Vec<UseLeaseId> = rows
            .iter()
            .map(|r| -> Result<UseLeaseId, VoomError> {
                let id: i64 = r
                    .try_get("id")
                    .map_err(|e| map_row_err("asset_use_leases", &e))?;
                Ok(UseLeaseId(u64_from_i64(id)))
            })
            .collect::<Result<_, _>>()?;
        Ok(ExpireReport { expired })
    }

    async fn expire_due(&self, now: OffsetDateTime) -> Result<ExpireReport, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self.expire_due_in_tx(&mut tx, now).await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn recover_stale_issuer_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        // Manual locks only: gate the UPDATE on `ttl_bound = 0` so the
        // common success path is one round-trip via RETURNING. The error
        // branch falls back to a single diagnostic SELECT to pick between
        // NotFound, Conflict (already terminal), and Config (TTL-bound).
        let now_iso = iso8601(now)?;
        let row = sqlx::query(&format!(
            "UPDATE asset_use_leases \
              SET release_reason = 'issuer_lost', released_at = ?, epoch = epoch + 1 \
              WHERE id = ? AND release_reason IS NULL AND ttl_bound = 0 \
            RETURNING {USE_LEASE_RETURNING_COLS}"
        ))
        .bind(&now_iso)
        .bind(i64_from_u64(lease_id.0))
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases recover update: {e}")))?;
        if let Some(lease) = row.as_ref().map(row_to_use_lease).transpose()? {
            return Ok(lease);
        }
        // Disambiguate: read both columns in one query.
        let probe =
            sqlx::query("SELECT ttl_bound, release_reason FROM asset_use_leases WHERE id = ?")
                .bind(i64_from_u64(lease_id.0))
                .fetch_optional(&mut **tx)
                .await
                .map_err(|e| VoomError::Database(format!("asset_use_leases recover probe: {e}")))?;
        let Some(probe) = probe else {
            return Err(VoomError::NotFound(format!(
                "use_lease {lease_id} not found"
            )));
        };
        let ttl_bound: i64 = probe
            .try_get("ttl_bound")
            .map_err(|e| map_row_err("asset_use_leases", &e))?;
        let release_reason: Option<String> = probe
            .try_get("release_reason")
            .map_err(|e| map_row_err("asset_use_leases", &e))?;
        if release_reason.is_some() {
            Err(VoomError::Conflict(format!(
                "use_lease {lease_id} already terminal"
            )))
        } else if ttl_bound != 0 {
            Err(VoomError::Config(
                "recover_stale_issuer applies to manual locks only".to_owned(),
            ))
        } else {
            // Live + manual, but the gated UPDATE matched zero rows.
            // Only a concurrent writer can land us here.
            Err(VoomError::Conflict(format!(
                "use_lease {lease_id} concurrent modification"
            )))
        }
    }

    async fn recover_stale_issuer(
        &self,
        lease_id: UseLeaseId,
        now: OffsetDateTime,
    ) -> Result<UseLease, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self
            .recover_stale_issuer_in_tx(&mut tx, lease_id, now)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }

    async fn reanchor_on_move_in_tx<'tx>(
        &self,
        tx: &mut sqlx::Transaction<'tx, sqlx::Sqlite>,
        retired: FileLocationId,
        new: FileLocationId,
        _now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError> {
        // `retired == new` is a contractual no-op. Skip the SQL so
        // drain-loop callers don't spin forever: the candidate scan
        // matches `scope_location_id = retired`, and an update that
        // sets `scope_location_id = retired` leaves every row still
        // matching the filter, so each iteration would re-pick the
        // same batch.
        if retired == new {
            return Ok(ReanchorReport {
                reanchored: Vec::new(),
            });
        }
        // Bounded batch: see `expire_due_in_tx` for the rationale. The
        // Sprint 6+ daemon spec is responsible for looping until the
        // report is empty.
        let rows = sqlx::query(
            "UPDATE asset_use_leases \
              SET scope_location_id = ?, epoch = epoch + 1 \
              WHERE id IN ( \
                  SELECT id FROM asset_use_leases \
                   WHERE scope_location_id = ? AND release_reason IS NULL \
                   ORDER BY id ASC \
                   LIMIT ? \
              ) \
            RETURNING id",
        )
        .bind(i64_from_u64(new.0))
        .bind(i64_from_u64(retired.0))
        .bind(USE_LEASE_BATCH_LIMIT)
        .fetch_all(&mut **tx)
        .await
        .map_err(|e| VoomError::Database(format!("asset_use_leases reanchor update: {e}")))?;
        let reanchored: Vec<UseLeaseId> = rows
            .iter()
            .map(|r| -> Result<UseLeaseId, VoomError> {
                let id: i64 = r
                    .try_get("id")
                    .map_err(|e| map_row_err("asset_use_leases", &e))?;
                Ok(UseLeaseId(u64_from_i64(id)))
            })
            .collect::<Result<_, _>>()?;
        Ok(ReanchorReport { reanchored })
    }

    async fn reanchor_on_move(
        &self,
        retired: FileLocationId,
        new: FileLocationId,
        now: OffsetDateTime,
    ) -> Result<ReanchorReport, VoomError> {
        let mut tx = begin_tx(&self.pool).await?;
        let out = self
            .reanchor_on_move_in_tx(&mut tx, retired, new, now)
            .await?;
        commit_tx(tx).await?;
        Ok(out)
    }
}

#[cfg(test)]
#[path = "use_leases_test.rs"]
mod tests;
