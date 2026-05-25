use super::{ObservedCandidateFacts, ScanPersistError, persist_scanned_media_snapshot};

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::scan::ScanReportFileStatus;
use serde_json::json;
use time::OffsetDateTime;
use voom_core::clock_test_support::ManualClock;
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ErrorCode, FailureClass, VoomError, WorkerId};
use voom_events::EventKind;
use voom_store::repo::identity::IdentityRepo;
use voom_store::repo::workers::{NewWorker, WorkerKind, WorkerRepo};
use voom_worker_protocol::ProbeFileStatus;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn persists_discovered_file_and_media_snapshot_with_selected_worker() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    let candidate = candidate_facts(123, "blake3:abc");
    let result = matching_probe_result(&candidate);

    let persisted = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/movie.mkv"),
        &[],
        &candidate,
        &result,
    )
    .await
    .unwrap();

    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "file_versions").await, 1);
    assert_eq!(table_count(&cp, "file_locations").await, 1);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);

    let (kind, value): (String, String) =
        sqlx::query_as("SELECT kind, value FROM file_locations WHERE id = ?")
            .bind(i64::try_from(persisted.file_location_id.0).unwrap())
            .fetch_one(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(kind, "local_path");
    assert_eq!(value, "/library/movie.mkv");

    let snapshot = cp
        .identity()
        .get_media_snapshot(persisted.media_snapshot_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.file_asset_id.0, 1);
    assert_eq!(snapshot.file_version_id, persisted.file_version_id);
    assert_eq!(snapshot.probed_by, Some(worker.id));
    assert_eq!(snapshot.payload, result.snapshot);
    assert_eq!(snapshot.payload["streams"][0]["language"], "eng");
    assert_eq!(
        snapshot.payload["streams"][0]["disposition"]["default"],
        true
    );
    assert_eq!(
        state_transition_event_kinds(&cp).await,
        vec![
            EventKind::FileAssetCreated.as_str(),
            EventKind::FileVersionCreated.as_str(),
            EventKind::FileLocationRecorded.as_str(),
            EventKind::MediaSnapshotRecorded.as_str(),
        ]
    );
}

#[tokio::test]
async fn content_drift_skips_persistence_and_returns_failed_content_drift() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    let candidate = candidate_facts(123, "blake3:abc");
    let mut result = matching_probe_result(&candidate);
    result.post_probe.content_hash = "blake3:changed".to_owned();

    let err = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/movie.mkv"),
        &[],
        &candidate,
        &result,
    )
    .await
    .unwrap_err();

    let ScanPersistError::File(file_err) = err else {
        panic!("expected per-file scan error");
    };
    assert_eq!(file_err.status(), ScanReportFileStatus::FailedContentDrift);
    assert_eq!(file_err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert_eq!(
        file_err.failure_class(),
        FailureClass::ArtifactChecksumMismatch
    );
    assert_eq!(
        file_err.message(),
        "file changed between hashing and probing"
    );

    assert_eq!(table_count(&cp, "file_assets").await, 0);
    assert_eq!(table_count(&cp, "file_versions").await, 0);
    assert_eq!(table_count(&cp, "file_locations").await, 0);
    assert_eq!(table_count(&cp, "media_snapshots").await, 0);
    assert!(state_transition_event_kinds(&cp).await.is_empty());
}

#[tokio::test]
async fn missing_or_retired_worker_id_is_rejected_without_replacement_worker() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let candidate = candidate_facts(123, "blake3:abc");
    let result = matching_probe_result(&candidate);

    let missing_err = persist_scanned_media_snapshot(
        &cp,
        WorkerId(999),
        Path::new("/library/movie.mkv"),
        &[],
        &candidate,
        &result,
    )
    .await
    .unwrap_err();
    assert_store_conflict(missing_err);
    assert_eq!(table_count(&cp, "workers").await, 0);
    assert_eq!(table_count(&cp, "file_assets").await, 0);
    assert_eq!(table_count(&cp, "media_snapshots").await, 0);

    let worker = register_local_worker(&cp, "scan-worker").await;
    cp.workers()
        .retire(worker.id, worker.epoch, T0 + time::Duration::seconds(1))
        .await
        .unwrap();

    let retired_err = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/movie.mkv"),
        &[],
        &candidate,
        &result,
    )
    .await
    .unwrap_err();
    assert_store_conflict(retired_err);
    assert_eq!(table_count(&cp, "workers").await, 1);
    assert_eq!(table_count(&cp, "file_assets").await, 0);
    assert_eq!(table_count(&cp, "media_snapshots").await, 0);
    assert!(state_transition_event_kinds(&cp).await.is_empty());
}

fn candidate_facts(size_bytes: u64, content_hash: &str) -> ObservedCandidateFacts {
    ObservedCandidateFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
    }
}

fn matching_probe_result(
    candidate: &ObservedCandidateFacts,
) -> voom_worker_protocol::ProbeFileResult {
    let observed = voom_worker_protocol::ObservedFileFacts {
        size_bytes: candidate.size_bytes,
        content_hash: candidate.content_hash.clone(),
        modified_at: None,
        local_file_key: None,
    };
    voom_worker_protocol::ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: "test".to_owned(),
        pre_probe: observed.clone(),
        post_probe: observed,
        snapshot: json!({
            "format": "sprint10-v1",
            "probe": {
                "provider": "ffprobe",
                "provider_version": "test",
                "command": "ffprobe",
                "probed_at": "2026-05-24T00:00:00Z"
            },
            "streams": [
                {
                    "index": 0,
                    "kind": "audio",
                    "codec_name": "aac",
                    "language": "eng",
                    "disposition": {
                        "default": true
                    }
                }
            ]
        }),
    }
}

async fn register_local_worker(
    cp: &crate::ControlPlane,
    name: &str,
) -> voom_store::repo::workers::Worker {
    cp.workers()
        .register(NewWorker {
            name: name.to_owned(),
            kind: WorkerKind::Local,
            registered_at: T0,
            node_id: None,
        })
        .await
        .unwrap()
}

async fn table_count(cp: &crate::ControlPlane, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(&sql)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn state_transition_event_kinds(cp: &crate::ControlPlane) -> Vec<String> {
    sqlx::query_scalar("SELECT kind FROM events WHERE kind != ? ORDER BY event_id ASC")
        .bind(EventKind::SchemaInitialized.as_str())
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
}

fn assert_store_conflict(err: ScanPersistError) {
    let ScanPersistError::Store(err) = err else {
        panic!("expected store error");
    };
    assert!(matches!(err, VoomError::Conflict(_)), "got: {err:?}");
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock,
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, tmp)
}
