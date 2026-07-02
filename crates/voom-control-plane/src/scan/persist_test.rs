use super::{
    ObservedCandidateFacts, ScanPersistError, persist_scanned_media_snapshot, verify_probe_facts,
};

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
use voom_store::repo::workers::{NewWorker, WorkerKind};
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
        .get_media_snapshot(persisted.media_snapshot_id.unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.file_asset_id.0, 1);
    assert_eq!(snapshot.file_version_id, persisted.file_version_id);
    assert_eq!(snapshot.probed_by, Some(worker.id));
    assert_eq!(snapshot.payload["format"], result.snapshot["format"]);
    assert_eq!(snapshot.payload["streams"][0]["language"], "eng");
    assert_eq!(snapshot.payload["streams"][0]["id"], "stream-0");
    assert_eq!(result.snapshot["streams"][0].get("id"), None);
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
async fn persist_scan_assigns_stable_stream_ids() {
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

    let snapshot = cp
        .identity()
        .get_media_snapshot(persisted.media_snapshot_id.unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(snapshot.payload["streams"][0]["id"], "stream-0");
    assert_eq!(result.snapshot["streams"][0].get("id"), None);
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

#[tokio::test]
async fn two_hardlinks_resolve_to_one_asset_with_two_locations() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    // Two paths, same physical file: identical content and identical (dev, ino).
    let first = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 2);
    let second = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 2);

    let a = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/a.mkv"),
        &[],
        &first,
        &matching_probe_result(&first),
    )
    .await
    .unwrap();
    assert!(!a.hardlink);

    let b = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/b.mkv"),
        &[],
        &second,
        &matching_probe_result(&second),
    )
    .await
    .unwrap();

    // The hardlink resolves to the first asset/version, adding a location only.
    assert!(b.hardlink);
    assert_eq!(b.file_asset_id, a.file_asset_id);
    assert_eq!(b.file_version_id, a.file_version_id);
    assert_ne!(b.file_location_id, a.file_location_id);
    assert!(b.media_snapshot_id.is_none());

    assert_eq!(table_count(&cp, "file_assets").await, 1);
    assert_eq!(table_count(&cp, "file_versions").await, 1);
    assert_eq!(table_count(&cp, "file_locations").await, 2);
    assert_eq!(table_count(&cp, "media_snapshots").await, 1);
    assert_eq!(table_count(&cp, "scan_file_facts").await, 2);
}

#[tokio::test]
async fn byte_identical_copy_on_a_different_inode_stays_a_distinct_asset() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    // Same content, DIFFERENT inode: a copy, not a hardlink.
    let first = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 1);
    let copy = candidate_facts_with_inode(123, "blake3:abc", 42, 8, 1);

    persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/a.mkv"),
        &[],
        &first,
        &matching_probe_result(&first),
    )
    .await
    .unwrap();
    let b = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/copy.mkv"),
        &[],
        &copy,
        &matching_probe_result(&copy),
    )
    .await
    .unwrap();

    assert!(!b.hardlink);
    assert_eq!(table_count(&cp, "file_assets").await, 2);
    assert_eq!(table_count(&cp, "file_versions").await, 2);
    assert_eq!(table_count(&cp, "scan_file_facts").await, 2);
}

#[tokio::test]
async fn recycled_inode_with_different_content_does_not_collapse_identity() {
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    // Same (dev, ino) but DIFFERENT content — a recycled inode or in-place
    // edit. The content guard must reject the hardlink attach.
    let first = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 1);
    let recycled = candidate_facts_with_inode(456, "blake3:xyz", 42, 7, 1);

    persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/a.mkv"),
        &[],
        &first,
        &matching_probe_result(&first),
    )
    .await
    .unwrap();
    let b = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/recycled.mkv"),
        &[],
        &recycled,
        &matching_probe_result(&recycled),
    )
    .await
    .unwrap();

    assert!(
        !b.hardlink,
        "different content must not alias onto the version"
    );
    assert_ne!(b.file_asset_id, first_asset(&cp).await);
    assert_eq!(table_count(&cp, "file_assets").await, 2);
    assert_eq!(table_count(&cp, "file_versions").await, 2);
}

async fn first_asset(cp: &crate::ControlPlane) -> voom_core::FileAssetId {
    let id: i64 = sqlx::query_scalar("SELECT MIN(id) FROM file_assets")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    voom_core::FileAssetId(u64::try_from(id).unwrap())
}

// `verify_probe_facts` AND-chains four equalities (pre/post size, pre/post
// hash) into one content-drift error. A regression to a subset (e.g. dropping
// a `&&` term) would still pass the integration drift test, which only mutates
// post_probe.content_hash. These pin each of the other three terms in
// isolation so the guard cannot silently narrow.

#[test]
fn verify_probe_facts_accepts_fully_matching_result() {
    let candidate = candidate_facts(123, "blake3:abc");
    let result = matching_probe_result(&candidate);
    assert!(verify_probe_facts(&candidate, &result).is_ok());
}

#[test]
fn verify_probe_facts_rejects_pre_probe_size_mismatch() {
    let candidate = candidate_facts(123, "blake3:abc");
    let mut result = matching_probe_result(&candidate);
    result.pre_probe.size_bytes = candidate.size_bytes + 1;
    let err = verify_probe_facts(&candidate, &result).unwrap_err();
    assert_eq!(err.status(), ScanReportFileStatus::FailedContentDrift);
    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
}

