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

/// Migration 0041 (physical version 4): scan observation evidence
/// (issue #421, ADR 0077). Adds the nullable strict-JSON evidence payload to
/// `scan_observations`; see the file header and
/// `voom_core::ScanObservationEvidence`.
const MIGRATION_0041_SQL: &str =
    include_str!("../../../migrations/0041_scan_observation_evidence.sql");

/// Migration 0042 (physical version 5): node-local location-handle media
/// dispatch preflight (issue #423, ADR 0075). Pure preflight guard aborting
/// the migration when non-terminal byte-touching media workflow tickets
/// carry payloads without the nested `media_dispatch` envelope; see the
/// file header.
const MIGRATION_0042_SQL: &str =
    include_str!("../../../migrations/0042_node_local_media_dispatch_preflight.sql");

/// Migration 0043 (physical version 6): fenced commit-intent source handle
/// (issue #423 T7, ADR 0075). Recreates `artifact_commit_intents` behind a
/// preflight guard on any existing rows, pinning the staged bytes' source
/// rooted address so the node can materialize staging itself; see the file
/// header.
const MIGRATION_0043_SQL: &str =
    include_str!("../../../migrations/0043_commit_intent_source_handle.sql");

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
        Migration::new(
            4,
            Cow::Borrowed("scan_observation_evidence"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0041_SQL),
            false,
        ),
        Migration::new(
            5,
            Cow::Borrowed("node_local_media_dispatch_preflight"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0042_SQL),
            false,
        ),
        Migration::new(
            6,
            Cow::Borrowed("commit_intent_source_handle"),
            MigrationType::Simple,
            Cow::Borrowed(MIGRATION_0043_SQL),
            false,
        ),
    ]),
    ignore_missing: false,
    locking: true,
    no_tx: false,
});

#[cfg(test)]
#[path = "migrator_test.rs"]
mod tests;
