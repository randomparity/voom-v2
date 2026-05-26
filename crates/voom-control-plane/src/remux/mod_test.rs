use super::*;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, JobId, LeaseId, TicketId, rng_test_support::FrozenRng};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_worker_protocol::{
    RemuxObservedFacts, RemuxRequest, RemuxResult, RemuxStatus, VerifyArtifactObservedFacts,
    VerifyArtifactRequest, VerifyArtifactResult, VerifyArtifactStatus,
};

#[tokio::test]
async fn execute_records_verified_committed_remux_result() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    cp.record_media_snapshot(
        seeded.0,
        None,
        json!({
            "streams": [
                {
                    "id": "stream-0",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "disposition": {
                        "default": true
                    }
                },
                {
                    "id": "stream-1",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "aac",
                    "language": "eng",
                    "channels": 2,
                    "disposition": {
                        "default": false
                    }
                }
            ]
        }),
        OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();

    let report = execute_remux_with_dispatchers(
        &cp,
        ExecuteRemuxInput {
            job_id: JobId(1),
            ticket_id: TicketId(2),
            lease_id: LeaseId(3),
            source_file_version_id: seeded.0,
            source_location_id: Some(seeded.1),
            operation_payload: json!({
                "type": "remux",
                "container": "mkv",
                "track_actions": [],
                "track_order": ["video", "audio"],
                "defaults": [{"target": "audio", "strategy": "first"}]
            }),
            staging_root: dir.path().join("stage"),
            target_dir: dir.path().join("out"),
        },
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.source_file_version_id, seeded.0);
    assert!(
        report
            .staging_path
            .ends_with("ticket-2/lease-3/Movie.remux.mkv")
    );
    assert!(report.target_path.ends_with("Movie.remux.mkv"));
    assert!(report.target_path.exists());
    assert_eq!(count_events(&cp, EventKind::ArtifactStaged).await, 1);
    assert_eq!(count_events(&cp, EventKind::MediaSnapshotRecorded).await, 2);
}

#[tokio::test]
async fn execute_rejects_worker_result_for_wrong_input_facts_before_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    record_source_snapshot(&cp, seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded),
        &WrongInputFactsRemuxDispatcher,
        &FakeVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(
        !dir.path().join("out/Movie.remux.mkv").exists(),
        "mismatched input facts must stop before commit"
    );
}

#[tokio::test]
async fn execute_revalidates_drifted_source_before_worker_dispatch() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    record_source_snapshot(&cp, seeded.0).await;
    std::fs::write(&source, b"changed source bytes").unwrap();
    let dispatcher = CountingRemuxDispatcher::default();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded),
        &dispatcher,
        &FakeVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert_eq!(dispatcher.call_count(), 0);
}

#[tokio::test]
async fn execute_revalidates_missing_source_before_worker_dispatch() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    record_source_snapshot(&cp, seeded.0).await;
    std::fs::remove_file(&source).unwrap();
    let dispatcher = CountingRemuxDispatcher::default();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded),
        &dispatcher,
        &FakeVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
    assert_eq!(dispatcher.call_count(), 0);
}

#[tokio::test]
async fn result_snapshot_failure_preserves_committed_result_ids_in_error() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    record_source_snapshot(&cp, seeded.0).await;
    sqlx::query(
        "CREATE TRIGGER fail_remux_result_snapshot \
         BEFORE INSERT ON media_snapshots \
         BEGIN SELECT RAISE(ABORT, 'probe unavailable'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    let message = err.to_string();
    assert!(message.contains("commit_record_id"));
    assert!(message.contains("result_file_version_id"));
    assert!(message.contains("result_file_location_id"));
}

#[derive(Debug)]
struct FakeRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for FakeRemuxDispatcher {
    async fn dispatch_remux(
        &self,
        request: RemuxRequest,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
    }
}

#[derive(Debug)]
struct WrongInputFactsRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for WrongInputFactsRemuxDispatcher {
    async fn dispatch_remux(
        &self,
        request: RemuxRequest,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        let mut result = remux_result(request);
        result.input_pre.size_bytes += 1;
        result.input_post = result.input_pre.clone();
        Ok(result)
    }
}

#[derive(Debug, Default)]
struct CountingRemuxDispatcher {
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingRemuxDispatcher {
    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RemuxDispatcher for CountingRemuxDispatcher {
    async fn dispatch_remux(
        &self,
        request: RemuxRequest,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
    }
}

#[derive(Debug)]
struct FakeVerifyDispatcher;

#[async_trait]
impl crate::artifact::verify::VerifyArtifactDispatcher for FakeVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "fake-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: request.expected.size_bytes,
                content_hash: request.expected.content_hash,
                modified_at: None,
                local_file_key: None,
            },
        })
    }
}

fn remux_result(request: RemuxRequest) -> RemuxResult {
    let output_hash = blake3_checksum(b"remux bytes");
    let input = RemuxObservedFacts {
        size_bytes: request.input.expected.size_bytes,
        content_hash: request.input.expected.content_hash,
        modified_at: None,
        local_file_key: None,
    };
    RemuxResult {
        status: RemuxStatus::Remuxed,
        provider: "mkvtoolnix".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: RemuxObservedFacts {
            size_bytes: 11,
            content_hash: output_hash,
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        kept_snapshot_stream_ids: request
            .selection
            .keep_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
        default_snapshot_stream_ids: request
            .selection
            .default_streams
            .iter()
            .map(|stream| stream.snapshot_stream_id.clone())
            .collect(),
    }
}

async fn fixture() -> (
    crate::ControlPlane,
    tempfile::NamedTempFile,
    tempfile::TempDir,
) {
    let db = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(voom_core::SystemClock),
        std::sync::Arc::new(std::sync::Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, db, tempfile::TempDir::new().unwrap())
}

async fn record_source_snapshot(
    cp: &crate::ControlPlane,
    file_version_id: voom_core::FileVersionId,
) {
    cp.record_media_snapshot(
        file_version_id,
        None,
        json!({
            "streams": [
                {
                    "id": "stream-0",
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "disposition": {
                        "default": true
                    }
                },
                {
                    "id": "stream-1",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "aac",
                    "language": "eng",
                    "channels": 2,
                    "disposition": {
                        "default": false
                    }
                }
            ]
        }),
        OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap();
}

fn remux_input(
    dir: &tempfile::TempDir,
    seeded: (voom_core::FileVersionId, voom_core::FileLocationId),
) -> ExecuteRemuxInput {
    ExecuteRemuxInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: seeded.0,
        source_location_id: Some(seeded.1),
        operation_payload: json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{"target": "audio", "strategy": "first"}]
        }),
        staging_root: dir.path().join("stage"),
        target_dir: dir.path().join("out"),
    }
}

async fn seed_source(
    cp: &crate::ControlPlane,
    path: &std::path::Path,
    bytes: &[u8],
) -> (voom_core::FileVersionId, voom_core::FileLocationId) {
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value: path.display().to_string(),
                content_hash: blake3_checksum(bytes),
                size_bytes: u64::try_from(bytes.len()).unwrap(),
                observed_at: OffsetDateTime::UNIX_EPOCH,
                proof: None,
            },
            None,
        )
        .await
        .unwrap();
    let IngestOutcome::NewFileAsset {
        file_version_id,
        file_location_id,
        ..
    } = outcome
    else {
        panic!("seed_source should create a new file asset");
    };
    (file_version_id, file_location_id)
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

async fn count_events(cp: &crate::ControlPlane, kind: EventKind) -> usize {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .len()
}
