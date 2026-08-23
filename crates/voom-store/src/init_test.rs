use std::borrow::Cow;

use super::*;
use crate::pool::connect;
use crate::repo::audit::events::{EventFilter, EventRepo, Page, SqliteEventRepo};
use crate::schema::{expected_migrations, probe_schema};
use sqlx::Connection;
use sqlx::migrate::{Migrate, Migration, MigrationType, Migrator};
use voom_events::{Event, EventKind};

#[tokio::test]
async fn init_in_memory_applies_every_embedded_migration() {
    let pool = connect("sqlite::memory:").await.unwrap();
    let report = init_on(&pool).await.unwrap();
    assert!(!report.already_initialized);
    assert_eq!(expected_migrations(), 5);
    assert_eq!(report.migrations_applied, expected_migrations());
}

#[tokio::test]
async fn init_emits_schema_initialized_on_fresh_db() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let report = init(&url).await.unwrap();
    assert!(!report.already_initialized);
    assert!(report.migrations_applied > 0);

    // Read the events table — there should be exactly one schema.initialized row.
    let pool = connect(&url).await.unwrap();
    let repo = SqliteEventRepo::new(pool);
    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "exactly one schema.initialized row on fresh init"
    );
}

#[tokio::test]
async fn schema_initialized_event_emitted_on_recovery() {
    // Simulates the partial-failure window: migrations committed durably,
    // but the `schema.initialized` event row never landed (e.g. transient
    // I/O on the event-insert transaction, or a crash between migration
    // commit and event append). The events table is append-only, so we
    // can't DELETE the row after the fact — instead, we drive the migrator
    // directly to land the schema with no event row, then call init() to
    // exercise the recovery branch. Pre-fix, that second call's guard
    // (`before_count == 0`) was false, so the row was permanently lost.
    let pool = connect("sqlite::memory:").await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();

    // Sanity: the schema is current, and no schema.initialized row exists.
    assert!(matches!(
        probe_schema(&pool).await.unwrap(),
        SchemaState::Current { .. }
    ));
    let repo = SqliteEventRepo::new(pool.clone());
    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        0,
        "preconditions: migrator ran but no event row was appended"
    );

    let report = init_on(&pool).await.unwrap();
    assert!(
        report.already_initialized,
        "schema is already migrated on the recovery call"
    );
    assert_eq!(
        report.migrations_applied, 0,
        "no new migrations are applied on recovery"
    );

    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "exactly one schema.initialized row after recovery"
    );
    let Event::SchemaInitialized(payload) = &page.items[0].envelope.payload else {
        panic!("expected SchemaInitialized payload");
    };
    assert_eq!(
        payload.migrations_applied,
        expected_migrations(),
        "recovery payload carries the absolute migration count, not the per-call delta"
    );
}

