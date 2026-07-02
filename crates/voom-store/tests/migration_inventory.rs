#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

//! Guard against the most likely future regression: someone adds a new
//! `migrations/000N_*.sql` but forgets to register it in `migrator.rs`'s
//! hand-rolled `vec![Migration::new(...)]`. The sqlx macro used to scan the
//! directory automatically; we replaced that with a manual list to drop the
//! `macros` feature, so this test re-asserts the inventory invariant.

use std::borrow::Cow;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use sqlx::migrate::Migrator;
use tempfile::NamedTempFile;
use voom_store::test_support::sqlite_url_for;
use voom_store::{MIGRATOR, connect_or_create};

const EXPECTED_MIGRATION_FILES: &[&str] = &[
    "0001_init.sql",
    "0002_durable_execution.sql",
    "0003_identity.sql",
    "0004_use_leases_ancillary.sql",
    "0005_commit_intents_persistent_permit.sql",
    "0006_policy_inputs.sql",
    "0007_policy_registry.sql",
    "0008_issue_dedupe_key.sql",
    "0009_nodes.sql",
    "0010_remote_execution.sql",
    "0011_scheduler_decisions.sql",
    "0012_staged_artifact_commit.sql",
    "0013_audio_sidecar_support.sql",
    "0014_video_profiles.sql",
    "0015_workflow_summaries.sql",
    "0016_worker_grant_max_parallel_wildcard.sql",
    "0017_scan_file_facts.sql",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn migrations_dir() -> PathBuf {
    workspace_root().join("migrations")
}

fn migration_file_names() -> Vec<String> {
    let migrations_dir = migrations_dir();
    let mut names: Vec<String> = fs::read_dir(&migrations_dir)
        .unwrap_or_else(|e| panic!("read_dir({}) failed: {e}", migrations_dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
        })
        .collect();
    names.sort_unstable();
    names
}

/// Parse a migrations filename like `0001_init.sql` into its version number.
fn parse_version(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".sql")?;
    let (version_str, _description) = stem.split_once('_')?;
    version_str.parse().ok()
}

fn migrator_through(version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            MIGRATOR
                .iter()
                .filter(|migration| migration.version <= version)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    }
}

#[test]
fn every_migrations_file_is_registered_in_migrator() {
    let file_names = migration_file_names();
    assert_eq!(file_names, EXPECTED_MIGRATION_FILES);

    let file_versions: Vec<i64> = file_names
        .iter()
        .filter_map(|name| parse_version(name))
        .collect();

    let mut registered_versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    registered_versions.sort_unstable();

    assert_eq!(
        file_versions, registered_versions,
        "migrations/ directory and MIGRATOR are out of sync — every \
         migrations/000N_*.sql must be registered in voom-store/src/migrator.rs"
    );
    assert!(
        !file_versions.is_empty(),
        "no migrations found — sanity check that the test is reading the right path"
    );
}

#[tokio::test]
async fn staged_artifact_commit_migration_preserves_seeded_file_version_links() {
    let migration_path = migrations_dir().join("0012_staged_artifact_commit.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = connect_or_create(&url).await.unwrap();

    migrator_through(11).run(&pool).await.unwrap();

    let now = "2026-05-25T00:00:00Z";
    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(now)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();

    let source_file_version_id = sqlx::query(
        "INSERT INTO file_versions \
         (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (?, 'blake3:source', 3, 'external_observed', ?)",
    )
    .bind(file_asset_id)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();

    sqlx::query(
        "INSERT INTO file_locations \
         (file_version_id, kind, value, observed_at) \
         VALUES (?, 'local_path', '/media/source.mkv', ?)",
    )
    .bind(source_file_version_id)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO media_snapshots (file_version_id, probed_at, payload) \
         VALUES (?, ?, '{}')",
    )
    .bind(source_file_version_id)
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();

    let source_lineage =
        serde_json::json!({ "source_file_version_id": source_file_version_id }).to_string();
    sqlx::query(
        "INSERT INTO artifact_handles \
         (size_bytes, checksum, privacy_class, durability_class, allowed_access_modes, \
          mutability, source_lineage, created_at, file_asset_id, file_version_id) \
         VALUES (3, 'blake3:source', 'internal', 'durable', '[\"read\"]', \
                 'immutable', ?, ?, ?, ?)",
    )
    .bind(source_lineage)
    .bind(now)
    .bind(file_asset_id)
    .bind(source_file_version_id)
    .execute(&pool)
    .await
    .unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(violations, Vec::<(String, i64, String, i64)>::new());

    sqlx::query(
        "INSERT INTO file_versions \
         (file_asset_id, content_hash, size_bytes, produced_by, produced_from_version_id, \
          created_at) \
         VALUES (?, 'blake3:new', 3, 'staged_commit', ?, '2026-05-25T00:00:00Z')",
    )
    .bind(file_asset_id)
    .bind(source_file_version_id)
    .execute(&pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn worker_grant_max_parallel_migration_rewrites_legacy_limit() {
    let migration_path = migrations_dir().join("0016_worker_grant_max_parallel_wildcard.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = connect_or_create(&url).await.unwrap();

    migrator_through(15).run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO workers \
         (name, kind, status, registered_at, last_seen_at, epoch) \
         VALUES ('worker-a', 'local', 'active', '2026-05-25T00:00:00Z', \
                 '2026-05-25T00:00:00Z', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let worker_id = sqlx::query_scalar::<_, i64>("SELECT id FROM workers WHERE name = 'worker-a'")
        .fetch_one(&pool)
        .await
        .unwrap();

    sqlx::query(
        "INSERT INTO worker_grants \
         (worker_id, can_execute, can_access_read, can_access_write, denies, max_parallel) \
         VALUES (?, '[\"probe_file\"]', '[]', '[]', '[]', ?)",
    )
    .bind(worker_id)
    .bind(serde_json::json!({"limit": 3}).to_string())
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO worker_grants \
         (worker_id, can_execute, can_access_read, can_access_write, denies, max_parallel) \
         VALUES (?, '[\"transcode_video\"]', '[]', '[]', '[]', ?)",
    )
    .bind(worker_id)
    .bind(serde_json::json!({"limit": 5, "transcode_video": 2}).to_string())
    .execute(&pool)
    .await
    .unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    let rows: Vec<String> =
        sqlx::query_scalar("SELECT max_parallel FROM worker_grants ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let values = rows
        .iter()
        .map(|row| serde_json::from_str::<serde_json::Value>(row).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(values[0], serde_json::json!({"*": 3}));
    assert_eq!(values[1], serde_json::json!({"transcode_video": 2}));
}

#[test]
fn migrator_versions_are_strictly_increasing() {
    let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
    let mut sorted = versions.clone();
    sorted.sort_unstable();
    assert_eq!(
        versions, sorted,
        "MIGRATOR must be ordered by ascending version: {versions:?}"
    );
    let dedup_len = {
        let mut d = sorted.clone();
        d.dedup();
        d.len()
    };
    assert_eq!(
        versions.len(),
        dedup_len,
        "MIGRATOR must have unique versions: {versions:?}"
    );
}
