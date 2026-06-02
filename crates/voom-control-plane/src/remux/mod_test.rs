use super::*;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, JobId, LeaseId, TicketId, rng_test_support::FrozenRng};
use voom_events::EventKind;
use voom_store::repo::artifacts::ArtifactCommitState;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_worker_protocol::{
    ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus, RemuxObservedFacts,
    RemuxRequest, RemuxResult, RemuxStatus, VerifyArtifactObservedFacts, VerifyArtifactRequest,
    VerifyArtifactResult, VerifyArtifactStatus,
};

#[tokio::test]
async fn execute_records_verified_committed_remux_result() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    let report = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
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
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxStarted).await, 1);
    assert_eq!(
        count_events(&cp, EventKind::ArtifactRemuxSucceeded).await,
        1
    );
    let started = single_started_remux_payload(&cp).await;
    assert_eq!(started.provider, None);
    assert_eq!(started.provider_version, None);
    assert_eq!(count_events(&cp, EventKind::MediaSnapshotRecorded).await, 2);
}

#[tokio::test]
async fn execute_uses_pinned_source_media_snapshot() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let pinned = record_source_snapshot_with_audio_languages(&cp, seeded.0, &["eng"]).await;
    let _latest = record_source_snapshot_with_audio_languages(&cp, seeded.0, &["spa"]).await;

    let report = execute_remux_with_dispatchers(
        &cp,
        remux_input_with_operation_payload(
            &dir,
            seeded,
            json!({
                "type": "remux",
                "container": "mkv",
                "source_media_snapshot_id": pinned.0,
                "track_actions": [
                    {
                        "type": "keep_tracks",
                        "target": "audio",
                        "filter": {"type": "language_in", "values": ["eng"]}
                    }
                ],
                "track_order": ["video", "audio"],
                "defaults": []
            }),
        ),
        &ExpectKeepStreamsRemuxDispatcher {
            expected: vec!["stream-0", "stream-1"],
        },
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.result_media_snapshot_id.0, pinned.0 + 2);
}

#[tokio::test]
async fn execute_rejects_missing_source_media_snapshot_id() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let _snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input_without_source_media_snapshot_id(&dir, seeded),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("source_media_snapshot_id"));
}

#[tokio::test]
async fn execute_rejects_source_media_snapshot_for_other_file_version() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    let other = dir.path().join("Other.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    std::fs::write(&other, b"other bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let other_seeded = seed_source(&cp, &other, b"other bytes").await;
    let mismatched_snapshot_id = record_source_snapshot(&cp, other_seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, mismatched_snapshot_id),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("does not belong"));
    let failed = single_failed_remux_payload(&cp).await;
    assert_eq!(failed.error_code, "CONFIG_INVALID");
    assert_eq!(failed.source_file_location_id, Some(seeded.1.0));
    assert_eq!(failed.staging_path, None);
    assert!(failed.selected_streams.is_empty());
}

#[tokio::test]
async fn execute_rejects_worker_result_for_wrong_input_facts_before_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &WrongInputFactsRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(
        !dir.path().join("out/Movie.remux.mkv").exists(),
        "mismatched input facts must stop before commit"
    );
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxStarted).await, 1);
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxFailed).await, 1);
    assert_eq!(
        failed_remux_error_code(&cp).await.as_deref(),
        Some("ARTIFACT_CHECKSUM_MISMATCH")
    );
}

#[tokio::test]
async fn verification_failure_event_includes_staged_artifact_ids() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &FakeRemuxDispatcher,
        &FailedVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::VerificationFailure);
    let failed = single_failed_remux_payload(&cp).await;
    assert_eq!(failed.error_code, "VERIFICATION_FAILURE");
    assert_eq!(failed.artifact_handle_id, Some(1));
    assert_eq!(failed.artifact_location_id, Some(1));
}

#[tokio::test]
async fn worker_progress_frames_record_remux_progress_events() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &ProgressingRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxProgress).await, 1);
}

#[tokio::test]
async fn success_event_append_failure_prevents_successful_report_without_success_event() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;
    sqlx::query(
        "CREATE TRIGGER fail_remux_success_event \
         BEFORE INSERT ON events WHEN NEW.kind = 'artifact.remux_succeeded' \
         BEGIN SELECT RAISE(ABORT, 'event log unavailable'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert!(dir.path().join("out/Movie.remux.mkv").exists());
    assert_eq!(
        count_events(&cp, EventKind::ArtifactRemuxSucceeded).await,
        0
    );
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxFailed).await, 0);
}