#[test]
fn verify_probe_facts_rejects_post_probe_size_mismatch() {
    let candidate = candidate_facts(123, "blake3:abc");
    let mut result = matching_probe_result(&candidate);
    result.post_probe.size_bytes = candidate.size_bytes + 1;
    let err = verify_probe_facts(&candidate, &result).unwrap_err();
    assert_eq!(err.status(), ScanReportFileStatus::FailedContentDrift);
    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
}

#[test]
fn verify_probe_facts_rejects_pre_probe_hash_mismatch() {
    let candidate = candidate_facts(123, "blake3:abc");
    let mut result = matching_probe_result(&candidate);
    result.pre_probe.content_hash = "blake3:changed".to_owned();
    let err = verify_probe_facts(&candidate, &result).unwrap_err();
    assert_eq!(err.status(), ScanReportFileStatus::FailedContentDrift);
    assert_eq!(err.failure_class(), FailureClass::ArtifactChecksumMismatch);
}

#[tokio::test]
async fn hardlink_with_its_own_sidecars_attaches_them_to_the_bundle() {
    use crate::scan::discovery::{SidecarCandidate, SidecarKind};

    let dir = tempfile::tempdir().unwrap();
    let sidecar_path = {
        let path = dir.path().join("b.srt");
        std::fs::write(&path, b"subtitle").unwrap();
        std::fs::canonicalize(path).unwrap()
    };
    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    // First path ingests the physical file with no sidecars.
    let first = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 2);
    persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/a.mkv"),
        &[],
        &first,
        &matching_probe_result(&first),
    )
    .await
    .unwrap();

    // The hardlink at a different path carries its own sidecar. It must not be
    // dropped: the hardlink resolves to the existing asset AND its sidecar is
    // attached to that asset's bundle.
    let second = candidate_facts_with_inode(123, "blake3:abc", 42, 7, 2);
    let sidecars = vec![SidecarCandidate {
        path: sidecar_path,
        kind: SidecarKind::Subtitle,
    }];
    let hardlink = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/b.mkv"),
        &sidecars,
        &second,
        &matching_probe_result(&second),
    )
    .await
    .unwrap();

    assert!(hardlink.hardlink);
    assert_eq!(hardlink.sidecars.len(), 1);
    assert_eq!(hardlink.sidecars[0].bundle_member_role, "external_subtitle");
    assert!(hardlink.bundle_id.is_some());
    // One physical primary (no second asset), plus the sidecar asset.
    assert_eq!(table_count(&cp, "file_assets").await, 2);
    assert_eq!(table_count(&cp, "file_versions").await, 2);
}

#[tokio::test]
async fn persists_sidecars_under_per_kind_roles() {
    use crate::scan::discovery::{SidecarCandidate, SidecarKind};

    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, bytes: &[u8]| -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::canonicalize(path).unwrap()
    };
    // observe_sidecars reads and hashes each file, so the paths must exist.
    let sidecars = vec![
        SidecarCandidate {
            path: write("Movie.srt", b"subtitle"),
            kind: SidecarKind::Subtitle,
        },
        SidecarCandidate {
            path: write("Movie.nfo", b"nfo"),
            kind: SidecarKind::Nfo,
        },
        SidecarCandidate {
            path: write("Movie-poster.jpg", b"poster"),
            kind: SidecarKind::Poster,
        },
        SidecarCandidate {
            path: write("Movie-trailer.mkv", b"trailer"),
            kind: SidecarKind::Trailer,
        },
    ];

    let (cp, _tmp) = cp_with_manual_clock(T0).await;
    let worker = register_local_worker(&cp, "scan-worker").await;
    let candidate = candidate_facts(10, "blake3:primary");
    let result = matching_probe_result(&candidate);

    let persisted = persist_scanned_media_snapshot(
        &cp,
        worker.id,
        Path::new("/library/Movie.mkv"),
        &sidecars,
        &candidate,
        &result,
    )
    .await
    .unwrap();

    assert_eq!(
        persisted.bundle_member_role.as_deref(),
        Some("primary_video")
    );
    let roles: std::collections::BTreeMap<String, String> = persisted
        .sidecars
        .iter()
        .map(|sidecar| {
            (
                sidecar
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                sidecar.bundle_member_role.clone(),
            )
        })
        .collect();
    assert_eq!(
        roles.get("Movie.srt").map(String::as_str),
        Some("external_subtitle")
    );
    assert_eq!(roles.get("Movie.nfo").map(String::as_str), Some("nfo"));
    assert_eq!(
        roles.get("Movie-poster.jpg").map(String::as_str),
        Some("poster")
    );
    assert_eq!(
        roles.get("Movie-trailer.mkv").map(String::as_str),
        Some("trailer")
    );

    // The durable membership rows carry the same per-kind roles (primary + 4).
    let db_roles: Vec<String> =
        sqlx::query_scalar("SELECT role FROM asset_bundle_members ORDER BY role ASC")
            .fetch_all(cp.pool_for_test())
            .await
            .unwrap();
    assert_eq!(
        db_roles,
        vec![
            "external_subtitle",
            "nfo",
            "poster",
            "primary_video",
            "trailer"
        ]
    );
}

fn candidate_facts(size_bytes: u64, content_hash: &str) -> ObservedCandidateFacts {
    ObservedCandidateFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        dev: None,
        ino: None,
        nlink: None,
    }
}

fn candidate_facts_with_inode(
    size_bytes: u64,
    content_hash: &str,
    dev: u64,
    ino: u64,
    nlink: u64,
) -> ObservedCandidateFacts {
    ObservedCandidateFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        dev: Some(dev),
        ino: Some(ino),
        nlink: Some(nlink),
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