#[tokio::test]
async fn concurrent_inits_never_double_write_schema_initialized() {
    // The recovery path uses a single INSERT ... WHERE NOT EXISTS so two
    // concurrent inits cannot both insert. Spawn N tasks racing on the
    // same file URL and assert exactly one row lands. Without the atomic
    // form, the SELECT-then-INSERT pair would let two tasks both see
    // "no row" and both insert.
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    // Seed the schema once so the racing tasks all exercise the
    // recovery branch (already-initialized + missing event row). The
    // first init below would create the row on its own; clearing that
    // would require a destructive DELETE which the append-only trigger
    // forbids, so instead we land the migrations directly without ever
    // emitting the event.
    let pool = connect(&url).await.unwrap();
    crate::migrator::MIGRATOR.run(&pool).await.unwrap();
    drop(pool);

    let mut handles = Vec::with_capacity(8);
    for _ in 0..8 {
        let url = url.clone();
        handles.push(tokio::spawn(async move { init(&url).await }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let pool = connect(&url).await.unwrap();
    let repo = SqliteEventRepo::new(pool);
    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 16,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "exactly one schema.initialized row even under concurrent inits"
    );
}

#[tokio::test]
async fn init_does_not_emit_when_already_initialized() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    let _ = init(&url).await.unwrap();
    let report = init(&url).await.unwrap();
    assert!(report.already_initialized);

    let pool = connect(&url).await.unwrap();
    let repo = SqliteEventRepo::new(pool);
    let page = repo
        .list(
            EventFilter {
                kind: Some(EventKind::SchemaInitialized),
                ..EventFilter::default()
            },
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "second init must not double-write");
}

#[tokio::test]
async fn init_is_idempotent_on_same_pool() {
    let pool = connect("sqlite::memory:").await.unwrap();
    let first = init_on(&pool).await.unwrap();
    let second = init_on(&pool).await.unwrap();
    assert!(!first.already_initialized);
    assert!(second.already_initialized);
    assert_eq!(second.migrations_applied, 0);
}

#[tokio::test]
async fn single_shot_replacement_has_no_polling() {
    let pool = connect("sqlite::memory:").await.unwrap();
    // Seed a Partial fixture ahead of the transaction so a full rollback
    // (migration 1 fails immediately) returns to exactly this state, not
    // Uninitialized — CREATE TABLE IF NOT EXISTS inside the transaction is a
    // no-op against an already-existing table, so it isn't undone by rollback.
    sqlx::query(
        "CREATE TABLE _sqlx_migrations ( \
         version BIGINT PRIMARY KEY, \
         description TEXT NOT NULL, \
         installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
         success BOOLEAN NOT NULL, \
         checksum BLOB NOT NULL, \
         execution_time BIGINT NOT NULL \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let broken = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            1,
            Cow::Borrowed("broken"),
            MigrationType::Simple,
            Cow::Borrowed("NOT VALID SQL;"),
            false,
        )]),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };

    let start = tokio::time::Instant::now();
    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
    tx.ensure_migrations_table().await.unwrap();
    let migrate_result = broken.run_direct(&mut *tx).await;
    assert!(migrate_result.is_err(), "invalid SQL must fail run_direct");
    drop(tx); // rolls back, releasing the lock
    drop(conn); // return the sole :memory: pool connection before probing
    let after = probe_schema(&pool).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        matches!(after, SchemaState::Partial { .. }),
        "expected Partial after rollback to the pre-seeded fixture, got {after:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(25),
        "single-shot classification must not poll or sleep: took {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn locked_migration_true_race_reports_zero_applied() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();

    // Peer A: fully migrate on a directly-held, uncommitted transaction.
    let mut conn_a = pool.acquire().await.unwrap();
    let mut held = conn_a.begin_with("BEGIN IMMEDIATE").await.unwrap();
    MIGRATOR.run_direct(&mut *held).await.unwrap();

    // Peer B: spawn a real run_migrations_on against the same pool.
    let pool_b = pool.clone();
    let peer_b = tokio::spawn(async move { run_migrations_on(&pool_b).await });

    // Release peer A's lock. Peer B's own `locked_before_count` read always
    // happens either after this commit (sees everything already applied) or
    // blocks on peer A's BEGIN IMMEDIATE until this same commit releases it
    // — there is no interleaving that produces a different observable
    // outcome for peer B, so this test needs no sleep or rendezvous signal.
    held.commit().await.unwrap();

    let report = peer_b.await.unwrap().unwrap();
    assert_eq!(report.migrations_applied, 0);
    assert!(report.already_initialized);
}

#[tokio::test]
async fn busy_timeout_exhaustion_surfaces_database_error() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();

    let mut conn1 = pool.acquire().await.unwrap();
    let _held = conn1.begin_with("BEGIN IMMEDIATE").await.unwrap();

    let mut conn2 = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA busy_timeout = 0")
        .execute(&mut *conn2)
        .await
        .unwrap();
    // Load-bearing: return conn2 to the pool's idle queue *before* calling
    // run_migrations_on, whose own internal pool.acquire() calls must reuse
    // this specific (busy_timeout = 0) connection rather than spin up a
    // fresh one with the default 30s busy_timeout. conn1 stays checked out
    // for the whole test and no other connection is ever created, so conn2
    // is the only idle connection available when that happens.
    drop(conn2);

    let result = run_migrations_on(&pool).await;
    assert!(
        matches!(result, Err(VoomError::Database { .. })),
        "expected a busy-timeout Database error, got {result:?}"
    );
}

