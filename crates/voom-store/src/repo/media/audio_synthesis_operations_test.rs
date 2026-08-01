use time::OffsetDateTime;
use voom_core::{FileVersionId, LeaseId, MediaSnapshotId};

use super::*;

const NOW: &str = "1970-01-01T00:00:00Z";

struct Fixture {
    pool: sqlx::SqlitePool,
    repo: SqliteAudioSynthesisOperationRepo,
    source_file_version_id: FileVersionId,
    source_media_snapshot_id: MediaSnapshotId,
    lease_id: LeaseId,
    worker_id: u64,
    _tmp: voom_test_support::TempDatabase,
}

async fn fixture() -> Fixture {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let worker_id = insert_worker(&pool).await;
    let lease_id = insert_lease(&pool, worker_id).await;
    let (source_file_version_id, source_media_snapshot_id) = insert_source(&pool, worker_id).await;
    Fixture {
        pool: pool.clone(),
        repo: SqliteAudioSynthesisOperationRepo::new(pool),
        source_file_version_id,
        source_media_snapshot_id,
        lease_id,
        worker_id,
        _tmp: tmp,
    }
}

async fn insert_worker(pool: &sqlx::SqlitePool) -> u64 {
    sqlx::query(
        "INSERT INTO workers (name, kind, status, registered_at, last_seen_at) \
         VALUES ('synth', 'synthetic', 'active', ?, ?)",
    )
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid()
    .try_into()
    .unwrap()
}

async fn insert_lease(pool: &sqlx::SqlitePool, worker_id: u64) -> LeaseId {
    let job_id = sqlx::query(
        "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
         VALUES ('audio-synthesis-test', 'open', 0, ?, ?)",
    )
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let ticket_id = sqlx::query(
        "INSERT INTO tickets \
         (job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (?, 'audio-synthesis-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    LeaseId(
        sqlx::query(
            "INSERT INTO leases \
             (ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
              ttl_seconds) VALUES (?, ?, 'held', ?, '1970-01-01T01:00:00Z', ?, 3600)",
        )
        .bind(ticket_id)
        .bind(i64::try_from(worker_id).unwrap())
        .bind(NOW)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    )
}

async fn insert_source(
    pool: &sqlx::SqlitePool,
    worker_id: u64,
) -> (FileVersionId, MediaSnapshotId) {
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let file_version_id = FileVersionId(
        sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
             VALUES (?, 'sha256:source', 10, 'external_observed', ?)",
        )
        .bind(asset_id)
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let snapshot_id = MediaSnapshotId(
        sqlx::query(
            "INSERT INTO media_snapshots (file_version_id, probed_by, probed_at, payload) \
             VALUES (?, ?, ?, '{}')",
        )
        .bind(i64::try_from(file_version_id.0).unwrap())
        .bind(i64::try_from(worker_id).unwrap())
        .bind(NOW)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    (file_version_id, snapshot_id)
}

impl Fixture {
    fn operation(&self) -> NewAudioSynthesisOperation {
        NewAudioSynthesisOperation {
            operation_key: "synthesis:v1:key".to_owned(),
            planned_operation_id: "node-1".to_owned(),
            source_file_version_id: self.source_file_version_id,
            source_media_snapshot_id: self.source_media_snapshot_id,
            target_codec: "aac".to_owned(),
            target_channels: 2,
            container: "mkv".to_owned(),
            target_path: "/committed/movie.audio-aac.mkv".to_owned(),
        }
    }

    fn claim(&self, generation: u32, token: &str) -> NewAudioSynthesisClaim {
        NewAudioSynthesisClaim {
            operation_key: "synthesis:v1:key".to_owned(),
            expected_generation: generation,
            lease_id: self.lease_id,
            claim_token: token.to_owned(),
            expires_at: OffsetDateTime::from_unix_timestamp(10).unwrap(),
        }
    }
}

fn companions() -> Vec<NewAudioSynthesisCompanion> {
    vec![
        NewAudioSynthesisCompanion {
            companion_id: "companion-1".to_owned(),
            source_snapshot_stream_id: "stream-1".to_owned(),
            source_provider_stream_index: 1,
            result_snapshot_stream_id: "companion-1".to_owned(),
        },
        NewAudioSynthesisCompanion {
            companion_id: "companion-3".to_owned(),
            source_snapshot_stream_id: "stream-3".to_owned(),
            source_provider_stream_index: 3,
            result_snapshot_stream_id: "companion-3".to_owned(),
        },
    ]
}

#[tokio::test]
async fn create_planned_replays_the_exact_ordered_companion_set() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;

    let first = fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();
    let replay = fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.operation.state, AudioSynthesisOperationState::Planned);
    assert_eq!(
        first
            .companions
            .iter()
            .map(|companion| companion.companion_id.as_str())
            .collect::<Vec<_>>(),
        vec!["companion-1", "companion-3"]
    );
}

#[tokio::test]
async fn create_planned_rejects_semantic_or_order_drift() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();
    let mut drifted = fixture.operation();
    drifted.target_channels = 1;

    let error = fixture
        .repo
        .create_planned(drifted, &companions(), now)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("persisted descriptors"));

    let mut unordered = companions();
    unordered.swap(0, 1);
    let error = fixture
        .repo
        .create_planned(fixture.operation(), &unordered, now)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid or unordered"));
}