#[tokio::test]
async fn execute_revalidates_drifted_source_before_worker_dispatch() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;
    std::fs::write(&source, b"changed source bytes").unwrap();
    let dispatcher = CountingRemuxDispatcher::default();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &dispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
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
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;
    std::fs::remove_file(&source).unwrap();
    let dispatcher = CountingRemuxDispatcher::default();

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &dispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
    assert_eq!(dispatcher.call_count(), 0);
}

#[tokio::test]
async fn execute_sends_canonical_staging_root_to_worker() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;
    let root = dir.path().canonicalize().unwrap();
    let staging_root = root.join("stage-parent/stage/../stage");
    let canonical_staging_root = root.join("stage-parent/stage");
    let dispatcher = CaptureStagingRootRemuxDispatcher::default();
    let mut input = remux_input(&dir, seeded, source_media_snapshot_id);
    input.staging_root = staging_root;

    execute_remux_with_dispatchers(
        &cp,
        input,
        &dispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(
        dispatcher.captured_staging_root(),
        Some(
            canonical_staging_root
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
}

#[tokio::test]
async fn result_snapshot_failure_preserves_committed_result_ids_in_error() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;
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
        remux_input(&dir, seeded, source_media_snapshot_id),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    let message = err.to_string();
    assert!(message.contains("commit_record_id"));
    assert!(message.contains("result_file_version_id"));
    assert!(message.contains("result_file_location_id"));
    assert!(dir.path().join("out/Movie.remux.mkv").exists());
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxFailed).await, 0);
}

#[tokio::test]
async fn execute_does_not_commit_when_staged_result_probe_fails() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;
    let source_media_snapshot_id = record_source_snapshot(&cp, seeded.0).await;

    let err = execute_remux_with_dispatchers(
        &cp,
        remux_input(&dir, seeded, source_media_snapshot_id),
        &FakeRemuxDispatcher,
        &FakeVerifyDispatcher,
        &ErroringResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert!(
        !dir.path().join("out/Movie.remux.mkv").exists(),
        "a staged-result probe failure must not leave a committed target"
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactRemuxSucceeded).await,
        0
    );
    // The probe runs after ArtifactRemuxStarted and routes through
    // record_failure, so this pre-commit failure records exactly one failed
    // event (unlike a post-commit DB-write failure, which records none).
    assert_eq!(count_events(&cp, EventKind::ArtifactRemuxFailed).await, 1);
    // Only the source snapshot exists; no result snapshot is recorded.
    assert_eq!(count_events(&cp, EventKind::MediaSnapshotRecorded).await, 1);
}

#[test]
fn commit_failure_message_preserves_recovery_metadata() {
    let target_path = std::path::PathBuf::from("/media/Movie.remux.mkv");
    let temp_path = std::path::PathBuf::from("/media/.voom-tmp.Movie.remux.mkv");
    let staging_path = std::path::PathBuf::from("/stage/ticket-2/lease-3/Movie.remux.mkv");
    let report = crate::artifact::commit::CommitArtifactReport {
        commit_record_id: voom_core::ids::ArtifactCommitRecordId(10),
        artifact_handle_id: voom_core::ArtifactHandleId(20),
        verification_id: voom_core::ids::ArtifactVerificationId(30),
        target_path: target_path.clone(),
        temp_path: Some(temp_path.clone()),
        state: ArtifactCommitState::RecoveryRequired,
        result_file_version_id: Some(voom_core::FileVersionId(40)),
        result_file_location_id: Some(voom_core::FileLocationId(50)),
        recovery_required: Some(crate::artifact::commit::CommitRecoveryReport {
            recovery_reason: "mutation_failed".to_owned(),
            target_path,
            target_exists: true,
            temp_path: Some(temp_path),
            temp_exists: false,
            staging_path,
            staging_exists: true,
            result_file_version_id: Some(voom_core::FileVersionId(40)),
            result_file_location_id: Some(voom_core::FileLocationId(50)),
        }),
    };

    let message = format_commit_failure_message("commit finalize requires recovery", Some(&report));

    assert!(message.contains("commit finalize requires recovery"));
    assert!(message.contains("commit_record_id=10"));
    assert!(message.contains("artifact_handle_id=20"));
    assert!(message.contains("verification_id=30"));
    assert!(message.contains("state=recovery_required"));
    assert!(message.contains("recovery_reason=mutation_failed"));
    assert!(message.contains("target_path=/media/Movie.remux.mkv"));
    assert!(message.contains("target_exists=true"));
    assert!(message.contains("temp_path=/media/.voom-tmp.Movie.remux.mkv"));
    assert!(message.contains("temp_exists=false"));
    assert!(message.contains("staging_path=/stage/ticket-2/lease-3/Movie.remux.mkv"));
    assert!(message.contains("staging_exists=true"));
    assert!(message.contains("result_file_version_id=40"));
    assert!(message.contains("result_file_location_id=50"));
}

