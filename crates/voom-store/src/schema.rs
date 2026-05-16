use std::collections::HashMap;

use sqlx::SqlitePool;
use time::OffsetDateTime;
use voom_core::VoomError;

use crate::migrator::MIGRATOR;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaState {
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
    /// At least one applied migration version is not in the embedded MIGRATOR
    /// — either a newer binary touched this DB or migrations were renumbered.
    TooNew { applied: u32, expected: u32 },
    /// One or more migration rows are recorded with `success=0` — a previous
    /// migration attempt aborted mid-flight. sqlx refuses to migrate further
    /// against a dirty schema, so this requires manual operator action
    /// (remove the failed row from `_sqlx_migrations` or restore from
    /// backup) rather than a simple `voom init` rerun.
    Dirty {
        failed_version: i64,
        applied: u32,
        expected: u32,
    },
}

/// Number of migrations this build ships, derived from the embedded MIGRATOR
/// at runtime. No hand-maintained constant — adding a `migrations/000N_*.sql`
/// file automatically bumps this without code changes.
#[must_use]
pub fn expected_migrations() -> u32 {
    u32::try_from(MIGRATOR.iter().count()).unwrap_or(u32::MAX)
}

/// Map of `version → checksum` for every migration this build ships. Both the
/// version *and* the checksum are validated against `_sqlx_migrations` rows
/// so a row with a known version but mutated SQL is still surfaced as drift.
fn embedded_versions() -> HashMap<i64, Vec<u8>> {
    MIGRATOR
        .iter()
        .map(|m| (m.version, m.checksum.to_vec()))
        .collect()
}