#[tokio::test]
async fn claim_and_generation_fence_dispatch_paths() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();
    let first_claim = fixture.claim(0, "first");
    fixture.repo.acquire_claim(&first_claim, now).await.unwrap();

    let competing = fixture.claim(0, "competing");
    assert!(fixture.repo.acquire_claim(&competing, now).await.is_err());
    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &first_claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: fixture.lease_id,
                worker_id: fixture.worker_id,
                worker_epoch: 7,
                idempotency_key: "synthesis:key:0".to_owned(),
                attempt_directory: "/staging/op/g0".to_owned(),
                staging_path: "/staging/op/g0/result.mkv".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    fixture
        .repo
        .quarantine_and_advance_generation(&first_claim, attempt.id, now)
        .await
        .unwrap();

    let second_claim = fixture.claim(1, "second");
    fixture
        .repo
        .acquire_claim(&second_claim, now)
        .await
        .unwrap();
    fixture
        .repo
        .assert_live_claim(&second_claim, now)
        .await
        .unwrap();
    let second = fixture
        .repo
        .record_dispatch_attempt(
            &second_claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: fixture.lease_id,
                worker_id: fixture.worker_id,
                worker_epoch: 8,
                idempotency_key: "synthesis:key:1".to_owned(),
                attempt_directory: "/staging/op/g1".to_owned(),
                staging_path: "/staging/op/g1/result.mkv".to_owned(),
            },
            now,
        )
        .await
        .unwrap();

    assert_eq!(attempt.status, AudioSynthesisDispatchAttemptStatus::Active);
    assert_eq!(second.generation, 1);
    assert_ne!(attempt.staging_path, second.staging_path);
    assert!(
        fixture
            .repo
            .assert_live_claim(&first_claim, now)
            .await
            .is_err()
    );
    let quarantined: String =
        sqlx::query_scalar("SELECT status FROM audio_synthesis_dispatch_attempts WHERE id = ?")
            .bind(i64::try_from(attempt.id).unwrap())
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(quarantined, "quarantined");
}

#[tokio::test]
async fn audio_synthesis_dispatch_attempt_status_round_trips_durable_vocabulary() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();
    let claim = fixture.claim(0, "status-vocabulary");
    fixture.repo.acquire_claim(&claim, now).await.unwrap();
    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: fixture.lease_id,
                worker_id: fixture.worker_id,
                worker_epoch: 7,
                idempotency_key: "synthesis:status:0".to_owned(),
                attempt_directory: "/staging/op/g0".to_owned(),
                staging_path: "/staging/op/g0/result.mkv".to_owned(),
            },
            now,
        )
        .await
        .unwrap();

    for (stored, expected) in [
        ("active", AudioSynthesisDispatchAttemptStatus::Active),
        ("terminal", AudioSynthesisDispatchAttemptStatus::Terminal),
        (
            "quarantined",
            AudioSynthesisDispatchAttemptStatus::Quarantined,
        ),
        ("quiesced", AudioSynthesisDispatchAttemptStatus::Quiesced),
    ] {
        let (evidence_kind, evidence_at, acknowledged_by) = match expected {
            AudioSynthesisDispatchAttemptStatus::Active
            | AudioSynthesisDispatchAttemptStatus::Quarantined => (None, None, None),
            AudioSynthesisDispatchAttemptStatus::Terminal => {
                (Some("terminal_response"), Some(NOW), None)
            }
            AudioSynthesisDispatchAttemptStatus::Quiesced => (
                Some("operator_acknowledgement"),
                Some(NOW),
                Some("operator"),
            ),
        };
        sqlx::query(
            "UPDATE audio_synthesis_dispatch_attempts \
             SET status = ?, evidence_kind = ?, evidence_at = ?, acknowledged_by = ? \
             WHERE id = ?",
        )
        .bind(stored)
        .bind(evidence_kind)
        .bind(evidence_at)
        .bind(acknowledged_by)
        .bind(i64::try_from(attempt.id).unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();

        let loaded = fixture
            .repo
            .get_dispatch_attempt(attempt.operation_id, attempt.generation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.status, expected);
    }
}

#[tokio::test]
async fn audio_synthesis_dispatch_attempt_status_rejects_unknown_durable_value() {
    let fixture = fixture().await;
    let now = OffsetDateTime::UNIX_EPOCH;
    fixture
        .repo
        .create_planned(fixture.operation(), &companions(), now)
        .await
        .unwrap();
    let claim = fixture.claim(0, "invalid-status");
    fixture.repo.acquire_claim(&claim, now).await.unwrap();
    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &claim,
            &NewAudioSynthesisDispatchAttempt {
                dispatch_lease_id: fixture.lease_id,
                worker_id: fixture.worker_id,
                worker_epoch: 7,
                idempotency_key: "synthesis:invalid-status:0".to_owned(),
                attempt_directory: "/staging/op/g0".to_owned(),
                staging_path: "/staging/op/g0/result.mkv".to_owned(),
            },
            now,
        )
        .await
        .unwrap();
    let mut connection = fixture.pool.acquire().await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = TRUE")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("UPDATE audio_synthesis_dispatch_attempts SET status = ? WHERE id = ?")
        .bind("impossible")
        .bind(i64::try_from(attempt.id).unwrap())
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = FALSE")
        .execute(&mut *connection)
        .await
        .unwrap();
    drop(connection);

    let error = fixture
        .repo
        .get_dispatch_attempt(attempt.operation_id, attempt.generation)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("audio_synthesis_dispatch_attempts.status")
    );
    assert!(error.to_string().contains("impossible"));
}
