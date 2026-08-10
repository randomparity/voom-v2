use std::borrow::Cow;
use std::sync::LazyLock;

use sqlx::migrate::{Migration, MigrationType, Migrator};

/// SQL for migration 0001, embedded at compile time.
const MIGRATION_0001_SQL: &str = include_str!("../../../migrations/0001_init.sql");

/// SQL for migration 0002 (M1 durable execution + events), embedded at
/// compile time.
const MIGRATION_0002_SQL: &str = include_str!("../../../migrations/0002_durable_execution.sql");

/// SQL for migration 0003 (M2 identity & bundles), embedded at compile time.
const MIGRATION_0003_SQL: &str = include_str!("../../../migrations/0003_identity.sql");

/// SQL for migration 0004 (M3 use leases + commit gate + ancillary registries),
/// embedded at compile time.
const MIGRATION_0004_SQL: &str = include_str!("../../../migrations/0004_use_leases_ancillary.sql");

/// SQL for migration 0005 (M3 Phase 2 commit-intent persistent permit +
/// `recovery_reason` column), embedded at compile time.
const MIGRATION_0005_SQL: &str =
    include_str!("../../../migrations/0005_commit_intents_persistent_permit.sql");

/// SQL for migration 0006 (Sprint 3 policy input persistence), embedded at
/// compile time.
const MIGRATION_0006_SQL: &str = include_str!("../../../migrations/0006_policy_inputs.sql");

/// SQL for migration 0007 (Sprint 4 policy registry), embedded at compile time.
const MIGRATION_0007_SQL: &str = include_str!("../../../migrations/0007_policy_registry.sql");

/// SQL for migration 0008 (Sprint 6 issue dedupe key), embedded at compile time.
const MIGRATION_0008_SQL: &str = include_str!("../../../migrations/0008_issue_dedupe_key.sql");

/// SQL for migration 0009 (Sprint 7 node registry), embedded at compile time.
const MIGRATION_0009_SQL: &str = include_str!("../../../migrations/0009_nodes.sql");

/// SQL for migration 0010 (Sprint 8 remote execution persistence), embedded
/// at compile time.
const MIGRATION_0010_SQL: &str = include_str!("../../../migrations/0010_remote_execution.sql");

/// SQL for migration 0011 (Sprint 9 scheduler decision persistence), embedded
/// at compile time.
const MIGRATION_0011_SQL: &str = include_str!("../../../migrations/0011_scheduler_decisions.sql");

/// SQL for migration 0012 (Sprint 11 staged artifact commit), embedded at
/// compile time.
const MIGRATION_0012_SQL: &str =
    include_str!("../../../migrations/0012_staged_artifact_commit.sql");

/// SQL for migration 0013 (Sprint 14 audio sidecar support), embedded at
/// compile time.
const MIGRATION_0013_SQL: &str = include_str!("../../../migrations/0013_audio_sidecar_support.sql");

/// SQL for migration 0014 (Sprint 15 video profiles seed), embedded at
/// compile time.
const MIGRATION_0014_SQL: &str = include_str!("../../../migrations/0014_video_profiles.sql");

/// SQL for migration 0015 (Sprint 16 durable workflow summaries), embedded at
/// compile time.
const MIGRATION_0015_SQL: &str = include_str!("../../../migrations/0015_workflow_summaries.sql");

/// SQL for migration 0016 (worker grant `max_parallel` wildcard normalization),
/// embedded at compile time.
const MIGRATION_0016_SQL: &str =
    include_str!("../../../migrations/0016_worker_grant_max_parallel_wildcard.sql");

/// SQL for migration 0017 (scan hardlink inode facts, #249), embedded at
/// compile time.
const MIGRATION_0017_SQL: &str = include_str!("../../../migrations/0017_scan_file_facts.sql");

/// SQL for migration 0018 (Sprint 17 durable backup records), embedded at
/// compile time.
const MIGRATION_0018_SQL: &str = include_str!("../../../migrations/0018_backups.sql");

/// SQL for migration 0019 (Sprint 17 library + library-root config, T11),
/// embedded at compile time.
const MIGRATION_0019_SQL: &str = include_str!("../../../migrations/0019_libraries.sql");

/// SQL for migration 0020 (Sprint 17 scheduling + safety policy CRUD, #281),
/// embedded at compile time.
const MIGRATION_0020_SQL: &str =
    include_str!("../../../migrations/0020_scheduling_safety_policies.sql");

