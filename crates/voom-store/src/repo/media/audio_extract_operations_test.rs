use time::OffsetDateTime;
use voom_core::{BundleId, FileVersionId, LeaseId, MediaSnapshotId, WorkerId};

use super::*;

const NOW: &str = "1970-01-01T00:00:00Z";

struct Fixture {
    pool: sqlx::SqlitePool,
    repo: SqliteAudioExtractOperationRepo,
    source_file_version_id: FileVersionId,
    source_bundle_id: BundleId,
    source_media_snapshot_id: MediaSnapshotId,
    lease_id: LeaseId,
    worker_id: WorkerId,
    _tmp: tempfile::NamedTempFile,
}

async fn fixture() -> Fixture {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let pool = crate::test_support::fresh_initialized_pool_at(tmp.path())
        .await
        .unwrap();
    let worker_id = WorkerId(
        sqlx::query(
            "INSERT INTO workers \
         (name, kind, status, registered_at, last_seen_at) \
         VALUES ('probe', 'synthetic', 'active', ?, ?)",
        )
        .bind(NOW)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let job_id = sqlx::query(
        "INSERT INTO jobs (kind, state, priority, created_at, updated_at) \
         VALUES ('audio-extract-test', 'open', 0, ?, ?)",
    )
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let ticket_id = sqlx::query(
        "INSERT INTO tickets \
         (job_id, kind, state, priority, payload, attempt, max_attempts, \
          next_eligible_at, created_at, state_changed_at) \
         VALUES (?, 'audio-extract-test', 'leased', 0, '{}', 1, 3, ?, ?, ?)",
    )
    .bind(job_id)
    .bind(NOW)
    .bind(NOW)
    .bind(NOW)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let lease_id = LeaseId(
        sqlx::query(
            "INSERT INTO leases \
             (ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, \
              ttl_seconds) \
             VALUES (?, ?, 'held', ?, '1970-01-01T01:00:00Z', ?, 3600)",
        )
        .bind(ticket_id)
        .bind(i64::try_from(worker_id.0).unwrap())
        .bind(NOW)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let work_id = sqlx::query(
        "INSERT INTO media_works (kind, display_title, created_at) \
         VALUES ('movie', 'Movie', ?)",
    )
    .bind(NOW)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let variant_id = sqlx::query(
        "INSERT INTO media_variants (media_work_id, label, created_at) VALUES (?, 'main', ?)",
    )
    .bind(work_id)
    .bind(NOW)
    .execute(&pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let source_bundle_id = BundleId(
        sqlx::query(
            "INSERT INTO asset_bundles (media_variant_id, display_name, created_at) \
             VALUES (?, 'Movie', ?)",
        )
        .bind(variant_id)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at) VALUES (?)")
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let source_file_version_id = FileVersionId(
        sqlx::query(
            "INSERT INTO file_versions \
             (file_asset_id, content_hash, size_bytes, produced_by, created_at) \
             VALUES (?, 'blake3:source', 10, 'external_observed', ?)",
        )
        .bind(asset_id)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    let source_media_snapshot_id = MediaSnapshotId(
        sqlx::query(
            "INSERT INTO media_snapshots (file_version_id, probed_by, probed_at, payload) \
             VALUES (?, ?, ?, '{}')",
        )
        .bind(i64::try_from(source_file_version_id.0).unwrap())
        .bind(i64::try_from(worker_id.0).unwrap())
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid()
        .try_into()
        .unwrap(),
    );
    Fixture {
        pool: pool.clone(),
        repo: SqliteAudioExtractOperationRepo::new(pool),
        source_file_version_id,
        source_bundle_id,
        source_media_snapshot_id,
        lease_id,
        worker_id,
        _tmp: tmp,
    }
}

impl Fixture {
    fn operation(&self) -> NewAudioExtractOperation {
        NewAudioExtractOperation {
            operation_key: "extract:v1:key".to_owned(),
            operation_id: Some("op-1".to_owned()),
            source_file_version_id: self.source_file_version_id,
            source_bundle_id: self.source_bundle_id,
            source_media_snapshot_id: self.source_media_snapshot_id,
        }
    }
}

fn outputs() -> Vec<NewAudioExtractOutput> {
    vec![
        NewAudioExtractOutput {
            output_id: Some("out-1".to_owned()),
            source_snapshot_stream_id: "stream-1".to_owned(),
            source_provider_stream_index: 1,
            bundle_role: "commentary_audio".to_owned(),
            target_path: "/committed/audio-1.ogg".to_owned(),
        },
        NewAudioExtractOutput {
            output_id: Some("out-2".to_owned()),
            source_snapshot_stream_id: "stream-2".to_owned(),
            source_provider_stream_index: 2,
            bundle_role: "external_audio".to_owned(),
            target_path: "/committed/audio-2.ogg".to_owned(),
        },
    ]
}

#[tokio::test]
async fn create_planned_persists_ordered_outputs_and_replays_exact_input() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();

    let first = fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let replay = fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.operation.state, AudioExtractOperationState::Planned);
    assert_eq!(
        first
            .outputs
            .iter()
            .map(|output| output.output_id.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("out-1"), Some("out-2")]
    );
}

#[tokio::test]
async fn create_planned_rejects_semantic_drift_for_an_existing_key() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
    fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let mut drifted = outputs();
    drifted[1].target_path = "/committed/drifted.ogg".to_owned();

    let error = fixture
        .repo
        .create_planned(fixture.operation(), &drifted, now)
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not match persisted descriptor")
    );
}

