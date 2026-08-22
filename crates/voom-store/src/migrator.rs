use std::borrow::Cow;
use std::sync::LazyLock;

use sqlx::migrate::{Migration, MigrationType, Migrator};

/// SQL for migration 0001, embedded at compile time. The complete schema:
/// see `migrations/0001_schema.sql` for why this is a single migration
/// rather than the sequential chain it replaced.
const MIGRATION_0001_SQL: &str = include_str!("../../../migrations/0001_schema.sql");

/// Migration 0037 (physical version 2): owner-local scheduling evidence
/// (issue #477, ADR 0071). Logical numbering continues the pre-squash
/// history; see the file header for its guarded rebuild of
/// `artifact_access_plans` and the additive `scheduler_decisions` column.
const MIGRATION_0037_SQL: &str =
    include_str!("../../../migrations/0037_owner_local_scheduling_evidence.sql");

/// Migration 0041 (physical version 3): scan observation evidence
/// (issue #421, ADR 0077). Adds the nullable strict-JSON evidence payload to
/// `scan_observations`; see the file header and
/// `voom_core::ScanObservationEvidence`.
const MIGRATION_0041_SQL: &str =
    include_str!("../../../migrations/0041_scan_observation_evidence.sql");

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
            Cow::Borrowed("schema"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0001_SQL),
            false,
        ),
        Migration::new(
            2,
            Cow::Borrowed("owner_local_scheduling_evidence"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0037_SQL),
            false,
        ),
        Migration::new(
            3,
            Cow::Borrowed("scan_observation_evidence"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0041_SQL),
            false,
        ),
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});
