use super::*;

use async_trait::async_trait;
use time::OffsetDateTime;
use voom_core::{ErrorCode, JobId, LeaseId, TicketId, rng_test_support::FrozenRng};
use voom_events::EventKind;
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_worker_protocol::{
    TranscodeVideoObservedFacts, TranscodeVideoProfile, TranscodeVideoRequest,
    TranscodeVideoResult, TranscodeVideoStatus, VerifyArtifactObservedFacts, VerifyArtifactRequest,
    VerifyArtifactResult, VerifyArtifactStatus,
};

use crate::transcode::resolve::ResolvedProfile;

fn default_resolved() -> ResolvedProfile {
    ResolvedProfile {
        profile: TranscodeVideoProfile::default_hevc(),
        output_container: "mkv".to_owned(),
    }
}

#[tokio::test]
async fn execute_records_verified_committed_transcode_result_and_events() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let report = execute_transcode_video_with_dispatchers(
        &cp,
        ExecuteTranscodeVideoInput {
            job_id: JobId(1),
            ticket_id: TicketId(2),
            lease_id: LeaseId(3),
            source_file_version_id: seeded.0,
            source_location_id: Some(seeded.1),
            staging_root: dir.path().join("stage"),
            target_dir: dir.path().join("out"),
            resolved: default_resolved(),
        },
        &FakeTranscodeDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap();

    assert_eq!(report.source_file_version_id, seeded.0);
    assert!(
        report
            .staging_path
            .ends_with("ticket-2/lease-3/Movie.default-hevc.hevc.mkv")
    );
    assert!(report.target_path.ends_with("Movie.default-hevc.hevc.mkv"));
    assert!(report.target_path.exists());
    assert_eq!(
        count_events(&cp, EventKind::ArtifactTranscodeStarted).await,
        1
    );
    assert_eq!(
        count_events(&cp, EventKind::ArtifactTranscodeSucceeded).await,
        1
    );
    assert_eq!(report.resolved_profile, "default-hevc");
    assert_eq!(report.encoder, "libx265");
    assert_eq!(report.target_codec, "hevc");
    assert_eq!(report.output_container, "mkv");
    assert!(!report.copied_video);
    assert_eq!(report.output_width, 1280);
    assert_eq!(report.output_height, 720);
    assert_eq!(report.output_pixel_format, "yuv420p");

    let succeeded = succeeded_payload(&cp).await;
    assert_eq!(succeeded.profile_name, "default-hevc");
    assert_eq!(succeeded.encoder, "libx265");
    assert_eq!(succeeded.target_codec, "hevc");
    assert_eq!(succeeded.output_container, "mkv");
    assert!(!succeeded.copied_video);
    assert_eq!(succeeded.output_width, 1280);
    assert_eq!(succeeded.output_height, 720);
    assert_eq!(succeeded.output_pixel_format, "yuv420p");
    assert!(
        succeeded
            .staging_path
            .ends_with("ticket-2/lease-3/Movie.default-hevc.hevc.mkv"),
        "succeeded event recorded an empty/wrong staging_path: {:?}",
        succeeded.staging_path
    );
    assert_eq!(
        succeeded.staging_path,
        report.staging_path.display().to_string()
    );
}

#[tokio::test]
async fn execute_rejects_non_hevc_worker_result_before_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = execute_transcode_video_with_dispatchers(
        &cp,
        ExecuteTranscodeVideoInput {
            job_id: JobId(1),
            ticket_id: TicketId(2),
            lease_id: LeaseId(3),
            source_file_version_id: seeded.0,
            source_location_id: Some(seeded.1),
            staging_root: dir.path().join("stage"),
            target_dir: dir.path().join("out"),
            resolved: default_resolved(),
        },
        &WrongCodecTranscodeDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
}

#[tokio::test]
async fn execute_rejects_worker_result_for_wrong_input_facts_before_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = execute_transcode_video_with_dispatchers(
        &cp,
        ExecuteTranscodeVideoInput {
            job_id: JobId(1),
            ticket_id: TicketId(2),
            lease_id: LeaseId(3),
            source_file_version_id: seeded.0,
            source_location_id: Some(seeded.1),
            staging_root: dir.path().join("stage"),
            target_dir: dir.path().join("out"),
            resolved: default_resolved(),
        },
        &WrongInputFactsTranscodeDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(
        !dir.path().join("out/Movie.default-hevc.hevc.mkv").exists(),
        "mismatched input facts must stop before commit"
    );
}

