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

/// Migration 0038 (physical version 3): fenced artifact commit intents
/// (issue #422, ADR 0074). Adds `artifact_commit_intents` behind a
/// preflight guard on non-terminal legacy commit records; see the file
/// header.
const MIGRATION_0038_SQL: &str =
    include_str!("../../../migrations/0038_artifact_commit_intents.sql");

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
            Cow::Borrowed("artifact_commit_intents"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0038_SQL),
            false,
        ),
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});