#[tokio::test]
async fn all_or_nothing_rollback_on_migration_failure() {
    let pool = connect("sqlite::memory:").await.unwrap();

    let broken = Migrator {
        migrations: Cow::Owned(vec![
            Migration::new(
                1,
                Cow::Borrowed("first"),
                MigrationType::Simple,
                Cow::Borrowed("CREATE TABLE first_migration_marker (id INTEGER PRIMARY KEY);"),
                false,
            ),
            Migration::new(
                2,
                Cow::Borrowed("second_invalid"),
                MigrationType::Simple,
                Cow::Borrowed("NOT VALID SQL;"),
                false,
            ),
        ]),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };

    let mut conn = pool.acquire().await.unwrap();
    let mut tx = conn.begin_with("BEGIN IMMEDIATE").await.unwrap();
    tx.ensure_migrations_table().await.unwrap();
    let migrate_result = broken.run_direct(&mut *tx).await;
    assert!(migrate_result.is_err(), "second migration must fail");
    drop(tx); // rolls back, including migration 1's nested SAVEPOINT
    drop(conn); // return the sole :memory: pool connection before querying

    let exists: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = \
         'first_migration_marker'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        exists, 0,
        "migration 1's table must not survive rollback of migration 2's failure"
    );
}

#[tokio::test]
async fn init_refuses_when_db_schema_is_too_new() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, installed_on, success, checksum, execution_time) \
         VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let err = init_on(&pool).await.unwrap_err();
    assert_eq!(err.code(), "DB_SCHEMA_TOO_NEW");
    assert!(format!("{err}").contains("cannot init"));
}

#[tokio::test]
async fn probe_after_init_then_extra_row_returns_too_new() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, installed_on, success, checksum, execution_time) \
         VALUES (99999, 'synthetic-future', strftime('%s','now'), 1, X'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    match probe_schema(&pool).await.unwrap() {
        SchemaState::TooNew { applied, expected } => {
            assert_eq!(expected, expected_migrations());
            assert!(applied > expected);
        }
        other => panic!("expected TooNew, got {other:?}"),
    }
}

#[tokio::test]
async fn probe_after_init_then_checksum_mutation_returns_too_new() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();
    assert!(matches!(
        probe_schema(&pool).await.unwrap(),
        SchemaState::Current { .. }
    ));

    sqlx::query("UPDATE _sqlx_migrations SET checksum = X'DEADBEEF' WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();

    match probe_schema(&pool).await.unwrap() {
        SchemaState::TooNew { applied, expected } => {
            assert_eq!(applied, expected, "count unchanged; only checksum differs");
        }
        other => panic!("expected TooNew (checksum drift), got {other:?}"),
    }
}

#[tokio::test]
async fn probe_returns_dirty_when_known_version_row_marked_failed() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();

    match probe_schema(&pool).await.unwrap() {
        SchemaState::Dirty {
            failed_version,
            applied,
            expected,
        } => {
            assert_eq!(
                failed_version, 1,
                "failed_version must point at the dirty row"
            );
            assert_eq!(
                applied,
                expected_migrations() - 1,
                "only the marked-failed row should be missing from the success count"
            );
            assert_eq!(expected, expected_migrations());
        }
        other => panic!("expected Dirty, got {other:?}"),
    }
}

#[tokio::test]
async fn init_refuses_when_schema_is_dirty() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    // Synthesize a dirty state: known version with success=0.
    sqlx::query("UPDATE _sqlx_migrations SET success = 0 WHERE version = 1")
        .execute(&pool)
        .await
        .unwrap();

    let err = init_on(&pool).await.unwrap_err();
    assert_eq!(err.code(), "DB_DIRTY_MIGRATION");
    let msg = format!("{err}");
    assert!(
        msg.contains("version 1"),
        "error must name the dirty version: {msg}"
    );
    assert!(
        msg.contains("DELETE FROM _sqlx_migrations") || msg.contains("restore"),
        "error must point at manual remediation: {msg}"
    );
}

#[tokio::test]
async fn probe_returns_too_new_when_failed_unknown_version_row_present() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO _sqlx_migrations \
         (version, description, installed_on, success, checksum, execution_time) \
         VALUES (99999, 'failed-future', strftime('%s','now'), 0, X'00', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    match probe_schema(&pool).await.unwrap() {
        SchemaState::TooNew { applied, .. } => {
            assert_eq!(applied, expected_migrations(), "only successful row counts");
        }
        other => panic!("expected TooNew, got {other:?}"),
    }
}

#[tokio::test]
async fn probe_after_init_then_corrupt_schema_meta_returns_migration_error() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    sqlx::query("DROP TABLE schema_meta")
        .execute(&pool)
        .await
        .unwrap();

    let err = probe_schema(&pool).await.unwrap_err();
    assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
}

