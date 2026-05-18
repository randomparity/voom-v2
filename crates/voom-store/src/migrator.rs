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
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});
