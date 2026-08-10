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

use serde_json::{Value as JsonValue, json};
use sqlx::migrate::Migrator;
use voom_events::Event;
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use voom_store::test_support::{create_uninitialized_pool, embedded_migrator, sqlite_url_for};
use voom_test_support::TempDatabase;

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
    "0018_backups.sql",
    "0019_libraries.sql",
    "0020_scheduling_safety_policies.sql",
    "0021_profile_management.sql",
    "0022_workflow_file_run_starts.sql",
    "0023_workflow_file_run_history.sql",
    "0024_atomic_audio_extract_operations.sql",
    "0025_recoverable_audio_synthesis.sql",
    "0026_policy_artifact_verification.sql",
    "0027_audio_synthesis_asset_lineage.sql",
    "0028_sliding_file_window.sql",
    "0029_nvidia_video_acceleration.sql",
    "0030_videotoolbox_video_profiles.sql",
    "0031_backend_neutral_accelerator_claims.sql",
    "0032_vaapi_video_acceleration.sql",
    "0033_remote_acquire_replay_shape.sql",
    "0034_node_owned_roots.sql",
    "0035_node_incarnations.sql",
    "0036_scan_sessions.sql",
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
            embedded_migrator()
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

    let mut registered_versions: Vec<i64> = embedded_migrator()
        .iter()
        .map(|migration| migration.version)
        .collect();
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
async fn node_incarnation_migration_preserves_existing_nodes_and_workers() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(34).run(&pool).await.unwrap();
    let (node_id, worker_id) = seed_remote_execution_owner(&pool).await;

    embedded_migrator().run(&pool).await.unwrap();

    let active_incarnation: Option<String> =
        sqlx::query_scalar("SELECT active_incarnation_id FROM nodes WHERE id = ?")
            .bind(node_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    let worker_incarnation: Option<String> =
        sqlx::query_scalar("SELECT node_incarnation_id FROM workers WHERE id = ?")
            .bind(worker_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(active_incarnation, None);
    assert_eq!(worker_incarnation, None);
}

type RootRow = (
    i64,
    Option<i64>,
    String,
    String,
    String,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

#[tokio::test]
async fn remote_acquire_replay_shape_migration_canonicalizes_only_missing_decision_ids() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(32).run(&pool).await.unwrap();
    let (node_id, worker_id) = seed_remote_execution_owner(&pool).await;
    let unchanged = seed_remote_acquire_replays(&pool, node_id, worker_id).await;

    embedded_migrator().run(&pool).await.unwrap();

    for key in ["legacy-idle", "legacy-no-candidate", "legacy-leased"] {
        let response = remote_replay_json(&pool, key).await;
        assert_eq!(
            response.pointer("/data/scheduler_decision_id"),
            Some(&json!(0)),
            "migration must canonicalize {key}"
        );
    }
    for (key, before) in unchanged {
        let after: String = sqlx::query_scalar(
            "SELECT response_json FROM remote_idempotency_keys WHERE idempotency_key = ?",
        )
        .bind(key)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(after, before, "migration must not rewrite {key}");
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the migration regression keeps its ordered before/after integrity proof together"
)]
async fn node_owned_roots_migration_quarantines_legacy_paths() {
    let migration_path = migrations_dir().join("0034_node_owned_roots.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(33).run(&pool).await.unwrap();
    let seed = seed_legacy_root_and_location(&pool).await;
    seed_location_foreign_key_children(&pool, &seed).await;
    let child_ids_before = location_child_rowids(&pool).await;
    let location_ids_before: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM file_locations ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();

    embedded_migrator().run(&pool).await.unwrap();

    let root: RootRow = sqlx::query_as(
        "SELECT id, owner_node_id, provider_kind, provider_locator, state, enabled, \
             default_output_root_id, default_staging_root_id, default_backup_root_id \
             FROM library_roots WHERE id = ?",
    )
    .bind(seed.root)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        root,
        (
            seed.root,
            None,
            "local_filesystem".to_owned(),
            "/legacy/library".to_owned(),
            "unassigned".to_owned(),
            0,
            None,
            None,
            None,
        )
    );

    let location: (
        i64,
        String,
        Option<i64>,
        Option<String>,
        String,
        String,
        i64,
    ) = sqlx::query_as(
        "SELECT id, address_state, storage_root_id, provider_relative_locator, \
             legacy_kind, legacy_locator, epoch FROM file_locations WHERE id = ?",
    )
    .bind(seed.location)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        location,
        (
            seed.location,
            "unassigned_legacy".to_owned(),
            None,
            None,
            "local_path".to_owned(),
            "/legacy/library/movie.mkv".to_owned(),
            7,
        )
    );

    let retired_location: (
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT address_state, legacy_kind, proof_kind, proof_value, retired_at, epoch \
         FROM file_locations WHERE id = ?",
    )
    .bind(seed.retired_location)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        retired_location,
        (
            "unassigned_legacy".to_owned(),
            "historical".to_owned(),
            Some("file_id_generation".to_owned()),
            Some("11:12:6".to_owned()),
            Some("2026-08-04T00:01:00Z".to_owned()),
            9,
        )
    );

    let scan_location_id: i64 = sqlx::query_scalar("SELECT file_location_id FROM scan_file_facts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(scan_location_id, seed.location);

    let location_ids_after: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM file_locations ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(location_ids_after, location_ids_before);
    assert_eq!(location_child_rowids(&pool).await, child_ids_before);
    assert_seeded_location_children(&pool).await;
    let lineage: String = sqlx::query_scalar(
        "SELECT source_lineage FROM artifact_handles \
         WHERE file_version_id = ? AND source_lineage IS NOT NULL",
    )
    .bind(seed.file_version)
    .fetch_one(&pool)
    .await
    .unwrap();
    let lineage: serde_json::Value = serde_json::from_str(&lineage).unwrap();
    assert_eq!(
        lineage.get("source_file_location_id"),
        Some(&json!(seed.location))
    );
    assert!(lineage.get("source_location_id").is_none());
    assert_post_root_migration_integrity(&pool, &seed).await;
}