#[tokio::test]
async fn execute_rejects_copied_video_disagreement_before_commit() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("Movie.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let seeded = seed_source(&cp, &source, b"source bytes").await;

    let err = execute_transcode_video_with_dispatchers(
        &cp,
        ExecuteTranscodeVideoInput {
            job_id: JobId(1),
            ticket_id: TicketId(2),
            lease_id: LeaseId(3),
            source_file_version_id: seeded.0,
            source_location_id: Some(seeded.1),
            staging_root: dir.path().join("stage"),
            target_dir: dir.path().join("out"),
            resolved: default_resolved(),
        },
        // default_resolved() is not copy_compatible, so request copy_video=false;
        // a worker result claiming copied_video=true must be rejected.
        &CopiedVideoDisagreementDispatcher,
        &FakeVerifyDispatcher,
        &FakeResultProbeDispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(
        !dir.path().join("out/Movie.default-hevc.hevc.mkv").exists(),
        "copied_video disagreement must stop before commit"
    );
}

#[derive(Debug)]
struct FakeTranscodeDispatcher;

#[async_trait]
impl TranscodeVideoDispatcher for FakeTranscodeDispatcher {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"hevc bytes").unwrap();
        Ok(transcode_result(request, "hevc"))
    }
}

#[derive(Debug)]
struct WrongCodecTranscodeDispatcher;

#[async_trait]
impl TranscodeVideoDispatcher for WrongCodecTranscodeDispatcher {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"hevc bytes").unwrap();
        Ok(transcode_result(request, "h264"))
    }
}

#[derive(Debug)]
struct CopiedVideoDisagreementDispatcher;

#[async_trait]
impl TranscodeVideoDispatcher for CopiedVideoDisagreementDispatcher {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"hevc bytes").unwrap();
        let mut result = transcode_result(request, "hevc");
        result.copied_video = true;
        Ok(result)
    }
}

#[derive(Debug)]
struct WrongInputFactsTranscodeDispatcher;

#[async_trait]
impl TranscodeVideoDispatcher for WrongInputFactsTranscodeDispatcher {
    async fn dispatch_transcode_video(
        &self,
        request: TranscodeVideoRequest,
    ) -> Result<TranscodeVideoResult, voom_core::VoomError> {
        std::fs::write(&request.output.path, b"hevc bytes").unwrap();
        let mut result = transcode_result(request, "hevc");
        result.input_pre.size_bytes += 1;
        result.input_post = result.input_pre.clone();
        Ok(result)
    }
}

#[derive(Debug)]
struct FakeResultProbeDispatcher;

#[async_trait]
impl crate::transcode::commit::TranscodeResultProbeDispatcher for FakeResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &crate::ControlPlane,
        request: voom_worker_protocol::ProbeFileRequest,
    ) -> Result<crate::transcode::commit::ProbedTranscodeResult, voom_core::VoomError> {
        let mut tx = cp.pool.begin().await.unwrap();
        let worker_id = crate::scan::bootstrap::ensure_builtin_ffprobe_worker_in_tx(cp, &mut tx)
            .await?
            .id;
        tx.commit().await.unwrap();
        let facts = voom_worker_protocol::ObservedFileFacts {
            size_bytes: request.expected.size_bytes,
            content_hash: request.expected.content_hash.clone(),
            modified_at: None,
            local_file_key: None,
        };
        Ok(crate::transcode::commit::ProbedTranscodeResult {
            worker_id,
            result: voom_worker_protocol::ProbeFileResult {
                status: voom_worker_protocol::ProbeFileStatus::Probed,
                provider: "ffprobe".to_owned(),
                provider_version: "test".to_owned(),
                pre_probe: facts.clone(),
                post_probe: facts,
                snapshot: serde_json::json!({
                    "format": "sprint10-v1",
                    "probe": {"provider": "ffprobe", "provider_version": "test"},
                    "container": {"format_name": "matroska,webm"},
                    "streams": [
                        {
                            "index": 0,
                            "kind": "video",
                            "codec_name": "hevc",
                            "pixel_format": "yuv420p",
                            "width": 1280,
                            "height": 720
                        }
                    ]
                }),
            },
        })
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

fn transcode_result(request: TranscodeVideoRequest, codec: &str) -> TranscodeVideoResult {
    let output_hash = blake3_checksum(b"hevc bytes");
    let input = TranscodeVideoObservedFacts {
        size_bytes: request.input.expected.size_bytes,
        content_hash: request.input.expected.content_hash,
        modified_at: None,
        local_file_key: None,
    };
    TranscodeVideoResult {
        status: TranscodeVideoStatus::Transcoded,
        provider: "ffmpeg".to_owned(),
        provider_version: "test".to_owned(),
        input_pre: input.clone(),
        input_post: input,
        output: TranscodeVideoObservedFacts {
            size_bytes: 10,
            content_hash: output_hash,
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        output_video_codec: codec.to_owned(),
        output_width: 1280,
        output_height: 720,
        output_pixel_format: "yuv420p".to_owned(),
        copied_video: false,
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

async fn succeeded_payload(
    cp: &crate::ControlPlane,
) -> voom_events::payload::ArtifactTranscodeSucceededPayload {
    let page = cp
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::ArtifactTranscodeSucceeded),
                ..EventFilter::default()
            },
            Page {
                limit: 100,
                cursor: None,
            },
        )
        .await
        .unwrap();
    match &page.items[0].envelope.payload {
        voom_events::Event::ArtifactTranscodeSucceeded(p) => p.clone(),
        other => panic!("expected ArtifactTranscodeSucceeded, got {other:?}"),
    }
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

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