/// SQL for migration 0021 (Sprint 17 profile management support columns:
/// video-profile soft-retire + per-library scoring default, #285), embedded at
/// compile time.
const MIGRATION_0021_SQL: &str = include_str!("../../../migrations/0021_profile_management.sql");

/// SQL for migration 0022 (per-file phase-barrier run starts, #329), embedded
/// at compile time.
const MIGRATION_0022_SQL: &str =
    include_str!("../../../migrations/0022_workflow_file_run_starts.sql");

/// SQL for migration 0023 (inherited per-file phase outcomes, #330), embedded
/// at compile time.
const MIGRATION_0023_SQL: &str =
    include_str!("../../../migrations/0023_workflow_file_run_history.sql");

/// SQL for migration 0024 (atomic plural audio extraction, #337), embedded at
/// compile time.
const MIGRATION_0024_SQL: &str =
    include_str!("../../../migrations/0024_atomic_audio_extract_operations.sql");

/// SQL for migration 0025 (recoverable audio synthesis, #333), embedded at
/// compile time.
const MIGRATION_0025_SQL: &str =
    include_str!("../../../migrations/0025_recoverable_audio_synthesis.sql");

/// SQL for migration 0026 (policy-driven artifact verification, #334),
/// embedded at compile time.
const MIGRATION_0026_SQL: &str =
    include_str!("../../../migrations/0026_policy_artifact_verification.sql");

/// SQL for migration 0027 (sequential audio-synthesis asset lineage, #338),
/// embedded at compile time.
const MIGRATION_0027_SQL: &str =
    include_str!("../../../migrations/0027_audio_synthesis_asset_lineage.sql");

/// SQL for migration 0028 (durable sliding file window, #401), embedded at
/// compile time.
const MIGRATION_0028_SQL: &str = include_str!("../../../migrations/0028_sliding_file_window.sql");

/// SQL for migration 0029 (NVIDIA video acceleration, #400), embedded at
/// compile time.
const MIGRATION_0029_SQL: &str =
    include_str!("../../../migrations/0029_nvidia_video_acceleration.sql");

/// SQL for migration 0030 (`VideoToolbox` profile vocabulary, #411), embedded at
/// compile time.
const MIGRATION_0030_SQL: &str =
    include_str!("../../../migrations/0030_videotoolbox_video_profiles.sql");

/// SQL for migration 0031 (backend-neutral accelerator claims, #411), embedded
/// at compile time.
const MIGRATION_0031_SQL: &str =
    include_str!("../../../migrations/0031_backend_neutral_accelerator_claims.sql");

/// SQL for migration 0032 (VAAPI video acceleration, #409), embedded at
/// compile time.
const MIGRATION_0032_SQL: &str =
    include_str!("../../../migrations/0032_vaapi_video_acceleration.sql");

/// SQL for migration 0033 (canonical remote acquire replay shape), embedded at
/// compile time.
const MIGRATION_0033_SQL: &str =
    include_str!("../../../migrations/0033_remote_acquire_replay_shape.sql");

/// SQL for migration 0034 (node-owned roots and relative file locations), embedded at
/// compile time.
const MIGRATION_0034_SQL: &str = include_str!("../../../migrations/0034_node_owned_roots.sql");

/// SQL for migration 0035 (node process incarnations and worker ownership), embedded at
/// compile time.
const MIGRATION_0035_SQL: &str = include_str!("../../../migrations/0035_node_incarnations.sql");

/// SQL for migration 0036 (durable scan sessions), embedded at compile time.
const MIGRATION_0036_SQL: &str = include_str!("../../../migrations/0036_scan_sessions.sql");