#[tokio::test]
async fn node_owned_roots_migration_preserves_and_decodes_legacy_location_events() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(33).run(&pool).await.unwrap();
    let events = [
        (
            "file_location.recorded",
            json!({
                "file_location_id": 11,
                "file_version_id": 21,
                "kind": "local_path",
                "value": "/legacy/recorded.mkv"
            }),
        ),
        (
            "file_location.aliased",
            json!({
                "file_location_id": 12,
                "file_version_id": 21,
                "kind": "local_path",
                "value": "/legacy/alias.mkv"
            }),
        ),
        (
            "file_location.recorded_by_move",
            json!({
                "retired_file_location_id": 12,
                "new_file_location_id": 13,
                "file_version_id": 21,
                "kind": "local_path",
                "value": "/legacy/moved.mkv",
                "observed_at": "2026-08-04T00:00:00Z"
            }),
        ),
    ];
    for (kind, payload) in events {
        sqlx::query(
            "INSERT INTO events \
             (occurred_at, kind, subject_type, subject_id, trace_id, payload) \
             VALUES ('2026-08-04T00:00:00Z', ?, 'file_location', 11, NULL, ?)",
        )
        .bind(kind)
        .bind(payload.to_string())
        .execute(&pool)
        .await
        .unwrap();
    }
    let before: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT event_id, kind, payload FROM events \
         WHERE kind LIKE 'file_location.%' ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    embedded_migrator().run(&pool).await.unwrap();

    let after: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT event_id, kind, payload FROM events \
         WHERE kind LIKE 'file_location.%' ORDER BY event_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        after, before,
        "append-only event facts must not be rewritten"
    );
    let page = SqliteEventRepo::new(pool)
        .list(
            EventFilter::default(),
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap();
    let decoded: Vec<&Event> = page
        .items
        .iter()
        .map(|row| &row.envelope.payload)
        .filter(|event| event.kind().as_str().starts_with("file_location."))
        .collect();
    assert!(matches!(decoded[0], Event::FileLocationRecorded(_)));
    assert!(matches!(decoded[1], Event::FileLocationAliased(_)));
    assert!(matches!(decoded[2], Event::FileLocationRecordedByMove(_)));
}

#[tokio::test]
async fn node_owned_roots_migration_rejects_ambiguous_artifact_lineage_keys() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(33).run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO artifact_handles \
         (privacy_class, durability_class, allowed_access_modes, mutability, source_lineage, \
          created_at) \
         VALUES ('internal', 'staging', '[\"read\"]', 'immutable', \
                 '{\"source_location_id\":1,\"source_file_location_id\":2}', \
                 '2026-08-04T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let error = embedded_migrator().run(&pool).await.unwrap_err();

    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "unexpected migration error: {error}"
    );
    let legacy_column_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pragma_table_info('library_roots') WHERE name = 'canonical_path'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        legacy_column_count, 1,
        "guard must fail before table rebuild"
    );
}

struct LegacyRootLocationSeed {
    root: i64,
    file_version: i64,
    location: i64,
    retired_location: i64,
}

