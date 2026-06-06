#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests favor unwrap/panic over plumbing Result<()> through every assertion"
    )
)]
//! App-services layer: wraps voom-store and exposes commands consumed by API/CLI.
//!
//! The `cases` submodule hosts the M1 use-case methods. Every method that
//! mutates durable state composes the matching repo `_in_tx` call with
//! `EventRepo::append_in_tx` inside one `pool.begin()` so the row write
//! and its event row share a transaction.

use std::sync::{Arc, Mutex};

use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::{Clock, ErrorCode, SystemClock, VoomError};
use voom_store::repo::{
    artifact_access_plans::SqliteArtifactAccessPlanRepo,
    artifacts::SqliteArtifactRepo,
    bundles::SqliteBundleRepo,
    events::SqliteEventRepo,
    identity::SqliteIdentityRepo,
    issues::SqliteIssueRepo,
    jobs::SqliteJobRepo,
    leases::SqliteLeaseRepo,
    nodes::SqliteNodeRepo,
    policies::SqlitePolicyRepo,
    policy_inputs::SqlitePolicyInputRepo,
    remote_idempotency::SqliteRemoteIdempotencyRepo,
    scheduler_decisions::{
        SchedulerDecision, SchedulerDecisionFilter, SqliteSchedulerDecisionRepo,
    },
    scheduler_node_limits::SqliteSchedulerNodeLimitRepo,
    tickets::SqliteTicketRepo,
    use_leases::SqliteUseLeaseRepo,
    video_profiles::{SqliteVideoProfileRepo, VideoProfile},
    workers::SqliteWorkerRepo,
    workflow_summaries::SqliteWorkflowSummaryRepo,
};
use voom_store::{SchemaState, connect, probe_schema};

mod artifact;
mod audio;
mod cases;
mod media_snapshot;
pub mod node_auth;
mod operation_source;
mod remux;
pub mod scan;
mod transcode;
pub(crate) mod worker_process;
mod workflow;

pub mod execution {
    pub use crate::cases::execution::remote_execution::{
        RemoteAcquireInput, RemoteAcquireOutcome, RemoteArtifactAccessPlan, RemoteCompleteInput,
        RemoteCompleteOutcome, RemoteFailInput, RemoteFailOutcome, RemoteLeaseDispatch,
        RemoteLeaseHeartbeatInput, RemoteLeaseHeartbeatOutcome, RemoteNodeHeartbeatInput,
        RemoteNodeHeartbeatOutcome, RemoteRecoverReport,
    };
}

pub mod policy {
    pub use crate::cases::policy::compliance::{
        ComplianceApplyData, ComplianceExecuteData, ComplianceExecuteError,
        ComplianceExecutionOptions, ComplianceReportData, ComplianceRunReportData,
        FilePhaseSummaryView, IssueApplicationSummary, PhaseSummaryView, WorkflowSummaryView,
    };
    pub use crate::cases::policy::policy_inputs::{
        PolicyInputFromScanInput, PolicyInputFromScanResult, WholeScanInput, WholeScanInputResult,
    };
}

pub mod workers {
    pub use crate::cases::workers::nodes::{RegisterNodeInput, RegisteredNode};
    pub use crate::cases::workers::{
        NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterWorkerForNodeInput,
    };
}

pub use artifact::{
    ArtifactDetail, ArtifactInspectionState, ArtifactListInput, ArtifactSummary,
    CommitArtifactCommandError, CommitArtifactInput, CommitArtifactPreMutationReport,
    CommitArtifactReport, CommitRecoveryReport, CommitSummary, PathFacts, PathObservation,
    RecoverySummary, StageCopyCommandError, StageCopyInput, StageCopyReport, VerificationSummary,
    VerifyArtifactInput, VerifyArtifactReport,
};
pub use audio::{
    ExecuteExtractAudioInput, ExecuteExtractAudioReport, ExecuteTranscodeAudioInput,
    ExecuteTranscodeAudioReport, ExtractAudioDispatcher, TranscodeAudioDispatcher,
    TranscodePostCommitRecoveryReport,
};
pub use cases::policy::plans::{plan_compiled_policy_with_input, plan_policy_source_with_input};
pub use remux::{ExecuteRemuxInput, ExecuteRemuxReport, RemuxDispatcher};
pub use transcode::{
    ExecuteTranscodeVideoInput, ExecuteTranscodeVideoReport, TranscodeVideoDispatcher,
};
pub use workflow::coordinator::{CoordinatorError, CoordinatorOutcome};
pub use workflow::plan::ticket_payload::WorkflowTicketPayload;

