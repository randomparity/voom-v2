use super::*;

use async_trait::async_trait;
use serde_json::json;
use time::OffsetDateTime;
use voom_core::{ErrorCode, FileVersionId, WorkerId};
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IdentityRepo, IngestOutcome};
use voom_worker_protocol::{
    ObservedFileFacts, ProbeFileRequest, ProbeFileResult, ProbeFileStatus, RemuxObservedFacts,
    RemuxResult, RemuxStatus,
};

#[tokio::test]
async fn result_snapshot_records_probed_payload_with_normalized_stream_ids() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source bytes").unwrap();
    let file_version_id = seed_source(&cp, &source, b"source bytes").await;
    let target = dir.path().join("target.mkv");
    let dispatcher = FakeResultProbeDispatcher::matching();

    let snapshot = record_result_snapshot_with_dispatcher(
        &cp,
        file_version_id,
        &target,
        &remux_result(),
        &dispatcher,
    )
    .await
    .unwrap();

    assert_eq!(snapshot.file_version_id, file_version_id);
    assert_eq!(snapshot.probed_by, Some(WorkerId(1)));
    assert!(snapshot.payload.get("snapshot_kind").is_none());
    assert_eq!(snapshot.payload["format"], "sprint10-v1");
    assert_eq!(snapshot.payload["probe"]["provider"], "ffprobe");
    assert_eq!(
        snapshot.payload["container"]["format_name"],
        "matroska,webm"
    );
    assert_eq!(snapshot.payload["streams"][0]["id"], "stream-0");
    assert_eq!(snapshot.payload["streams"][1]["id"], "explicit-audio");
    let request = dispatcher.take_request();
    assert_eq!(request.path, target.display().to_string());
    assert_eq!(request.expected.size_bytes, 10);
    assert_eq!(request.expected.content_hash, "blake3:output");
}

#[tokio::test]
async fn result_snapshot_rejects_probe_fact_drift_without_recording_snapshot() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source bytes").unwrap();
    let file_version_id = seed_source(&cp, &source, b"source bytes").await;
    let target = dir.path().join("target.mkv");
    let dispatcher = FakeResultProbeDispatcher::drifted();

    let err = record_result_snapshot_with_dispatcher(
        &cp,
        file_version_id,
        &target,
        &remux_result(),
        &dispatcher,
    )
    .await
    .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    let snapshots = cp
        .identity
        .list_media_snapshots_by_version(file_version_id)
        .await
        .unwrap();
    assert!(snapshots.is_empty());
}

#[derive(Debug)]
struct FakeResultProbeDispatcher {
    result: ProbeFileResult,
    request: std::sync::Mutex<Option<ProbeFileRequest>>,
}

impl FakeResultProbeDispatcher {
    fn matching() -> Self {
        Self {
            result: probe_result(10, "blake3:output"),
            request: std::sync::Mutex::new(None),
        }
    }

    fn drifted() -> Self {
        Self {
            result: probe_result(11, "blake3:drifted"),
            request: std::sync::Mutex::new(None),
        }
    }

    fn take_request(&self) -> ProbeFileRequest {
        self.request.lock().unwrap().take().unwrap()
    }
}

#[async_trait]
impl RemuxResultProbeDispatcher for FakeResultProbeDispatcher {
    async fn dispatch_result_probe(
        &self,
        cp: &ControlPlane,
        request: ProbeFileRequest,
    ) -> Result<ProbedRemuxResult, VoomError> {
        *self.request.lock().unwrap() = Some(request);
        let worker_id = ensure_result_probe_worker(cp).await?;
        Ok(ProbedRemuxResult {
            worker_id,
            result: self.result.clone(),
        })
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
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(u32::MAX),
        )),
    )
    .await
    .unwrap();
    (cp, db, tempfile::TempDir::new().unwrap())
}

async fn seed_source(
    cp: &crate::ControlPlane,
    path: &std::path::Path,
    bytes: &[u8],
) -> FileVersionId {
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
        file_version_id, ..
    } = outcome
    else {
        panic!("seed_source should create a new file asset");
    };
    file_version_id
}

fn remux_result() -> RemuxResult {
    let input = RemuxObservedFacts {
        size_bytes: 12,
        content_hash: "blake3:source".to_owned(),
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
            size_bytes: 10,
            content_hash: "blake3:output".to_owned(),
            modified_at: None,
            local_file_key: None,
        },
        output_container: "mkv".to_owned(),
        kept_snapshot_stream_ids: vec!["stream-0".to_owned()],
        default_snapshot_stream_ids: vec!["stream-0".to_owned()],
    }
}

fn probe_result(size_bytes: u64, content_hash: &str) -> ProbeFileResult {
    let facts = ObservedFileFacts {
        size_bytes,
        content_hash: content_hash.to_owned(),
        modified_at: None,
        local_file_key: None,
    };
    ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".to_owned(),
        provider_version: "7.0".to_owned(),
        pre_probe: facts.clone(),
        post_probe: facts,
        snapshot: json!({
            "format": "sprint10-v1",
            "probe": {
                "provider": "ffprobe",
                "provider_version": "7.0"
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
                    "id": "explicit-audio",
                    "index": 1,
                    "kind": "audio",
                    "codec_name": "aac"
                }
            ]
        }),
    }
}

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
