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
pub static MIGRATOR: LazyLock<Migrator> = LazyLock::new(|| Migrator {
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
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});