/// Type alias for the boxed, shared, interior-mutable RNG passed to
/// `SqliteLeaseRepo::fail` (and any future caller that needs full-jitter
/// backoff). `RngCore::next_u32` takes `&mut self`, so the `Arc` wraps
/// a `Mutex` to keep the `ControlPlane` itself `Clone`-able and
/// thread-safe.
pub type SharedRng = Arc<Mutex<dyn RngCore + Send>>;

#[derive(Clone)]
pub struct ControlPlane {
    pool: SqlitePool,
    clock: Arc<dyn Clock>,
    rng: SharedRng,
    pub(crate) events: SqliteEventRepo,
    pub(crate) jobs: SqliteJobRepo,
    pub(crate) tickets: SqliteTicketRepo,
    pub(crate) workers: SqliteWorkerRepo,
    pub(crate) nodes: SqliteNodeRepo,
    pub(crate) leases: SqliteLeaseRepo,
    pub(crate) remote_idempotency: SqliteRemoteIdempotencyRepo,
    pub(crate) artifact_access_plans: SqliteArtifactAccessPlanRepo,
    pub(crate) artifacts: SqliteArtifactRepo,
    pub(crate) issues: SqliteIssueRepo,
    pub(crate) identity: SqliteIdentityRepo,
    pub(crate) bundles: SqliteBundleRepo,
    pub(crate) use_leases: SqliteUseLeaseRepo,
    pub(crate) policy_inputs: SqlitePolicyInputRepo,
    pub(crate) policies: SqlitePolicyRepo,
    pub(crate) video_profiles: SqliteVideoProfileRepo,
    pub(crate) scheduler_decisions: SqliteSchedulerDecisionRepo,
    pub(crate) scheduler_node_limits: SqliteSchedulerNodeLimitRepo,
    pub(crate) workflow_summaries: SqliteWorkflowSummaryRepo,
}

