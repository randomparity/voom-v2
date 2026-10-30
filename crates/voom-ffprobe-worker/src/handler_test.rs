#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests use direct assertions and bounded fixture setup"
)]
use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;
use voom_worker_protocol::{
    ClientHandle, HttpClient, HttpServer, NdjsonOutcome, ServerHandle, WorkerCredentials,
};

// The pending future is injected at the file-work boundary: this verifies real HTTP
// streaming and replay, while real-file integrity is covered by the worker tests.
#[tokio::test]
async fn pending_work_acknowledges_and_progresses_then_replays_terminal() {
    for fail in [false, true] {
        let release = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = held_operation_handler(release.clone(), calls.clone(), fail);
        let credentials = WorkerCredentials {
            worker_id: voom_core::WorkerId(7),
            worker_epoch: 3,
            secret: secrecy::SecretString::from("test-secret"),
        };
        let server = HttpServer::new(credentials.clone(), handler)
            .serve(([127, 0, 0, 1], 0).into())
            .await
            .unwrap();
        let client =
            HttpClient::with_timeouts(server.bound, Duration::from_secs(5), Duration::from_secs(5));
        let req = OperationRequest {
            operation: OperationKind::ProbeFile,
            lease_id: voom_core::LeaseId(42),
            payload: serde_json::json!({}),
            heartbeat_deadline_ms: 2_000,
            progress_idle_deadline_ms: 2_000,
        };
        let mut dispatch = client
            .dispatch(&credentials, "held-work", req.clone())
            .await
            .unwrap();
        let mut observed = Vec::new();
        for seq in 0..3 {
            let outcome =
                tokio::time::timeout(Duration::from_secs(2), dispatch.frames.next_frame())
                    .await
                    .expect("progress must arrive while work remains pending")
                    .unwrap();
            let NdjsonOutcome::Frame(frame) = outcome else {
                panic!("premature terminal")
            };
            assert_eq!(frame.seq(), seq);
            observed.push(frame);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        loop {
            let outcome =
                tokio::time::timeout(Duration::from_secs(2), dispatch.frames.next_frame())
                    .await
                    .unwrap()
                    .unwrap();
            match outcome {
                NdjsonOutcome::Frame(frame) => observed.push(frame),
                NdjsonOutcome::Terminated(frame) => {
                    assert_eq!(matches!(&frame, ProgressFrame::Error { .. }), fail);
                    assert_eq!(frame.seq(), u64::try_from(observed.len()).unwrap());
                    observed.push(frame);
                    break;
                }
                NdjsonOutcome::StreamEnd => panic!("missing terminal"),
            }
        }
        let mut replay = client
            .dispatch(&credentials, "held-work", req)
            .await
            .unwrap();
        for expected in observed {
            let outcome = replay.frames.next_frame().await.unwrap();
            let frame = match outcome {
                NdjsonOutcome::Frame(frame) | NdjsonOutcome::Terminated(frame) => frame,
                NdjsonOutcome::StreamEnd => panic!("truncated replay"),
            };
            assert_eq!(frame, expected);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "completed replay must not repeat work"
        );
        server.shutdown.send(()).unwrap();
        server.joined.await.unwrap();
    }
}

fn sample_result() -> ProbeFileResult {
    let facts = voom_worker_protocol::ObservedFileFacts {
        size_bytes: 7,
        content_hash: format!("blake3:{}", blake3::hash(b"fixture").to_hex()),
        modified_at: None,
        local_file_key: None,
    };
    ProbeFileResult {
        status: ProbeFileStatus::Probed,
        provider: "ffprobe".into(),
        provider_version: "test".into(),
        pre_probe: facts.clone(),
        post_probe: facts,
        snapshot: serde_json::json!({}),
        hash_source: None,
    }
}

fn held_operation_handler(
    release: Arc<Notify>,
    calls: Arc<AtomicUsize>,
    fail: bool,
) -> OperationHandler {
    Arc::new(move |req| {
        let release = release.clone();
        let calls = calls.clone();
        Box::pin(async move {
            stream_operation(
                req.lease_id,
                OffsetDateTime::now_utc(),
                req.progress_idle_deadline_ms,
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    release.notified().await;
                    if fail {
                        Err(malformed_worker_result(
                            "controlled_error",
                            "fixture failure".into(),
                        ))
                    } else {
                        Ok(sample_result())
                    }
                },
            )
        })
    })
}