#[tokio::test]
async fn probe_after_init_then_corrupt_schema_init_at_value_returns_migration_error() {
    let pool = connect("sqlite::memory:").await.unwrap();
    init_on(&pool).await.unwrap();

    sqlx::query("UPDATE schema_meta SET value = 'not-a-timestamp' WHERE key = 'schema_init_at'")
        .execute(&pool)
        .await
        .unwrap();

    let err = probe_schema(&pool).await.unwrap_err();
    assert_eq!(err.code(), "DB_PARTIAL_SCHEMA");
}

#[tokio::test]
async fn init_from_partial_state_reports_delta_not_total() {
    // Synthesize a Partial state: _sqlx_migrations exists but has zero
    // success rows. In Sprint 0 the delta equals the total; this pins
    // the delta-counting code path so Sprint 1+ can add a real partial
    // case without rewriting init logic.
    let pool = connect("sqlite::memory:").await.unwrap();
    sqlx::query(
        "CREATE TABLE _sqlx_migrations ( \
         version BIGINT PRIMARY KEY, \
         description TEXT NOT NULL, \
         installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
         success BOOLEAN NOT NULL, \
         checksum BLOB NOT NULL, \
         execution_time BIGINT NOT NULL \
         )",
    )
    .execute(&pool)
    .await
    .unwrap();

    let report = init_on(&pool).await.unwrap();
    assert!(!report.already_initialized);
    assert_eq!(report.migrations_applied, expected_migrations());
}

use sqlx::Row as _;

// --- Migration 0037 (issue #477, ADR 0071) --------------------------------

/// Apply only the squashed base schema (logical migrations through 0036),
/// leaving the database one migration behind so a test can seed pre-0037
/// rows and then run the real `init` upgrade path.
async fn apply_base_schema_only(pool: &sqlx::SqlitePool) {
    use sqlx::migrate::{Migration, MigrationType, Migrator};
    use std::borrow::Cow;
    let embedded = crate::test_support::embedded_migrator();
    let base = Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            embedded.migrations[0].version,
            Cow::Borrowed(embedded.migrations[0].description.as_ref()),
            MigrationType::Simple,
            Cow::Borrowed(embedded.migrations[0].sql.as_ref()),
            false,
        )]),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    let mut conn = pool.acquire().await.unwrap();
    base.run(&mut *conn).await.unwrap();
}

const ALL_REASON_CODES: &[&str] = &[
    "selected",
    "no_ready_ticket",
    "missing_capability",
    "missing_grant",
    "operation_denied",
    "worker_not_executable",
    "node_not_executable",
    "heartbeat_expired",
    "unsupported_artifact_access",
    "worker_capacity_full",
    "node_capacity_full",
    "no_eligible_candidate",
];