#[tokio::test]
async fn claim_fences_competing_tokens_until_expiry() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
    fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: "extract:v1:key".to_owned(),
        expected_generation: 0,
        lease_id: fixture.lease_id,
        claim_token: "claim-one".to_owned(),
        expires_at: OffsetDateTime::from_unix_timestamp(10).unwrap(),
    };
    fixture.repo.acquire_claim(&claim, now).await.unwrap();

    let mut competing = claim.clone();
    competing.claim_token = "claim-two".to_owned();
    let error = fixture
        .repo
        .acquire_claim(&competing, OffsetDateTime::from_unix_timestamp(5).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("claim is held"));

    competing.expires_at = OffsetDateTime::from_unix_timestamp(20).unwrap();
    fixture
        .repo
        .acquire_claim(&competing, OffsetDateTime::from_unix_timestamp(11).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn dispatch_attempt_and_paths_are_durable_before_send() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
    fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: "extract:v1:key".to_owned(),
        expected_generation: 0,
        lease_id: fixture.lease_id,
        claim_token: "claim-one".to_owned(),
        expires_at: OffsetDateTime::from_unix_timestamp(10).unwrap(),
    };
    fixture.repo.acquire_claim(&claim, now).await.unwrap();

    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &claim,
            NewAudioExtractDispatchAttempt {
                worker_id: fixture.worker_id,
                worker_epoch: 0,
                idempotency_key: "audio-extract:key:0".to_owned(),
                attempt_directory: "/staging/attempt-0".to_owned(),
                paths: vec![
                    "/staging/attempt-0/out-1.ogg".to_owned(),
                    "/staging/attempt-0/out-2.ogg".to_owned(),
                ],
            },
            now,
        )
        .await
        .unwrap();

    let stored_paths: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM audio_extract_dispatch_attempt_paths \
         WHERE attempt_id = ? ORDER BY ordinal",
    )
    .bind(i64::try_from(attempt.id).unwrap())
    .fetch_all(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(stored_paths, attempt.paths);
    assert_eq!(attempt.status, AudioExtractDispatchAttemptStatus::Active);
}

#[tokio::test]
async fn terminal_dispatch_advance_fences_stale_generation_completion() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
    fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: "extract:v1:key".to_owned(),
        expected_generation: 0,
        lease_id: fixture.lease_id,
        claim_token: "claim-one".to_owned(),
        expires_at: OffsetDateTime::from_unix_timestamp(10).unwrap(),
    };
    fixture.repo.acquire_claim(&claim, now).await.unwrap();
    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &claim,
            NewAudioExtractDispatchAttempt {
                worker_id: fixture.worker_id,
                worker_epoch: 0,
                idempotency_key: "audio-extract:key:0".to_owned(),
                attempt_directory: "/staging/attempt-0".to_owned(),
                paths: vec!["/staging/attempt-0/out-1.ogg".to_owned()],
            },
            now,
        )
        .await
        .unwrap();

    fixture
        .repo
        .mark_dispatch_terminal(
            &claim,
            attempt.id,
            OffsetDateTime::from_unix_timestamp(1).unwrap(),
        )
        .await
        .unwrap();
    fixture
        .repo
        .advance_terminal_generation(
            &claim,
            attempt.id,
            OffsetDateTime::from_unix_timestamp(1).unwrap(),
        )
        .await
        .unwrap();

    let operation = fixture
        .repo
        .get_exact_by_key(&fixture.operation(), &outputs())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(operation.operation.dispatch_generation, 1);
    let stale_error = fixture
        .repo
        .mark_dispatch_terminal(
            &claim,
            attempt.id,
            OffsetDateTime::from_unix_timestamp(2).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        stale_error
            .to_string()
            .contains("lost its claim or is not active")
    );
}