/// Inspect the schema without modifying it.
pub async fn probe_schema(pool: &SqlitePool) -> Result<SchemaState, VoomError> {
    let migrations_table_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| VoomError::Database(format!("probing for _sqlx_migrations failed: {e}")))?;

    if migrations_table_exists == 0 {
        return Ok(SchemaState::Uninitialized);
    }

    // Read ALL rows (not just success=1). A failed-but-recorded migration
    // attempt leaves a success=0 row that must NOT be ignored — otherwise
    // health can mis-report a half-applied DB as Current.
    let all_rows: Vec<(i64, Vec<u8>, bool)> =
        sqlx::query_as("SELECT version, checksum, success FROM _sqlx_migrations")
            .fetch_all(pool)
            .await
            .map_err(|e| VoomError::Database(format!("reading _sqlx_migrations failed: {e}")))?;

    let expected = expected_migrations();
    let known = embedded_versions();

    let unknown_version_present = all_rows.iter().any(|(v, _, _)| !known.contains_key(v));
    let any_failed = all_rows.iter().any(|(_, _, success)| !success);
    let successful_count =
        u32::try_from(all_rows.iter().filter(|(_, _, s)| *s).count()).unwrap_or(u32::MAX);

    // Order matters:
    //   1. Unknown-version rows (success or not) → TooNew.
    //   2. Any failed row with only known versions → Dirty. sqlx will refuse
    //      to migrate over a dirty row; this is operator-attention material,
    //      NOT a `voom init` rerun.
    //   3. Checksum drift on a successful known row → TooNew.
    //   4. successful_count < expected → Partial. Safe to rerun init.
    //   5. Else Current.
    if unknown_version_present {
        return Ok(SchemaState::TooNew {
            applied: successful_count,
            expected,
        });
    }
    if any_failed {
        // Surface the first failed version so operators get a precise
        // pointer into _sqlx_migrations for manual cleanup.
        let failed_version = all_rows
            .iter()
            .find(|(_, _, success)| !*success)
            .map_or(0, |(v, _, _)| *v);
        return Ok(SchemaState::Dirty {
            failed_version,
            applied: successful_count,
            expected,
        });
    }

    let any_drift = all_rows.iter().any(|(version, checksum, _)| {
        known
            .get(version)
            .is_some_and(|known_checksum| known_checksum.as_slice() != checksum.as_slice())
    });
    if any_drift {
        return Ok(SchemaState::TooNew {
            applied: successful_count,
            expected,
        });
    }

    // Ordered-prefix invariant: the set of successful applied versions must
    // be exactly the first N versions of the embedded MIGRATOR (where N is
    // the successful row count). A gap (e.g., MIGRATOR has [1, 2] but the DB
    // has only [2]) would otherwise be classified as Partial and `voom init`
    // would happily apply version 1 *after* version 2 — a migration-order
    // violation with data-corruption risk. Surface gaps and out-of-order
    // known versions as TooNew so operators must restore/repair manually.
    let mut applied_versions: Vec<i64> = all_rows
        .iter()
        .filter_map(|(v, _, s)| if *s { Some(*v) } else { None })
        .collect();
    applied_versions.sort_unstable();

    let expected_prefix: Vec<i64> = MIGRATOR
        .iter()
        .map(|m| m.version)
        .take(applied_versions.len())
        .collect();

    if applied_versions != expected_prefix {
        return Ok(SchemaState::TooNew {
            applied: successful_count,
            expected,
        });
    }

    if successful_count < expected {
        return Ok(SchemaState::Partial {
            applied: successful_count,
            expected,
        });
    }

    // successful_count == expected AND every (version, checksum) matches.
    // Read the schema_meta marker; failures here mean the migration table
    // applied but metadata is missing → Migration error (DB_PARTIAL_SCHEMA),
    // not Database (DB_UNREACHABLE). The DB is reachable; its content is wrong.
    let init_at: String =
        sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'schema_init_at'")
            .fetch_one(pool)
            .await
            .map_err(|e| {
                VoomError::Migration(format!(
                    "schema_meta.schema_init_at is missing or unreadable (DB is reachable \
                     but schema is corrupted): {e}"
                ))
            })?;

    let schema_init_at = OffsetDateTime::parse(
        &init_at,
        &time::format_description::well_known::Iso8601::DEFAULT,
    )
    .map_err(|e| {
        VoomError::Migration(format!(
            "schema_meta.schema_init_at is malformed ({init_at:?}): {e}"
        ))
    })?;

    Ok(SchemaState::Current {
        migration_count: successful_count,
        schema_init_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::connect;

    /// SQL that creates an empty `_sqlx_migrations` table matching sqlx's
    /// schema. Tests use this to simulate post-init states without depending
    /// on Task 11's `init_on` (which doesn't exist yet at this checkpoint).
    const CREATE_MIGRATIONS_TABLE: &str = "\
        CREATE TABLE _sqlx_migrations ( \
            version BIGINT PRIMARY KEY, \
            description TEXT NOT NULL, \
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
            success BOOLEAN NOT NULL, \
            checksum BLOB NOT NULL, \
            execution_time BIGINT NOT NULL \
        )";

    #[tokio::test]
    async fn probe_returns_uninitialized_on_fresh_db() {
        let pool = connect("sqlite::memory:").await.unwrap();
        assert_eq!(
            probe_schema(&pool).await.unwrap(),
            SchemaState::Uninitialized
        );
    }

    #[tokio::test]
    async fn expected_migrations_matches_embedded_count() {
        assert_eq!(expected_migrations(), 1);
    }

    #[tokio::test]
    async fn probe_returns_too_new_on_renumbered_migration_at_same_count() {
        // Pathological case: count matches expectation but the *version* is
        // not in the embedded MIGRATOR. Seed migrations table by hand — no
        // dependency on init_on (which lands in Task 11).
        let pool = connect("sqlite::memory:").await.unwrap();
        sqlx::query(CREATE_MIGRATIONS_TABLE)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             VALUES (42, 'renumbered', strftime('%s','now'), 1, X'00', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = probe_schema(&pool).await.unwrap();
        match state {
            SchemaState::TooNew { applied, expected } => {
                assert_eq!(applied, expected, "count matches but version is unknown");
            }
            other => panic!("expected TooNew (version not in MIGRATOR), got {other:?}"),
        }
    }
}