#[tokio::test]
async fn migration_0037_preserves_every_scheduler_decision_column_index_and_sequence() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();
    apply_base_schema_only(&pool).await;

    // A legacy idle decision with an explicit high id so the AUTOINCREMENT
    // sequence has real state to preserve.
    sqlx::query(
        "INSERT INTO scheduler_decisions \
         (id, created_at, updated_at, first_seen_at, last_seen_at, decision_kind, \
          request_source, idempotency_key, request_node_id, request_worker_id, ticket_id, \
          selected_worker_id, selected_node_id, selected_lease_id, outcome, reason_code, \
          summary, candidate_count, selected_score, suppressed_count, suppression_key, \
          explanation_json) \
         VALUES (41, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 'idle', 'remote_acquire', \
                 'legacy-key', NULL, NULL, NULL, NULL, NULL, NULL, 'idle', \
                 'no_ready_ticket', 'legacy row', 0, NULL, 3, \
                 'remote_acquire:node:1:worker:2:reason:no_ready_ticket:ops:none:bucket:2', \
                 '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let report = crate::init::init_on(&pool).await.unwrap();
    // 0037 (scheduling evidence), 0038 (commit intents), 0041 (observation
    // evidence), and 0042 (media dispatch preflight) on the base-schema-only
    // database.
    assert_eq!(report.migrations_applied, 4);

    // Every pre-existing column value survives, and the new column is NULL.
    let row = sqlx::query("SELECT * FROM scheduler_decisions WHERE id = 41")
        .fetch_one(&pool)
        .await
        .unwrap();
    let columns: Vec<&str> = row.columns().iter().map(sqlx::Column::name).collect();
    assert!(columns.contains(&"access_evidence"));
    let access_evidence: Option<String> = row.try_get("access_evidence").unwrap();
    assert_eq!(access_evidence, None);
    let summary: String = row.try_get("summary").unwrap();
    assert_eq!(summary, "legacy row");
    let suppressed_count: i64 = row.try_get("suppressed_count").unwrap();
    assert_eq!(suppressed_count, 3);

    // All eight pre-existing indexes still exist.
    for index in [
        "scheduler_decisions_by_created_at",
        "scheduler_decisions_by_ticket",
        "scheduler_decisions_by_request_worker",
        "scheduler_decisions_by_request_node",
        "scheduler_decisions_by_selected_worker",
        "scheduler_decisions_by_selected_node",
        "scheduler_decisions_by_outcome",
        "scheduler_decisions_by_reason_code",
        "scheduler_decisions_by_suppression_key",
    ] {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index' AND name = ?",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "missing index {index}");
    }

    // The AUTOINCREMENT sequence continues past the preserved max id, and
    // every supported reason code is still accepted by the schema.
    for (position, reason) in ALL_REASON_CODES.iter().enumerate() {
        let selected = *reason == "selected";
        let sql = if selected {
            "INSERT INTO scheduler_decisions \
             (created_at, updated_at, first_seen_at, last_seen_at, decision_kind, \
              request_source, request_node_id, request_worker_id, ticket_id, \
              selected_worker_id, selected_node_id, outcome, reason_code, summary, \
              candidate_count, explanation_json) \
             VALUES ('1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 'lease_acquire', \
                     'remote_acquire', NULL, NULL, NULL, NULL, NULL, 'selected', ?, \
                     'shape probe', 1, '{}')"
        } else {
            "INSERT INTO scheduler_decisions \
             (created_at, updated_at, first_seen_at, last_seen_at, decision_kind, \
              request_source, request_node_id, request_worker_id, ticket_id, \
              selected_worker_id, selected_node_id, outcome, reason_code, summary, \
              candidate_count, explanation_json) \
             VALUES ('1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', \
                     '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', 'no_candidate', \
                     'remote_acquire', NULL, NULL, NULL, NULL, NULL, \
                     'no_eligible_candidate', ?, 'shape probe', 1, '{}')"
        };
        let result = sqlx::query(sql).bind(reason).execute(&pool).await;
        assert!(result.is_ok(), "reason {reason} rejected: {result:?}");
        let expected_id = 42 + i64::try_from(position).unwrap();
        let inserted = result.unwrap().last_insert_rowid();
        assert_eq!(inserted, expected_id, "sequence must continue for {reason}");
    }
}

#[tokio::test]
async fn migration_0037_guard_rejects_legacy_plan_rows_before_any_schema_mutation() {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = crate::test_support::sqlite_url_for(tmp.path());
    let pool = crate::test_support::create_uninitialized_pool(&url)
        .await
        .unwrap();
    apply_base_schema_only(&pool).await;

    // Minimal FK parents, then one legacy-representation plan row.
    sqlx::query(
        "INSERT INTO nodes \
         (id, name, kind, status, registered_at, last_seen_at, heartbeat_ttl_seconds, \
          auth_token_hash, auth_token_hint, metadata) \
         VALUES (1, 'node-1', 'synthetic', 'registered', '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z', 60, 'token-hash', 'hint', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workers (id, name, kind, status, node_id, registered_at, last_seen_at) \
         VALUES (1, 'worker-1', 'remote', 'registered', 1, '1970-01-01T00:00:00Z', \
                 '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, state, priority, created_at, updated_at) \
         VALUES (1, 'guard-test', 'open', 0, '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tickets \
         (id, job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (1, 1, 'guard-test', 'leased', 0, '{}', 1, 3, \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO leases \
         (id, ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
          ttl_seconds) \
         VALUES (1, 1, 1, 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:01:00Z', \
                 '1970-01-01T00:00:00Z', 60)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO artifact_access_plans \
         (id, lease_id, ticket_id, worker_id, node_id, input_handles, output_handles, \
          selected_access_mode, status, evidence, created_at, updated_at) \
         VALUES (1, 1, 1, 1, 1, '[]', '[]', 'shared_mount', 'selected', '{}', \
                 '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let err = crate::init::init_on(&pool).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("_0037_legacy_artifact_access_plan_rows_present"),
        "guard must name itself: {err}"
    );

    // Nothing was mutated: the legacy table shape is untouched and migration
    // 0037 is not recorded as applied.
    let plan_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' \
         AND name = 'artifact_access_plans'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(plan_sql.contains("selected_access_mode"));
    assert!(!plan_sql.contains("owner_node_id"));
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_access_plans")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}