#[derive(Debug)]
struct FakeRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for FakeRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
    }
}

#[derive(Debug)]
struct WrongInputFactsRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for WrongInputFactsRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        let mut result = remux_result(request);
        result.input_pre.size_bytes += 1;
        result.input_post = result.input_pre.clone();
        Ok(result)
    }
}

#[derive(Debug)]
struct ProgressingRemuxDispatcher;

#[async_trait]
impl RemuxDispatcher for ProgressingRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        progress
            .record_remux_progress(
                Some(voom_worker_protocol::PercentBps::try_from(2500).unwrap()),
                None,
            )
            .await?;
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
    }
}

#[derive(Debug)]
struct ExpectKeepStreamsRemuxDispatcher {
    expected: Vec<&'static str>,
}

#[async_trait]
impl RemuxDispatcher for ExpectKeepStreamsRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        assert_eq!(
            request
                .selection
                .keep_streams
                .iter()
                .map(|stream| stream.snapshot_stream_id.as_str())
                .collect::<Vec<_>>(),
            self.expected
        );
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
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

#[derive(Debug, Default)]
struct CaptureStagingRootRemuxDispatcher {
    staging_root: std::sync::Mutex<Option<String>>,
}

impl CaptureStagingRootRemuxDispatcher {
    fn captured_staging_root(&self) -> Option<String> {
        self.staging_root.lock().unwrap().clone()
    }
}

#[async_trait]
impl RemuxDispatcher for CaptureStagingRootRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
    ) -> Result<RemuxResult, voom_core::VoomError> {
        *self.staging_root.lock().unwrap() = Some(request.output.staging_root.clone());
        std::fs::write(&request.output.path, b"remux bytes").unwrap();
        Ok(remux_result(request))
    }
}

#[async_trait]
impl RemuxDispatcher for CountingRemuxDispatcher {
    async fn dispatch_remux_with_progress(
        &self,
        request: RemuxRequest,
        _progress: &mut dyn dispatch::RemuxProgressSink,
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

#[derive(Debug)]
struct FakeResultProbeDispatcher;

#[async_trait]
impl commit::RemuxResultProbeDispatcher for FakeResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<commit::ProbedRemuxResult, voom_core::VoomError> {
        let mut tx = cp.pool_for_test().begin().await.unwrap();
        let worker = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let facts = ObservedFileFacts {
            size_bytes: request.expected.size_bytes,
            content_hash: request.expected.content_hash,
            modified_at: None,
            local_file_key: None,
        };
        Ok(commit::ProbedRemuxResult {
            worker_id: worker.id,
            result: ProbeFileResult {
                status: ProbeFileStatus::Probed,
                provider: "ffprobe".to_owned(),
                provider_version: "test".to_owned(),
                pre_probe: facts.clone(),
                post_probe: facts,
                snapshot: json!({
                    "format": "sprint10-v1",
                    "probe": {
                        "provider": "ffprobe",
                        "provider_version": "test"
                    },
                    "container": {
                        "format_name": "matroska,webm"
                    },
                    "streams": [
                        {
                            "index": 0,
                            "kind": "video",
                            "codec_name": "h264"
                        },
                        {
                            "index": 1,
                            "kind": "audio",
                            "codec_name": "aac",
                            "language": "eng",
                            "disposition": {
                                "default": true
                            }
                        }
                    ]
                }),
            },
        })
    }
}

#[derive(Debug)]
struct ErroringResultProbeDispatcher;

#[async_trait]
impl commit::RemuxResultProbeDispatcher for ErroringResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        _cp: &crate::ControlPlane,
        _request: ProbeFileRequest,
    ) -> Result<commit::ProbedRemuxResult, voom_core::VoomError> {
        Err(voom_core::VoomError::ExternalSystemUnavailable(
            "remux result probe failed: simulated worker error".to_owned(),
        ))
    }
}

#[derive(Debug)]
struct FailedVerifyDispatcher;