impl std::fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn Clock` / `dyn RngCore` do not require Debug; surface a
        // sentinel rather than widening the trait bound (which would
        // force every concrete implementor — including test fakes —
        // to derive Debug).
        f.debug_struct("ControlPlane")
            .field("pool", &self.pool)
            .field("clock", &"<dyn Clock>")
            .field("rng", &"<dyn RngCore>")
            .field("events", &self.events)
            .field("jobs", &self.jobs)
            .field("tickets", &self.tickets)
            .field("workers", &self.workers)
            .field("nodes", &self.nodes)
            .field("leases", &self.leases)
            .field("remote_idempotency", &self.remote_idempotency)
            .field("artifact_access_plans", &self.artifact_access_plans)
            .field("artifacts", &self.artifacts)
            .field("issues", &self.issues)
            .field("identity", &self.identity)
            .field("bundles", &self.bundles)
            .field("use_leases", &self.use_leases)
            .field("policy_inputs", &self.policy_inputs)
            .field("policies", &self.policies)
            .field("video_profiles", &self.video_profiles)
            .field("scheduler_decisions", &self.scheduler_decisions)
            .field("scheduler_node_limits", &self.scheduler_node_limits)
            .field("workflow_summaries", &self.workflow_summaries)
            .finish()
    }
}

impl ControlPlane {
    /// Open an existing database. **Never creates files or directories** — if
    /// the DB doesn't exist, returns `DB_UNREACHABLE`. The CLI's `init` command
    /// is the only path that creates databases, and it calls
    /// `voom_store::init(url)` directly without going through `ControlPlane`.
    ///
    /// Requires the schema to be at [`SchemaState::Current`] because the
    /// returned plane exposes the M1 writable use cases. Diagnostic flows
    /// (`/health` on a non-Current DB) must use [`HealthPlane::open`] instead.
    ///
    /// # Errors
    /// Returns `VoomError::Database` if the pool cannot be opened. If the
    /// schema probe is not `Current` it returns the variant matching the
    /// probe state: `SchemaTooNew` for `TooNew`, `DirtyMigration` for
    /// `Dirty`, otherwise `Migration` (uninitialized / partial).
    pub async fn open(database_url: &str) -> Result<Self, VoomError> {
        let pool = connect(database_url).await?;
        Self::open_with_pool_and_rng(pool, Arc::new(SystemClock), production_rng()).await
    }

    /// Wrap an already-connected pool with the supplied clock. The DB MUST
    /// already be at the current schema (use `voom_store::init` on first boot);
    /// any other state is rejected. Use-case methods on `ControlPlane` assume
    /// the full M1 schema is present.
    ///
    /// # Errors
    /// If the schema probe is not `Current`, returns the variant matching the
    /// probe state (`SchemaTooNew`, `DirtyMigration`, or `Migration`), or
    /// whatever error `probe_schema` itself produces.
    pub async fn open_with_pool(
        pool: SqlitePool,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, VoomError> {
        Self::open_with_pool_and_rng(pool, clock, production_rng()).await
    }

    /// Wrap an already-connected pool with the supplied clock AND RNG.
    /// Tests inject `FrozenRng` / `SeededRng` from
    /// `voom_core::rng_test_support`; production callers prefer
    /// `open_with_pool` which seeds a `StdRng` from OS randomness.
    ///
    /// # Errors
    /// If the schema probe is not `Current`, returns the variant matching the
    /// probe state (`SchemaTooNew`, `DirtyMigration`, or `Migration`), or
    /// whatever error `probe_schema` itself produces.
    pub async fn open_with_pool_and_rng(
        pool: SqlitePool,
        clock: Arc<dyn Clock>,
        rng: SharedRng,
    ) -> Result<Self, VoomError> {
        let probe = probe_schema(&pool).await?;
        match probe {
            SchemaState::Current { .. } => {}
            SchemaState::TooNew { applied, expected } => {
                return Err(VoomError::SchemaTooNew(format!(
                    "DB has {applied} migrations applied but this binary only ships \
                     {expected}; upgrade the voom binary to one that knows this schema"
                )));
            }
            SchemaState::Dirty { failed_version, .. } => {
                return Err(VoomError::DirtyMigration(format!(
                    "migration version {failed_version} is recorded with success=0; \
                     remove the failed row from _sqlx_migrations or restore from backup"
                )));
            }
            SchemaState::Uninitialized => {
                return Err(VoomError::UninitializedDatabase(
                    "ControlPlane requires a Current schema; got Uninitialized".to_owned(),
                ));
            }
            SchemaState::Partial { .. } => {
                return Err(VoomError::Migration(format!(
                    "ControlPlane requires a Current schema; got {probe:?}"
                )));
            }
        }
        Ok(Self::new_unchecked(pool, clock, rng))
    }

    fn new_unchecked(pool: SqlitePool, clock: Arc<dyn Clock>, rng: SharedRng) -> Self {
        Self {
            events: SqliteEventRepo::new(pool.clone()),
            jobs: SqliteJobRepo::new(pool.clone()),
            tickets: SqliteTicketRepo::new(pool.clone()),
            workers: SqliteWorkerRepo::new(pool.clone()),
            nodes: SqliteNodeRepo::new(pool.clone()),
            leases: SqliteLeaseRepo::new(pool.clone()),
            remote_idempotency: SqliteRemoteIdempotencyRepo::new(pool.clone()),
            artifact_access_plans: SqliteArtifactAccessPlanRepo::new(pool.clone()),
            artifacts: SqliteArtifactRepo::new(pool.clone()),
            issues: SqliteIssueRepo::new(pool.clone()),
            identity: SqliteIdentityRepo::new(pool.clone()),
            bundles: SqliteBundleRepo::new(pool.clone()),
            use_leases: SqliteUseLeaseRepo::new(pool.clone()),
            policy_inputs: SqlitePolicyInputRepo::new(pool.clone()),
            policies: SqlitePolicyRepo::new(pool.clone()),
            video_profiles: SqliteVideoProfileRepo::new(pool.clone()),
            scheduler_decisions: SqliteSchedulerDecisionRepo::new(pool.clone()),
            scheduler_node_limits: SqliteSchedulerNodeLimitRepo::new(pool.clone()),
            workflow_summaries: SqliteWorkflowSummaryRepo::new(pool.clone()),
            pool,
            clock,
            rng,
        }
    }

    /// Read-only health snapshot.
    ///
    /// # Errors
    /// Propagates `probe_schema` errors.
    pub async fn health(&self) -> Result<HealthSnapshot, VoomError> {
        health_from_pool(&self.pool).await
    }

    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn pool_for_test(&self) -> &SqlitePool {
        &self.pool
    }

    /// Pull a single `u32` from the shared RNG and wrap it in a
    /// fixed-value RNG. The case handlers use this to thread a
    /// `&mut (dyn RngCore + Send)` into repo calls without holding the
    /// std Mutex across the awaits inside the repo (the workspace lint
    /// `await_holding_lock` forbids that). Each `SqliteLeaseRepo::fail` call
    /// consumes exactly one jitter value via `default_backoff`, so a
    /// single-shot snapshot is sufficient.
    pub(crate) fn snapshot_rng(&self) -> SnapshotRng {
        let mut guard = self
            .rng
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        SnapshotRng {
            value: guard.next_u32(),
        }
    }

    // Test-support accessors let integration tests seed and inspect durable
    // state directly; production code uses the fields inside case handlers.
    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn events(&self) -> &SqliteEventRepo {
        &self.events
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn tickets(&self) -> &SqliteTicketRepo {
        &self.tickets
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn workers(&self) -> &SqliteWorkerRepo {
        &self.workers
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn leases(&self) -> &SqliteLeaseRepo {
        &self.leases
    }

    /// Read one durable scheduler decision.
    ///
    /// # Errors
    /// Propagates scheduler decision repository read errors.
    pub async fn scheduler_decision(
        &self,
        id: u64,
    ) -> Result<Option<SchedulerDecision>, VoomError> {
        self.scheduler_decisions.get(id).await
    }

    /// List durable scheduler decisions through a read-only `ControlPlane`
    /// surface. Scheduler decision writes remain owned by remote acquire.
    ///
    /// # Errors
    /// Propagates scheduler decision repository read errors.
    pub async fn scheduler_decisions(
        &self,
        filter: SchedulerDecisionFilter,
    ) -> Result<Vec<SchedulerDecision>, VoomError> {
        self.scheduler_decisions.list(filter).await
    }

    /// List the seeded video encode profiles, ordered by name.
    ///
    /// The `video_profiles` registry is read-only this sprint; this surface
    /// powers `voom profile list`.
    ///
    /// # Errors
    /// Propagates video-profile repository read errors.
    pub async fn list_video_profiles(&self) -> Result<Vec<VideoProfile>, VoomError> {
        self.video_profiles.list().await
    }

    /// Look up one video encode profile by registry name.
    ///
    /// Returns `None` for an unknown name; callers map that to `NOT_FOUND`.
    ///
    /// # Errors
    /// Propagates video-profile repository read errors.
    pub async fn get_video_profile(&self, name: &str) -> Result<Option<VideoProfile>, VoomError> {
        self.video_profiles.get_by_name(name).await
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn artifact_access_plans(&self) -> &SqliteArtifactAccessPlanRepo {
        &self.artifact_access_plans
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn artifacts(&self) -> &SqliteArtifactRepo {
        &self.artifacts
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn identity(&self) -> &SqliteIdentityRepo {
        &self.identity
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn workflow_summaries(&self) -> &SqliteWorkflowSummaryRepo {
        &self.workflow_summaries
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn use_leases(&self) -> &SqliteUseLeaseRepo {
        &self.use_leases
    }

    #[cfg(any(test, feature = "test"))]
    #[must_use]
    pub fn policy_inputs(&self) -> &SqlitePolicyInputRepo {
        &self.policy_inputs
    }
}

/// Read-only handle for diagnosing a database's schema state.
///
/// Unlike [`ControlPlane`], `HealthPlane::open` does not require the
/// schema to be at [`SchemaState::Current`]; it is the surface for the
/// `/health` diagnostic flow. It exposes only `health()` — no writable
/// use cases — so a non-Current database cannot be mutated through it.
#[derive(Clone)]
pub struct HealthPlane {
    pool: SqlitePool,
}

impl std::fmt::Debug for HealthPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HealthPlane")
            .field("pool", &self.pool)
            .finish()
    }
}

impl HealthPlane {
    /// Open an existing database for read-only diagnostics. Never creates
    /// files or directories — if the DB doesn't exist, returns
    /// `DB_UNREACHABLE`.
    ///
    /// # Errors
    /// Propagates `connect` errors.
    pub async fn open(database_url: &str) -> Result<Self, VoomError> {
        let pool = connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Read-only health snapshot.
    ///
    /// # Errors
    /// Propagates `probe_schema` errors.
    pub async fn health(&self) -> Result<HealthSnapshot, VoomError> {
        health_from_pool(&self.pool).await
    }
}

async fn health_from_pool(pool: &SqlitePool) -> Result<HealthSnapshot, VoomError> {
    let schema = probe_schema(pool).await?;
    Ok(match schema {
        SchemaState::Uninitialized => HealthSnapshot::Uninitialized,
        SchemaState::Partial { applied, expected } => HealthSnapshot::Partial { applied, expected },
        SchemaState::Current {
            migration_count,
            schema_init_at,
        } => HealthSnapshot::Current {
            migration_count,
            schema_init_at,
        },
        SchemaState::TooNew { applied, expected } => HealthSnapshot::TooNew { applied, expected },
        SchemaState::Dirty {
            failed_version,
            applied,
            expected,
        } => HealthSnapshot::Dirty {
            failed_version,
            applied,
            expected,
        },
    })
}

/// State-tagged health snapshot. The ADT shape replaces the previous
/// flat-struct-with-Options so the type system enforces which fields are
/// available in each state — no more `Option<u32>` debug-printed in
/// operator-facing error messages as `Some(0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthSnapshot {
    /// `_sqlx_migrations` table absent.
    Uninitialized,
    /// Fewer migrations applied than this binary ships. Safe to rerun
    /// `voom init`.
    Partial { applied: u32, expected: u32 },
    /// Exactly as many migrations applied as this binary ships AND every
    /// applied version is known to the embedded MIGRATOR.
    Current {
        migration_count: u32,
        schema_init_at: OffsetDateTime,
    },
    /// At least one applied migration version is not in the embedded MIGRATOR.
    TooNew { applied: u32, expected: u32 },
    /// One or more migration rows are recorded as `success=0`; manual recovery
    /// required before further migrations can run.
    Dirty {
        failed_version: i64,
        applied: u32,
        expected: u32,
    },
}

/// Operator-facing diagnostic triple for a non-Current health snapshot.
/// Surfaces (API, CLI) wrap this into their own envelope format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthDiagnostic {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
}