/// Embedded migration set, constructed without the `sqlx::migrate!` macro.
///
/// We don't use sqlx's `macros` feature: it pulls `sqlx-macros-core`, which
/// hard-depends on `sqlx-mysql` → `rsa` (RUSTSEC-2023-0071, no upstream fix).
/// Avoiding `macros` keeps the dependency graph minimal and lets us drop the
/// advisory ignore. The runtime types (`Migration`, `MigrationType`,
/// `Migrator`) live behind the `migrate` feature, which we still enable.
///
/// `Migration::new` computes the same SHA-384 checksum the macro would,
/// keeping checksum semantics identical for `probe_schema`'s drift detection.
///
/// Single source of truth for "what schema does this binary expect" — both
/// `init()` and `probe_schema()` read from here.
pub(crate) static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| Migrator {
    migrations: Cow::Owned(vec![
        Migration::new(
            1,
            Cow::Borrowed("init"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0001_SQL),
            false,
        ),
        Migration::new(
            2,
            Cow::Borrowed("durable_execution"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0002_SQL),
            false,
        ),
        Migration::new(
            3,
            Cow::Borrowed("identity"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0003_SQL),
            false,
        ),
        Migration::new(
            4,
            Cow::Borrowed("use_leases_ancillary"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0004_SQL),
            false,
        ),
        Migration::new(
            5,
            Cow::Borrowed("commit_intents_persistent_permit"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0005_SQL),
            false,
        ),
        Migration::new(
            6,
            Cow::Borrowed("policy_inputs"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0006_SQL),
            false,
        ),
        Migration::new(
            7,
            Cow::Borrowed("policy_registry"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0007_SQL),
            false,
        ),
        Migration::new(
            8,
            Cow::Borrowed("issue_dedupe_key"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0008_SQL),
            false,
        ),
        Migration::new(
            9,
            Cow::Borrowed("nodes"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0009_SQL),
            false,
        ),
        Migration::new(
            10,
            Cow::Borrowed("remote_execution"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0010_SQL),
            false,
        ),
        Migration::new(
            11,
            Cow::Borrowed("scheduler_decisions"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0011_SQL),
            false,
        ),
        Migration::new(
            12,
            Cow::Borrowed("staged_artifact_commit"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0012_SQL),
            false,
        ),
        Migration::new(
            13,
            Cow::Borrowed("audio_sidecar_support"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0013_SQL),
            false,
        ),
        Migration::new(
            14,
            Cow::Borrowed("video_profiles"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0014_SQL),
            false,
        ),
        Migration::new(
            15,
            Cow::Borrowed("workflow_summaries"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0015_SQL),
            false,
        ),
        Migration::new(
            16,
            Cow::Borrowed("worker_grant_max_parallel_wildcard"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0016_SQL),
            false,
        ),
        Migration::new(
            17,
            Cow::Borrowed("scan_file_facts"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0017_SQL),
            false,
        ),
        Migration::new(
            18,
            Cow::Borrowed("backups"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0018_SQL),
            false,
        ),
        Migration::new(
            19,
            Cow::Borrowed("libraries"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0019_SQL),
            false,
        ),
        Migration::new(
            20,
            Cow::Borrowed("scheduling_safety_policies"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0020_SQL),
            false,
        ),
        Migration::new(
            21,
            Cow::Borrowed("profile_management"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0021_SQL),
            false,
        ),
        Migration::new(
            22,
            Cow::Borrowed("workflow_file_run_starts"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0022_SQL),
            false,
        ),
        Migration::new(
            23,
            Cow::Borrowed("workflow_file_run_history"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0023_SQL),
            false,
        ),
        Migration::new(
            24,
            Cow::Borrowed("atomic_audio_extract_operations"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0024_SQL),
            false,
        ),
        Migration::new(
            25,
            Cow::Borrowed("recoverable_audio_synthesis"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0025_SQL),
            false,
        ),
        Migration::new(
            26,
            Cow::Borrowed("policy_artifact_verification"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0026_SQL),
            false,
        ),
        Migration::new(
            27,
            Cow::Borrowed("audio_synthesis_asset_lineage"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0027_SQL),
            false,
        ),
        Migration::new(
            28,
            Cow::Borrowed("sliding_file_window"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0028_SQL),
            false,
        ),
        Migration::new(
            29,
            Cow::Borrowed("nvidia_video_acceleration"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0029_SQL),
            false,
        ),
        Migration::new(
            30,
            Cow::Borrowed("videotoolbox_video_profiles"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0030_SQL),
            false,
        ),
        Migration::new(
            31,
            Cow::Borrowed("backend_neutral_accelerator_claims"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0031_SQL),
            false,
        ),
        Migration::new(
            32,
            Cow::Borrowed("vaapi_video_acceleration"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0032_SQL),
            false,
        ),
        Migration::new(
            33,
            Cow::Borrowed("remote_acquire_replay_shape"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0033_SQL),
            false,
        ),
        Migration::new(
            34,
            Cow::Borrowed("node_owned_roots"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0034_SQL),
            false,
        ),
        Migration::new(
            35,
            Cow::Borrowed("node_incarnations"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0035_SQL),
            false,
        ),
        Migration::new(
            36,
            Cow::Borrowed("scan_sessions"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0036_SQL),
            false,
        ),
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});