#[async_trait]
impl crate::artifact::verify::VerifyArtifactDispatcher for FailedVerifyDispatcher {
    async fn dispatch_verify_artifact(
        &self,
        _worker_id: voom_core::WorkerId,
        _request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, crate::artifact::worker::VerifyWorkerError> {
        Ok(VerifyArtifactResult {
            status: VerifyArtifactStatus::Verified,
            provider: "fake-verify".to_owned(),
            provider_version: "test".to_owned(),
            observed: VerifyArtifactObservedFacts {
                size_bytes: 0,
                content_hash: "blake3:bad".to_owned(),
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
) -> voom_core::MediaSnapshotId {
    record_source_snapshot_with_audio_languages(cp, file_version_id, &["eng"]).await
}

async fn record_source_snapshot_with_audio_languages(
    cp: &crate::ControlPlane,
    file_version_id: voom_core::FileVersionId,
    languages: &[&str],
) -> voom_core::MediaSnapshotId {
    let audio_streams = languages
        .iter()
        .enumerate()
        .map(|(offset, language)| {
            let index = offset + 1;
            json!({
                "id": format!("stream-{index}"),
                "index": index,
                "kind": "audio",
                "codec_name": "aac",
                "language": language,
                "channels": 2,
                "disposition": {
                    "default": false
                }
            })
        })
        .collect::<Vec<_>>();
    let mut streams = vec![json!({
        "id": "stream-0",
        "index": 0,
        "kind": "video",
        "codec_name": "h264",
        "disposition": {
            "default": true
        }
    })];
    streams.extend(audio_streams);

    cp.record_media_snapshot(
        file_version_id,
        None,
        json!({ "streams": streams }),
        OffsetDateTime::UNIX_EPOCH,
    )
    .await
    .unwrap()
    .id
}

fn remux_input(
    dir: &tempfile::TempDir,
    seeded: (voom_core::FileVersionId, voom_core::FileLocationId),
    source_media_snapshot_id: voom_core::MediaSnapshotId,
) -> ExecuteRemuxInput {
    remux_input_with_operation_payload(
        dir,
        seeded,
        json!({
            "type": "remux",
            "container": "mkv",
            "source_media_snapshot_id": source_media_snapshot_id.0,
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{"target": "audio", "strategy": "first"}]
        }),
    )
}

fn remux_input_without_source_media_snapshot_id(
    dir: &tempfile::TempDir,
    seeded: (voom_core::FileVersionId, voom_core::FileLocationId),
) -> ExecuteRemuxInput {
    remux_input_with_operation_payload(
        dir,
        seeded,
        json!({
            "type": "remux",
            "container": "mkv",
            "track_actions": [],
            "track_order": ["video", "audio"],
            "defaults": [{"target": "audio", "strategy": "first"}]
        }),
    )
}

fn remux_input_with_operation_payload(
    dir: &tempfile::TempDir,
    seeded: (voom_core::FileVersionId, voom_core::FileLocationId),
    operation_payload: serde_json::Value,
) -> ExecuteRemuxInput {
    let root = dir.path().canonicalize().unwrap();
    ExecuteRemuxInput {
        job_id: JobId(1),
        ticket_id: TicketId(2),
        lease_id: LeaseId(3),
        source_file_version_id: seeded.0,
        source_location_id: Some(seeded.1),
        operation_payload,
        staging_root: root.join("stage"),
        target_dir: root.join("out"),
    }
}

async fn seed_source(
    cp: &crate::ControlPlane,
    path: &std::path::Path,
    bytes: &[u8],
) -> (voom_core::FileVersionId, voom_core::FileLocationId) {
    let location_value = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    let outcome = cp
        .record_discovered_file(
            DiscoveredFile {
                location_kind: FileLocationKind::LocalPath,
                location_value,
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

async fn failed_remux_error_code(cp: &crate::ControlPlane) -> Option<String> {
    let event = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::ArtifactRemuxFailed),
                ..EventFilter::default()
            },
            Page {
                limit: 1,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .next()?;
    match event.envelope.payload {
        voom_events::Event::ArtifactRemuxFailed(payload) => Some(payload.error_code),
        other => panic!("expected remux failed payload, got {other:?}"),
    }
}

async fn single_started_remux_payload(
    cp: &crate::ControlPlane,
) -> voom_events::payload::ArtifactRemuxStartedPayload {
    let event = single_event(cp, EventKind::ArtifactRemuxStarted).await;
    match event.envelope.payload {
        voom_events::Event::ArtifactRemuxStarted(payload) => payload,
        other => panic!("expected remux started payload, got {other:?}"),
    }
}

async fn single_failed_remux_payload(
    cp: &crate::ControlPlane,
) -> voom_events::payload::ArtifactRemuxFailedPayload {
    let event = single_event(cp, EventKind::ArtifactRemuxFailed).await;
    match event.envelope.payload {
        voom_events::Event::ArtifactRemuxFailed(payload) => payload,
        other => panic!("expected remux failed payload, got {other:?}"),
    }
}

async fn single_event(
    cp: &crate::ControlPlane,
    kind: EventKind,
) -> voom_store::repo::events::EventRow {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
                ..EventFilter::default()
            },
            Page {
                limit: 1,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap()
}