#[tokio::test]
async fn quarantined_dispatch_requires_expiry_and_exact_operator_acknowledgement() {
    let fixture = fixture().await;
    let now = OffsetDateTime::from_unix_timestamp(0).unwrap();
    fixture
        .repo
        .create_planned(fixture.operation(), &outputs(), now)
        .await
        .unwrap();
    let claim = NewAudioExtractClaim {
        operation_key: "extract:v1:key".to_owned(),
        expected_generation: 0,
        lease_id: fixture.lease_id,
        claim_token: "claim-one".to_owned(),
        expires_at: OffsetDateTime::from_unix_timestamp(10).unwrap(),
    };
    fixture.repo.acquire_claim(&claim, now).await.unwrap();
    let attempt = fixture
        .repo
        .record_dispatch_attempt(
            &claim,
            NewAudioExtractDispatchAttempt {
                worker_id: fixture.worker_id,
                worker_epoch: 7,
                idempotency_key: "audio-extract:key:0".to_owned(),
                attempt_directory: "/staging/attempt-0".to_owned(),
                paths: vec!["/staging/attempt-0/out-1.ogg".to_owned()],
            },
            now,
        )
        .await
        .unwrap();
    fixture
        .repo
        .quarantine_dispatch(
            &claim,
            attempt.id,
            OffsetDateTime::from_unix_timestamp(1).unwrap(),
        )
        .await
        .unwrap();

    let live_error = fixture
        .repo
        .acknowledge_quiescence(
            &AudioExtractQuiescenceAcknowledgement {
                operation_key: "extract:v1:key".to_owned(),
                generation: 0,
                attempt_id: attempt.id,
                worker_id: fixture.worker_id,
                worker_epoch: 7,
                idempotency_key: "audio-extract:key:0".to_owned(),
                acknowledged_by: "operator".to_owned(),
            },
            OffsetDateTime::from_unix_timestamp(5).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(live_error.to_string().contains("does not exactly match"));

    let exact = AudioExtractQuiescenceAcknowledgement {
        operation_key: "extract:v1:key".to_owned(),
        generation: 0,
        attempt_id: attempt.id,
        worker_id: fixture.worker_id,
        worker_epoch: 7,
        idempotency_key: "audio-extract:key:0".to_owned(),
        acknowledged_by: "operator".to_owned(),
    };
    let mut mismatches = Vec::new();
    let mut wrong = exact.clone();
    wrong.operation_key = "extract:v1:wrong".to_owned();
    mismatches.push(wrong);
    let mut wrong = exact.clone();
    wrong.generation = 1;
    mismatches.push(wrong);
    let mut wrong = exact.clone();
    wrong.worker_id = WorkerId(fixture.worker_id.0 + 1);
    mismatches.push(wrong);
    let mut wrong = exact.clone();
    wrong.worker_epoch = 8;
    mismatches.push(wrong);
    let mut wrong = exact.clone();
    wrong.idempotency_key = "audio-extract:wrong:0".to_owned();
    mismatches.push(wrong);
    for mismatch in mismatches {
        let error = fixture
            .repo
            .acknowledge_quiescence(&mismatch, OffsetDateTime::from_unix_timestamp(11).unwrap())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not exactly match"));
    }

    fixture
        .repo
        .acknowledge_quiescence(&exact, OffsetDateTime::from_unix_timestamp(11).unwrap())
        .await
        .unwrap();
    let stored = fixture
        .repo
        .get_dispatch_attempt(1, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.status, AudioExtractDispatchAttemptStatus::Quiesced);
}