impl HealthSnapshot {
    /// Map a non-Current snapshot to its diagnostic triple. Returns `None`
    /// for `Current` — that state has no error to surface.
    ///
    /// This is the single source of truth for the error code, message, and
    /// hint for every non-healthy state. Both `voom-api` and `voom-cli` call
    /// it so their prose cannot drift apart.
    #[must_use]
    pub fn diagnostic(&self) -> Option<HealthDiagnostic> {
        match self {
            Self::Current { .. } => None,
            Self::Uninitialized => Some(HealthDiagnostic {
                code: ErrorCode::DbUninitialized,
                message: "database has no migrations applied".to_owned(),
                hint: Some("Run `voom init` on the host that owns this database".to_owned()),
            }),
            Self::Partial { applied, expected } => Some(HealthDiagnostic {
                code: ErrorCode::DbPartialSchema,
                message: format!(
                    "database partially migrated (applied={applied}, expected={expected})"
                ),
                hint: Some("Run `voom init` against the current binary".to_owned()),
            }),
            Self::TooNew { applied, expected } => Some(HealthDiagnostic {
                code: ErrorCode::DbSchemaTooNew,
                message: format!(
                    "database has migrations this binary does not know about \
                     (applied={applied}, expected={expected})"
                ),
                hint: Some("Upgrade the server binary or roll the database back".to_owned()),
            }),
            Self::Dirty {
                failed_version,
                applied,
                expected,
            } => Some(HealthDiagnostic {
                code: ErrorCode::DbDirtyMigration,
                message: format!(
                    "a previous migration left the schema in a dirty (failed) state \
                     (failed_version={failed_version}, applied={applied}, expected={expected}); \
                     sqlx will not run further migrations until the dirty row is resolved"
                ),
                hint: Some(
                    "Manual recovery required: remove the failed row from \
                     _sqlx_migrations (e.g. DELETE FROM _sqlx_migrations WHERE \
                     version = <failed_version>) or restore from backup. Do NOT \
                     just re-run voom init — it will fail the same way."
                        .to_owned(),
                ),
            }),
        }
    }
}

/// Seed a `SharedRng` from OS randomness. Used by `ControlPlane::open`
/// and `ControlPlane::open_with_pool`; tests inject `FrozenRng` /
/// `SeededRng` via `open_with_pool_and_rng`.
fn production_rng() -> SharedRng {
    Arc::new(Mutex::new(StdRng::from_os_rng()))
}

/// Single-shot RNG that returns one fixed `u32` from every call. The
/// shape lets `ControlPlane::snapshot_rng` lift one jitter value out
/// of the shared RNG while keeping the std Mutex off the await
/// boundary — the consumer (`SqliteLeaseRepo::fail` → `default_backoff`)
/// only needs one value per call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotRng {
    value: u32,
}

impl RngCore for SnapshotRng {
    fn next_u32(&mut self) -> u32 {
        self.value
    }

    fn next_u64(&mut self) -> u64 {
        u64::from(self.value) << 32 | u64::from(self.value)
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) {
        for chunk in dst.chunks_mut(4) {
            let bytes = self.value.to_le_bytes();
            for (slot, byte) in chunk.iter_mut().zip(bytes.iter()) {
                *slot = *byte;
            }
        }
    }
}

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;
