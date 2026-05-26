use super::*;

use time::OffsetDateTime;
use voom_core::FileVersionId;
use voom_store::repo::identity::{DiscoveredFile, FileLocationKind, IngestOutcome};
use voom_worker_protocol::{RemuxObservedFacts, RemuxResult, RemuxStatus};

#[tokio::test]
async fn result_snapshot_records_remux_result_ids() {
    let (cp, _db, dir) = fixture().await;
    let source = dir.path().join("source.mkv");
    std::fs::write(&source, b"source bytes").unwrap();
    let file_version_id = seed_source(&cp, &source, b"source bytes").await;

    let snapshot = record_result_snapshot(&cp, file_version_id, &remux_result())
        .await
        .unwrap();

    assert_eq!(snapshot.file_version_id, file_version_id);
    assert_eq!(snapshot.payload["source"], "remux_result");
    assert_eq!(snapshot.payload["container"], "mkv");
    assert_eq!(
        snapshot.payload["kept_snapshot_stream_ids"],
        serde_json::json!(["stream-0"])
    );
    assert_eq!(
        snapshot.payload["default_snapshot_stream_ids"],
        serde_json::json!(["stream-0"])
    );
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

fn blake3_checksum(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