/// A fake ffprobe that prints the fixture JSON for any probed path.
fn fixture_ffprobe(dir: &std::path::Path) -> FfprobeConfig {
    let json = dir.join("ffprobe.json");
    std::fs::write(&json, include_str!("../fixtures/ffprobe/basic-mp4.json")).unwrap();
    let script = dir.join("ffprobe");
    let body = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version handler-test\\n'; exit 0; fi\n\
         cat '{}'\n",
        json.display()
    );
    voom_test_support::executable::write_executable(&script, body).unwrap();
    FfprobeConfig::from_env_pairs([(crate::FFPROBE_BIN_ENV, script.as_os_str())]).unwrap()
}

/// The request the scan pump sends with reuse on, built from the file's
/// current stat and its real BLAKE3 digest.
#[cfg(unix)]
fn guarded_request(path: &std::path::Path) -> ProbeFileRequest {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).unwrap();
    ProbeFileRequest {
        path: path.display().to_string(),
        expected: voom_worker_protocol::ExpectedFileFacts {
            size_bytes: metadata.len(),
            content_hash: format!(
                "blake3:{}",
                blake3::hash(&std::fs::read(path).unwrap()).to_hex()
            ),
            modified_at: crate::observe::modified_at(&metadata),
            local_file_key: None,
        },
        guard: Some(voom_worker_protocol::ProbeGuard {
            changed_at_ns: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
            file_key: voom_core::FileKeyFacts {
                dev: metadata.dev(),
                ino: metadata.ino(),
                nlink: metadata.nlink(),
            },
        }),
    }
}

#[cfg(target_os = "linux")]
fn reuse_listed(dir: &std::path::Path) -> bool {
    crate::guard::on_reuse_listed_filesystem(std::fs::File::open(dir).unwrap())
}

#[cfg(not(target_os = "linux"))]
fn reuse_listed(_dir: &std::path::Path) -> bool {
    false
}

// G5: on the pinned test root a guarded probe follows the reuse list. Outside
// it (ext4 on Linux CI, every macOS filesystem) the hash is recomputed: no
// source, and a stale expected hash from before an mtime-restored edit fails
// as today. On a listed root (btrfs developer hosts) the hash is reused.
#[cfg(unix)]
#[tokio::test]
async fn guarded_probe_follows_the_reuse_list_on_the_test_root() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media.mkv");
    std::fs::write(&media, b"bytes the hash leg read").unwrap();
    let config = fixture_ffprobe(dir.path());
    let request = guarded_request(&media);

    let result = Box::pin(probe_file(&request, &config)).await.unwrap();

    if reuse_listed(dir.path()) {
        assert_eq!(result.hash_source, Some(voom_core::HashSource::Reused));
        return;
    }
    assert_eq!(result.hash_source, None);
    assert_eq!(result.pre_probe.content_hash, request.expected.content_hash);

    let original_mtime = std::fs::metadata(&media).unwrap().modified().unwrap();
    std::fs::write(&media, b"bytes the hash leg reaX").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&media)
        .unwrap()
        .set_modified(original_mtime)
        .unwrap();
    let err = Box::pin(probe_file(&request, &config)).await.unwrap_err();
    assert_eq!(
        err.failure_class(),
        voom_core::FailureClass::ArtifactChecksumMismatch
    );
    assert_eq!(err.payload()["stage"], "pre_probe");
}

// G8: an unguarded probe (output, media-child, setting off) never reports a
// hash source, so its encoded result keeps the protocol-5 shape.
#[cfg(unix)]
#[tokio::test]
async fn unguarded_probe_sets_no_hash_source() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media.mkv");
    std::fs::write(&media, b"output bytes").unwrap();
    let config = fixture_ffprobe(dir.path());
    let request = ProbeFileRequest {
        guard: None,
        ..guarded_request(&media)
    };

    let result = Box::pin(probe_file(&request, &config)).await.unwrap();

    assert_eq!(result.hash_source, None);
    assert!(
        serde_json::to_value(&result)
            .unwrap()
            .get("hash_source")
            .is_none()
    );
}