async fn seed_legacy_root_and_location(pool: &sqlx::SqlitePool) -> LegacyRootLocationSeed {
    let now = "2026-08-04T00:00:00Z";
    let library_id = sqlx::query(
        "INSERT INTO libraries \
         (slug, display_name, media_kind, enabled, created_at, updated_at) \
         VALUES ('legacy', 'Legacy', 'movie', 1, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let root_id = sqlx::query(
        "INSERT INTO library_roots \
         (library_id, root_kind, canonical_path, display_path, scan_mode, symlink_policy, \
          hidden_file_policy, default_output_root, default_staging_root, default_backup_root, \
          enabled, created_at, updated_at) \
         VALUES (?, 'local_path', '/legacy/library', '/legacy/library', 'manual_recursive', \
                 'reject', 'ignore', '/legacy/output', '/legacy/staging', '/legacy/backup', \
                 1, ?, ?)",
    )
    .bind(library_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version_id = sqlx::query(
        "INSERT INTO file_versions \
         (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (?, 'blake3:legacy', 7, 'external_observed', ?)",
    )
    .bind(file_asset_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let location_id = sqlx::query(
        "INSERT INTO file_locations \
         (file_version_id, kind, value, observed_at, epoch) \
         VALUES (?, 'local_path', '/legacy/library/movie.mkv', ?, 7)",
    )
    .bind(file_version_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let retired_location_id = sqlx::query(
        "INSERT INTO file_locations \
         (file_version_id, kind, value, proof_kind, proof_value, observed_at, retired_at, epoch) \
         VALUES (?, 'historical', '/legacy/library/old-name.mkv', 'file_id_generation', \
                 '11:12:6', ?, '2026-08-04T00:01:00Z', 9)",
    )
    .bind(file_version_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO scan_file_facts (file_location_id, dev, ino, nlink, observed_at) \
         VALUES (?, 11, 12, 2, ?)",
    )
    .bind(location_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    LegacyRootLocationSeed {
        root: root_id,
        file_version: file_version_id,
        location: location_id,
        retired_location: retired_location_id,
    }
}

async fn seed_location_foreign_key_children(
    pool: &sqlx::SqlitePool,
    seed: &LegacyRootLocationSeed,
) {
    seed_location_lease_and_commit_scope(pool, seed.location).await;
    seed_policy_location_inputs(pool, seed.location).await;
    let artifact = seed_artifact_commit_location(pool, seed).await;
    seed_workflow_location_summary(pool, seed, artifact).await;
    seed_audio_operation_locations(pool).await;
}

async fn seed_location_lease_and_commit_scope(pool: &sqlx::SqlitePool, location_id: i64) {
    let now = "2026-08-04T00:00:00Z";
    sqlx::query(
        "INSERT INTO asset_use_leases \
         (kind, scope_location_id, issuer_kind, issuer_ref, blocking_mode, ttl_bound, \
          acquired_at, clock_source) \
         VALUES ('playback', ?, 'user', 'migration', 'advisory', 0, ?, 'control_plane')",
    )
    .bind(location_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    let intent_id = sqlx::query(
        "INSERT INTO commit_intents \
         (target, closure_initial, accepted_evidence_ids, state, started_at) \
         VALUES ('{}', '{}', '[]', 'pending', ?)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO commit_intent_scope_members (commit_intent_id, scope_location_id) \
         VALUES (?, ?)",
    )
    .bind(intent_id)
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_policy_location_inputs(pool: &sqlx::SqlitePool, location_id: i64) {
    let now = "2026-08-04T00:00:00Z";
    let set_id = sqlx::query(
        "INSERT INTO policy_input_sets \
         (slug, display_name, schema_version, source_kind, created_at) \
         VALUES ('migration-location', 'Migration location', 1, 'test', ?)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO policy_media_snapshot_inputs \
         (policy_input_set_id, ordinal, file_location_id, stream_summary) \
         VALUES (?, 0, ?, '{}')",
    )
    .bind(set_id)
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_identity_evidence_inputs \
         (policy_input_set_id, ordinal, file_location_id, assertion_type, provider, \
          provider_version, confidence, provenance, observed_at) \
         VALUES (?, 0, ?, 'hash_match', 'migration', '1', 1.0, '{}', ?)",
    )
    .bind(set_id)
    .bind(location_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_bundle_target_inputs \
         (policy_input_set_id, ordinal, file_location_id, role, desired_state, \
          artifact_expectation) VALUES (?, 0, ?, 'primary_video', 'allowed', '{}')",
    )
    .bind(set_id)
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_quality_profile_selections \
         (policy_input_set_id, ordinal, file_location_id, profile_name, profile_version, \
          dimension_weights) VALUES (?, 0, ?, 'migration', '1', '{}')",
    )
    .bind(set_id)
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_issue_inputs \
         (policy_input_set_id, ordinal, file_location_id, kind, severity, priority, state, \
          reason, provenance) \
         VALUES (?, 0, ?, 'migration', 'low', 'normal', 'open', 'migration', '{}')",
    )
    .bind(set_id)
    .bind(location_id)
    .execute(pool)
    .await
    .unwrap();
}

struct ArtifactCommitSeed {
    handle_id: i64,
    verification_id: i64,
}

async fn seed_artifact_commit_location(
    pool: &sqlx::SqlitePool,
    seed: &LegacyRootLocationSeed,
) -> ArtifactCommitSeed {
    let now = "2026-08-04T00:00:00Z";
    let worker_id = sqlx::query(
        "INSERT INTO workers (name, kind, status, registered_at, last_seen_at) \
         VALUES ('migration-worker-418', 'local', 'active', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let handle_id = sqlx::query(
        "INSERT INTO artifact_handles \
         (size_bytes, checksum, privacy_class, durability_class, allowed_access_modes, \
          mutability, source_lineage, created_at, file_version_id) \
         VALUES (7, 'blake3:legacy', 'internal', 'durable', '[\"read\"]', \
                 'immutable', json_object('source_location_id', ?), ?, ?)",
    )
    .bind(seed.location)
    .bind(now)
    .bind(seed.file_version)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let artifact_location_id = sqlx::query(
        "INSERT INTO artifact_locations \
         (artifact_handle_id, kind, value, observed_at) \
         VALUES (?, 'staging', '/legacy/staging/movie.mkv', ?)",
    )
    .bind(handle_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let verification_id = sqlx::query(
        "INSERT INTO artifact_verifications \
         (artifact_handle_id, artifact_location_id, path, worker_id, status, \
          expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
          report, started_at, finished_at) \
         VALUES (?, ?, '/legacy/staging/movie.mkv', ?, 'succeeded', 7, 'blake3:legacy', \
                 7, 'blake3:legacy', '{}', ?, ?)",
    )
    .bind(handle_id)
    .bind(artifact_location_id)
    .bind(worker_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, report, started_at, \
          promotion_started_at, finished_at) \
         VALUES (?, ?, ?, '/legacy/library/movie.mkv', ?, ?, 'committed', '{}', ?, ?, ?)",
    )
    .bind(handle_id)
    .bind(seed.file_version)
    .bind(verification_id)
    .bind(seed.file_version)
    .bind(seed.location)
    .bind(now)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    ArtifactCommitSeed {
        handle_id,
        verification_id,
    }
}

async fn seed_workflow_location_summary(
    pool: &sqlx::SqlitePool,
    seed: &LegacyRootLocationSeed,
    artifact: ArtifactCommitSeed,
) {
    let now = "2026-08-04T00:00:00Z";
    let snapshot_id = sqlx::query(
        "INSERT INTO media_snapshots (file_version_id, probed_at, payload) VALUES (?, ?, '{}')",
    )
    .bind(seed.file_version)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let job_id = sqlx::query(
        "INSERT INTO jobs (kind, state, created_at, updated_at) \
         VALUES ('migration', 'succeeded', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO workflow_file_phase_summaries \
         (job_id, phase_ordinal, branch_id, ticket_ids, produced_file_version_id, \
          produced_file_location_id, artifact_handle_id, artifact_verification_id, \
          reprobe_snapshot_id, outcome, created_at) \
         VALUES (?, 0, 'migration', '[]', ?, ?, ?, ?, ?, 'verified', ?)",
    )
    .bind(job_id)
    .bind(seed.file_version)
    .bind(seed.location)
    .bind(artifact.handle_id)
    .bind(artifact.verification_id)
    .bind(snapshot_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_audio_operation_locations(pool: &sqlx::SqlitePool) {
    let (asset_id, versions, locations, snapshots) = seed_synthesis_versions(pool).await;
    let bundle_id = seed_audio_bundle(pool, asset_id).await;
    let operation_id = sqlx::query(
        "INSERT INTO audio_extract_operations \
         (operation_key, source_file_version_id, source_bundle_id, source_media_snapshot_id, \
          state, created_at) VALUES ('migration-extract', ?, ?, ?, 'planned', \
                                    '2026-08-04T00:00:00Z')",
    )
    .bind(versions[0])
    .bind(bundle_id.0)
    .bind(snapshots[0])
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO audio_extract_operation_outputs \
         (operation_id, ordinal, source_snapshot_stream_id, source_provider_stream_index, \
          bundle_role, target_path, result_file_asset_id, result_file_version_id, \
          result_file_location_id, result_media_snapshot_id, bundle_member_id) \
         VALUES (?, 0, 'audio:0', 0, 'external_audio', '/media/extract.mka', ?, ?, ?, ?, ?)",
    )
    .bind(operation_id)
    .bind(asset_id)
    .bind(versions[1])
    .bind(locations[1])
    .bind(snapshots[1])
    .bind(bundle_id.1)
    .execute(pool)
    .await
    .unwrap();
    insert_synthesis_operation(
        pool,
        SynthesisOperationSeed {
            operation_key: "migration-synthesis",
            target_path: "/media/synthesis.mkv",
            source_version_id: versions[1],
            source_snapshot_id: snapshots[1],
            result_asset_id: asset_id,
            result_version_id: versions[2],
            result_location_id: locations[2],
            result_snapshot_id: snapshots[2],
        },
    )
    .await;
}

async fn seed_audio_bundle(pool: &sqlx::SqlitePool, asset_id: i64) -> (i64, i64) {
    let now = "2026-08-04T00:00:00Z";
    let work_id = sqlx::query(
        "INSERT INTO media_works (kind, display_title, created_at) \
         VALUES ('movie', 'Migration', ?)",
    )
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let variant_id = sqlx::query(
        "INSERT INTO media_variants (media_work_id, label, created_at) VALUES (?, 'main', ?)",
    )
    .bind(work_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let bundle_id = sqlx::query(
        "INSERT INTO asset_bundles (media_variant_id, display_name, created_at) \
         VALUES (?, 'Migration', ?)",
    )
    .bind(variant_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let member_id = sqlx::query(
        "INSERT INTO asset_bundle_members (bundle_id, file_asset_id, role) \
         VALUES (?, ?, 'primary_video')",
    )
    .bind(bundle_id)
    .bind(asset_id)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    (bundle_id, member_id)
}

async fn assert_seeded_location_children(pool: &sqlx::SqlitePool) {
    for table in location_child_tables() {
        let query = format!("SELECT count(*) FROM {table}");
        let count: i64 = sqlx::query_scalar(&query).fetch_one(pool).await.unwrap();
        assert!(count > 0, "{table} seed was not preserved");
    }
}

async fn location_child_rowids(pool: &sqlx::SqlitePool) -> Vec<(String, Vec<i64>)> {
    let mut snapshot = Vec::new();
    for table in location_child_tables() {
        let query = format!("SELECT rowid FROM {table} ORDER BY rowid");
        let rowids = sqlx::query_scalar(&query).fetch_all(pool).await.unwrap();
        snapshot.push((table.to_owned(), rowids));
    }
    snapshot
}

fn location_child_tables() -> [&'static str; 12] {
    [
        "scan_file_facts",
        "asset_use_leases",
        "commit_intent_scope_members",
        "policy_media_snapshot_inputs",
        "policy_identity_evidence_inputs",
        "policy_bundle_target_inputs",
        "policy_quality_profile_selections",
        "policy_issue_inputs",
        "artifact_commit_records",
        "workflow_file_phase_summaries",
        "audio_extract_operation_outputs",
        "audio_synthesis_operations",
    ]
}

async fn assert_post_root_migration_integrity(
    pool: &sqlx::SqlitePool,
    seed: &LegacyRootLocationSeed,
) {
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(pool)
        .await
        .unwrap();
    assert!(violations.is_empty(), "{violations:?}");

    let old_references: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema \
         WHERE sql LIKE '%library_roots_old%' OR sql LIKE '%file_locations_old%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(old_references, 0);

    let foreign_keys_enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(foreign_keys_enabled, 1);
    let error = sqlx::query(
        "INSERT INTO file_locations \
         (file_version_id, address_state, storage_root_id, provider_relative_locator, \
          observed_at) VALUES (?, 'rooted', 9223372036854775807, 'movie.mkv', \
                               '2026-08-04T00:00:00Z')",
    )
    .bind(seed.file_version)
    .execute(pool)
    .await
    .unwrap_err();
    assert!(error.to_string().contains("FOREIGN KEY constraint failed"));
}

async fn seed_remote_execution_owner(pool: &sqlx::SqlitePool) -> (i64, i64) {
    let node_id = sqlx::query(
        "INSERT INTO nodes \
         (name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES ('migration-node', 'synthetic', 'active', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'hash', 'hint', '{}')",
    )
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let worker_id = sqlx::query(
        "INSERT INTO workers (name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES ('migration-worker', 'remote', 'active', ?, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(node_id)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    (node_id, worker_id)
}

async fn seed_remote_acquire_replays(
    pool: &sqlx::SqlitePool,
    node_id: i64,
    worker_id: i64,
) -> Vec<(&'static str, String)> {
    let cases = remote_acquire_replay_cases(worker_id);
    let mut unchanged = Vec::new();
    for (key, route, response, should_change) in cases {
        let encoded = serde_json::to_string(&response).unwrap();
        sqlx::query(
            "INSERT INTO remote_idempotency_keys \
             (node_id, route_key, worker_scope_id, worker_id, idempotency_key, request_hash, \
              response_json, status, created_at) \
             VALUES (?, ?, ?, ?, ?, 'hash', ?, 'completed', '1970-01-01T00:00:00Z')",
        )
        .bind(node_id)
        .bind(route)
        .bind(worker_id)
        .bind(worker_id)
        .bind(key)
        .bind(&encoded)
        .execute(pool)
        .await
        .unwrap();
        if !should_change {
            unchanged.push((key, encoded));
        }
    }
    unchanged
}

fn remote_acquire_replay_cases(
    worker_id: i64,
) -> Vec<(&'static str, &'static str, JsonValue, bool)> {
    let acquire = "POST /v1/execution/lease/acquire";
    vec![
        (
            "legacy-idle",
            acquire,
            replay_ok(&json!({"outcome":"idle","worker_id":worker_id})),
            true,
        ),
        (
            "legacy-no-candidate",
            acquire,
            replay_ok(&json!({"outcome":"no_candidate","worker_id":worker_id})),
            true,
        ),
        (
            "legacy-leased",
            acquire,
            replay_ok(&json!({
                "outcome":"leased",
                "lease_id":91,
                "ticket_id":92,
                "worker_id":worker_id,
                "operation":"probe_file",
                "dispatch_payload":{"source":"migration"},
                "lease_ttl_seconds":60,
                "heartbeat_after_seconds":30,
                "artifact_access_plan":{
                    "id":93,
                    "input_handles":["handle:input:migration"],
                    "output_handles":["handle:output:migration"],
                    "selected_access_mode":"shared_mount"
                }
            })),
            true,
        ),
        (
            "current",
            acquire,
            replay_ok(&json!({
                "outcome":"idle",
                "worker_id":worker_id,
                "scheduler_decision_id":42
            })),
            false,
        ),
        (
            "explicit-null",
            acquire,
            replay_ok(&json!({
                "outcome":"idle",
                "worker_id":worker_id,
                "scheduler_decision_id":null
            })),
            false,
        ),
        (
            "wrong-type",
            acquire,
            replay_ok(&json!({
                "outcome":"idle",
                "worker_id":worker_id,
                "scheduler_decision_id":"42"
            })),
            false,
        ),
        ("non-object", acquire, replay_ok(&json!("idle")), false),
        (
            "unknown-outcome",
            acquire,
            replay_ok(&json!({"outcome":"future"})),
            false,
        ),
        (
            "error",
            acquire,
            json!({"status":"error","code":"CONFLICT","message":"done"}),
            false,
        ),
        (
            "other-route",
            "POST /v1/execution/node/1/heartbeat",
            replay_ok(&json!({"outcome":"idle"})),
            false,
        ),
    ]
}

fn replay_ok(data: &JsonValue) -> JsonValue {
    json!({"status":"ok","data":data})
}

async fn remote_replay_json(pool: &sqlx::SqlitePool, key: &str) -> JsonValue {
    let response: String = sqlx::query_scalar(
        "SELECT response_json FROM remote_idempotency_keys WHERE idempotency_key = ?",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap();
    serde_json::from_str(&response).unwrap()
}

#[tokio::test]
async fn nvidia_profile_migration_preserves_every_existing_profile_field() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(28).run(&pool).await.unwrap();

    sqlx::query(
        "UPDATE video_profiles SET retired_at = '2026-07-29T00:00:00Z' \
         WHERE name = 'default-hevc'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE video_profiles SET preset = '9', tune = 'vq' \
         WHERE name = 'default-av1'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let before = legacy_video_profile_snapshot(&pool).await;
    embedded_migrator().run(&pool).await.unwrap();
    let after = legacy_video_profile_snapshot(&pool).await;

    assert_eq!(after, before);
    let retired: Option<String> =
        sqlx::query_scalar("SELECT retired_at FROM video_profiles WHERE name = 'default-hevc'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(retired.as_deref(), Some("2026-07-29T00:00:00Z"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM video_profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 6);
}

/// Migration 0030 rebuilds `video_profiles` again, so every row an operator
/// wrote under 0029 has to project straight through: `preset` retained, the new
/// `qp` null, and the existing quality field and decode backend untouched. A row
/// silently reset to a column default would change how an existing profile
/// encodes without anyone editing it.
#[tokio::test]
async fn vaapi_profile_migration_preserves_existing_profile_rows() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(29).run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO video_profiles \
         (id, name, target_codec, encoder, crf, preset, tune, output_container, \
          copy_compatible, decode_backend) \
         VALUES ('vp-legacy-x265', 'legacy-x265', 'hevc', 'libx265', 19, 'veryslow', \
                 'grain', 'mp4', 1, 'software')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO video_profiles \
         (id, name, target_codec, encoder, cq, preset, decode_backend) \
         VALUES ('vp-legacy-nvenc', 'legacy-nvenc', 'hevc', 'hevc_nvenc', 21, 'p6', 'nvidia')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let before = accelerated_video_profile_snapshot(&pool).await;
    embedded_migrator().run(&pool).await.unwrap();
    let after = accelerated_video_profile_snapshot(&pool).await;

    assert_eq!(after, before);
    let null_qp: i64 = sqlx::query_scalar("SELECT count(*) FROM video_profiles WHERE qp IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM video_profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    // Six migration-0014 seeds plus the two rows above: nothing dropped, and no
    // pre-existing row acquired a qp.
    assert_eq!((total, null_qp), (8, 8));
    let presets: i64 =
        sqlx::query_scalar("SELECT count(*) FROM video_profiles WHERE preset IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(presets, 8, "every pre-0030 row keeps its preset");
}

async fn legacy_video_profile_snapshot(pool: &sqlx::SqlitePool) -> String {
    sqlx::query_scalar(
        "SELECT json_group_array(json_object( \
           'id', id, 'name', name, 'target_codec', target_codec, 'encoder', encoder, \
           'crf', crf, 'preset', preset, 'tune', tune, 'codec_profile', codec_profile, \
           'codec_level', codec_level, 'pixel_format', pixel_format, \
           'max_width', max_width, 'max_height', max_height, \
           'output_container', output_container, 'copy_compatible', copy_compatible, \
           'retired_at', retired_at)) \
         FROM (SELECT * FROM video_profiles ORDER BY id)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The 0029 shape: every column that exists on both sides of the accelerator
/// migrations (0030 `VideoToolbox`, 0032 VAAPI), so the comparison is a projection
/// check rather than a restatement of the new table's columns.
async fn accelerated_video_profile_snapshot(pool: &sqlx::SqlitePool) -> String {
    sqlx::query_scalar(
        "SELECT json_group_array(json_object( \
           'id', id, 'name', name, 'target_codec', target_codec, 'encoder', encoder, \
           'crf', crf, 'cq', cq, 'preset', preset, 'tune', tune, \
           'codec_profile', codec_profile, 'codec_level', codec_level, \
           'pixel_format', pixel_format, 'max_width', max_width, 'max_height', max_height, \
           'output_container', output_container, 'copy_compatible', copy_compatible, \
           'retired_at', retired_at, 'decode_backend', decode_backend)) \
         FROM (SELECT * FROM video_profiles ORDER BY id)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn backend_neutral_migration_tags_nvidia_capability_and_preserves_claim() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(30).run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO workers \
         (id, name, kind, status, registered_at, last_seen_at, epoch) \
         VALUES (411, 'gpu-worker', 'local', 'active', '2026-07-29T00:00:00Z', \
                 '2026-07-29T00:00:00Z', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let descriptor = serde_json::json!({
        "accelerator": {
            "hardware_token": "nvidia:GPU-example",
            "device_uuid": "GPU-example",
            "device_name": "Example GPU",
            "driver_version": "1",
            "encoders": ["hevc_nvenc"],
            "decoders": ["h264_cuvid"],
            "max_sessions": 4
        }
    });
    sqlx::query(
        "INSERT INTO worker_capabilities \
         (worker_id, operation, codecs, hardware, artifact_access, extra) \
         VALUES (411, 'transcode_video', '[]', '[\"nvidia:GPU-example\"]', '[]', ?)",
    )
    .bind(descriptor.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO accelerator_claims \
         (hardware_token, backend, worker_id, boot_id, supervisor_pid, \
          supervisor_start_ticks, process_group_id, capacity, claimed_at) \
         VALUES ('nvidia:GPU-example', 'nvidia', 411, 'boot', 100, 12345, 100, 4, \
                 '2026-07-29T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    embedded_migrator().run(&pool).await.unwrap();

    let backend: String = sqlx::query_scalar(
        "SELECT json_extract(extra, '$.accelerator.backend') \
         FROM worker_capabilities WHERE worker_id = 411",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(backend, "nvidia");
    let identity: Option<String> = sqlx::query_scalar(
        "SELECT supervisor_start_identity FROM accelerator_claims \
         WHERE hardware_token = 'nvidia:GPU-example'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(identity.as_deref(), Some("linux-proc-ticks:12345"));
}

#[tokio::test]
async fn staged_artifact_commit_migration_preserves_seeded_file_version_links() {
    let migration_path = migrations_dir().join("0012_staged_artifact_commit.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();

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

    embedded_migrator().run(&pool).await.unwrap();

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

    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();

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

    embedded_migrator().run(&pool).await.unwrap();

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

#[tokio::test]
async fn policy_verification_migration_preserves_workflow_progress() {
    let migration_path = migrations_dir().join("0026_policy_artifact_verification.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();

    migrator_through(25).run(&pool).await.unwrap();

    let (file_version_id, job_id) = seed_legacy_workflow_progress(&pool).await;
    seed_legacy_artifact_verification(&pool, file_version_id).await;

    embedded_migrator().run(&pool).await.unwrap();

    let summary: (String, Option<i64>) = sqlx::query_as(
        "SELECT outcome, artifact_verification_id \
         FROM workflow_file_phase_summaries \
         WHERE job_id = ? AND phase_ordinal = 0 AND branch_id = 'movie'",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let history: String = sqlx::query_scalar(
        "SELECT outcome FROM workflow_file_run_history \
         WHERE job_id = ? AND branch_id = 'movie' AND phase_ordinal = 0",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let verification_owner: (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT workflow_ticket_id, workflow_lease_id FROM artifact_verifications")
            .fetch_one(&pool)
            .await
            .unwrap();
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(summary, ("skipped".to_owned(), None));
    assert_eq!(history, "skipped");
    assert_eq!(verification_owner, (None, None));
    assert_eq!(violations, Vec::<(String, i64, String, i64)>::new());
}

#[tokio::test]
async fn sliding_window_migration_backfills_legacy_progress_and_accepts_blocked() {
    let migration_path = migrations_dir().join("0028_sliding_file_window.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );

    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(27).run(&pool).await.unwrap();
    let (_file_version_id, job_id) = seed_legacy_workflow_progress(&pool).await;

    embedded_migrator().run(&pool).await.unwrap();

    let preserved: String = sqlx::query_scalar(
        "SELECT outcome FROM workflow_file_run_history \
         WHERE job_id = ? AND branch_id = 'movie' AND phase_ordinal = 0",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let window: i64 = sqlx::query_scalar(
        "SELECT max_in_flight_files FROM workflow_file_windows WHERE job_id = ?",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let progress: (String, i64, i64) = sqlx::query_as(
        "SELECT state, next_phase_ordinal, admission_tier \
         FROM workflow_file_progress WHERE job_id = ? AND branch_id = 'movie'",
    )
    .bind(job_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_file_run_history \
         (job_id, branch_id, phase_ordinal, outcome) VALUES (?, 'movie', 1, 'blocked')",
    )
    .bind(job_id)
    .execute(&pool)
    .await
    .unwrap();
    let outcomes: Vec<String> = sqlx::query_scalar(
        "SELECT outcome FROM workflow_file_run_history \
         WHERE job_id = ? AND branch_id = 'movie' ORDER BY phase_ordinal",
    )
    .bind(job_id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(preserved, "skipped");
    assert_eq!(window, 4);
    assert_eq!(progress, ("active".to_owned(), 1, 0));
    assert_eq!(outcomes, ["skipped", "blocked"]);
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(violations.is_empty());
}

#[tokio::test]
async fn audio_synthesis_lineage_migration_allows_sequential_versions_of_one_asset() {
    let migration_path = migrations_dir().join("0027_audio_synthesis_asset_lineage.sql");
    assert!(
        migration_path.is_file(),
        "{} must exist before the upgrade path can be exercised",
        migration_path.display()
    );
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    let pool = create_uninitialized_pool(&url).await.unwrap();
    migrator_through(26).run(&pool).await.unwrap();

    let (asset_id, versions, locations, snapshots) = seed_synthesis_versions(&pool).await;
    insert_synthesis_operation(
        &pool,
        SynthesisOperationSeed {
            operation_key: "synthesis:first",
            target_path: "/media/first.mkv",
            source_version_id: versions[0],
            source_snapshot_id: snapshots[0],
            result_asset_id: asset_id,
            result_version_id: versions[1],
            result_location_id: locations[1],
            result_snapshot_id: snapshots[1],
        },
    )
    .await;

    embedded_migrator().run(&pool).await.unwrap();

    insert_synthesis_operation(
        &pool,
        SynthesisOperationSeed {
            operation_key: "synthesis:second",
            target_path: "/media/second.mkv",
            source_version_id: versions[1],
            source_snapshot_id: snapshots[1],
            result_asset_id: asset_id,
            result_version_id: versions[2],
            result_location_id: locations[2],
            result_snapshot_id: snapshots[2],
        },
    )
    .await;
    let asset_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT result_file_asset_id FROM audio_synthesis_operations ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let violations: Vec<(String, i64, String, i64)> = sqlx::query_as("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(asset_ids, vec![asset_id, asset_id]);
    assert_eq!(violations, Vec::<(String, i64, String, i64)>::new());
}

async fn seed_synthesis_versions(pool: &sqlx::SqlitePool) -> (i64, Vec<i64>, Vec<i64>, Vec<i64>) {
    let now = "2026-07-27T00:00:00Z";
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let mut versions = Vec::new();
    let mut locations = Vec::new();
    let mut snapshots = Vec::new();
    for ordinal in 0..3 {
        let produced_by = if ordinal == 0 {
            "external_observed"
        } else {
            "staged_commit"
        };
        let produced_from_version_id = versions.last().copied();
        let version_id = sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, produced_from_version_id, \
              created_at) \
             VALUES (?, ?, 3, ?, ?, ?)",
        )
        .bind(asset_id)
        .bind(format!("blake3:version-{ordinal}"))
        .bind(produced_by)
        .bind(produced_from_version_id)
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let location_id = sqlx::query(
            "INSERT INTO file_locations (file_version_id, kind, value, observed_at) \
             VALUES (?, 'local_path', ?, ?)",
        )
        .bind(version_id)
        .bind(format!("/media/version-{ordinal}.mkv"))
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let snapshot_id = sqlx::query(
            "INSERT INTO media_snapshots (file_version_id, probed_at, payload) \
             VALUES (?, ?, '{}')",
        )
        .bind(version_id)
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
        versions.push(version_id);
        locations.push(location_id);
        snapshots.push(snapshot_id);
    }
    (asset_id, versions, locations, snapshots)
}

struct SynthesisOperationSeed<'a> {
    operation_key: &'a str,
    target_path: &'a str,
    source_version_id: i64,
    source_snapshot_id: i64,
    result_asset_id: i64,
    result_version_id: i64,
    result_location_id: i64,
    result_snapshot_id: i64,
}

async fn insert_synthesis_operation(pool: &sqlx::SqlitePool, seed: SynthesisOperationSeed<'_>) {
    sqlx::query(
        "INSERT INTO audio_synthesis_operations \
         (operation_key, planned_operation_id, source_file_version_id, \
          source_media_snapshot_id, target_codec, target_channels, container, target_path, \
          state, result_file_asset_id, result_file_version_id, result_file_location_id, \
          result_media_snapshot_id, created_at, finished_at) \
         VALUES (?, ?, ?, ?, 'aac', 2, 'mkv', ?, 'committed', ?, ?, ?, ?, ?, ?)",
    )
    .bind(seed.operation_key)
    .bind(seed.operation_key)
    .bind(seed.source_version_id)
    .bind(seed.source_snapshot_id)
    .bind(seed.target_path)
    .bind(seed.result_asset_id)
    .bind(seed.result_version_id)
    .bind(seed.result_location_id)
    .bind(seed.result_snapshot_id)
    .bind("2026-07-27T00:00:00Z")
    .bind("2026-07-27T00:00:01Z")
    .execute(pool)
    .await
    .unwrap();
}

async fn seed_legacy_workflow_progress(pool: &sqlx::SqlitePool) -> (i64, i64) {
    let now = "2026-07-27T00:00:00Z";
    let file_asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(now)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version_id = sqlx::query(
        "INSERT INTO file_versions \
         (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
         VALUES (?, 'blake3:source', 3, 'external_observed', ?)",
    )
    .bind(file_asset_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let job_id = sqlx::query(
        "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
         VALUES ('policy', 'open', 0, ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO workflow_file_run_starts \
         (job_id, branch_id, starting_file_version_id, starting_phase_ordinal) \
         VALUES (?, 'movie', ?, 1)",
    )
    .bind(job_id)
    .bind(file_version_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_file_run_history \
         (job_id, branch_id, phase_ordinal, outcome) \
         VALUES (?, 'movie', 0, 'skipped')",
    )
    .bind(job_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workflow_file_phase_summaries \
         (job_id, phase_ordinal, branch_id, ticket_ids, outcome, created_at) \
         VALUES (?, 0, 'movie', '[]', 'skipped', ?)",
    )
    .bind(job_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
    (file_version_id, job_id)
}

async fn seed_legacy_artifact_verification(pool: &sqlx::SqlitePool, file_version_id: i64) {
    let now = "2026-07-27T00:00:00Z";
    let worker_id = sqlx::query(
        "INSERT INTO workers \
         (name, kind, status, registered_at, last_seen_at) \
         VALUES ('legacy-verifier', 'synthetic', 'active', ?, ?)",
    )
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let artifact_handle_id = sqlx::query(
        "INSERT INTO artifact_handles \
         (size_bytes, checksum, privacy_class, durability_class, allowed_access_modes, \
          mutability, source_lineage, created_at, file_version_id) \
         VALUES (3, 'blake3:source', 'internal', 'durable', '[\"local_path\"]', \
                 'immutable', '{}', ?, ?)",
    )
    .bind(now)
    .bind(file_version_id)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let artifact_location_id = sqlx::query(
        "INSERT INTO artifact_locations \
         (artifact_handle_id, kind, value, observed_at) \
         VALUES (?, 'local_path', '/media/source.mkv', ?)",
    )
    .bind(artifact_handle_id)
    .bind(now)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    sqlx::query(
        "INSERT INTO artifact_verifications \
         (artifact_handle_id, artifact_location_id, path, worker_id, status, \
          expected_size_bytes, expected_checksum, observed_size_bytes, observed_checksum, \
          report, started_at, finished_at) \
         VALUES (?, ?, '/media/source.mkv', ?, 'succeeded', 3, 'blake3:source', \
                 3, 'blake3:source', '{}', ?, ?)",
    )
    .bind(artifact_handle_id)
    .bind(artifact_location_id)
    .bind(worker_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await
    .unwrap();
}

#[test]
fn migrator_versions_are_strictly_increasing() {
    let versions: Vec<i64> = embedded_migrator().iter().map(|m| m.version).collect();
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
