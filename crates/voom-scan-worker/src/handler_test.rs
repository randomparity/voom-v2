use std::path::Path;

use tempfile::TempDir;
use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::http::OperationBody;

use super::*;
use voom_worker_protocol::{ScanCandidate, ScanCandidateFile, decode_candidate_progress};

fn operation_request(operation: OperationKind, payload: serde_json::Value) -> OperationRequest {
    OperationRequest {
        operation,
        lease_id: LeaseId(42),
        payload,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

fn scan_request_payload(provider_locator: &str) -> serde_json::Value {
    serde_json::to_value(ScanLibraryRequest {
        provider_locator: provider_locator.to_owned(),
        extension_allowlist: Vec::new(),
    })
    .unwrap_or(serde_json::Value::Null)
}

fn buffered_body(dispatch: &OperationDispatch) -> Option<&[u8]> {
    match &dispatch.body {
        OperationBody::Buffered(bytes) => Some(bytes),
        OperationBody::Streaming(_) => None,
    }
}

fn parse_frames(body: &[u8]) -> Option<Vec<ProgressFrame>> {
    let mut frames = Vec::new();
    for line in body.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(frame) => frames.push(frame),
            Err(_) => return None,
        }
    }
    Some(frames)
}

#[tokio::test]
async fn non_scan_library_operations_are_protocol_errors() {
    let request = operation_request(OperationKind::HashFile, serde_json::Value::Null);
    let result = handle_operation(request).await;
    assert!(
        matches!(&result, Err(ProtocolError::UnknownOperation { name }) if name == "HashFile"),
        "expected UnknownOperation('HashFile'), got {result:?}"
    );
}

#[tokio::test]
async fn malformed_payload_is_a_terminal_malformed_worker_result_frame_on_http_200() {
    // A malformed payload must be a terminal worker result on HTTP 200 so
    // retries replay it instead of evicting the idempotency row.
    let request = operation_request(
        OperationKind::ScanLibrary,
        serde_json::json!({ "provider_locator": 12 }),
    );
    let dispatched = handle_operation(request).await;
    assert!(dispatched.is_ok(), "expected dispatch, got {dispatched:?}");
    let Ok(dispatch) = dispatched else {
        return;
    };
    let body = buffered_body(&dispatch);
    assert!(
        body.is_some(),
        "malformed-payload path must buffer its body"
    );
    let frames = parse_frames(body.unwrap_or_default());
    assert!(frames.is_some(), "body did not parse as NDJSON frames");
    assert!(
        matches!(
            frames.as_deref(),
            Some([ProgressFrame::Error {
                lease_id: LeaseId(42),
                class: FailureClass::MalformedWorkerResult,
                code: ErrorCode::MalformedWorkerResult,
                ..
            }])
        ),
        "unexpected frames: {frames:?}"
    );
}

#[tokio::test]
async fn unavailable_root_yields_a_terminal_artifact_unavailable_error_frame() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    let missing = dir.path().join("does-not-exist");
    let request = operation_request(
        OperationKind::ScanLibrary,
        scan_request_payload(missing.to_string_lossy().as_ref()),
    );
    let dispatched = handle_operation(request).await;
    assert!(dispatched.is_ok(), "expected dispatch, got {dispatched:?}");
    let Ok(dispatch) = dispatched else {
        return;
    };
    let body = buffered_body(&dispatch);
    assert!(body.is_some(), "root-failure path must buffer its body");
    let frames = parse_frames(body.unwrap_or_default());
    assert!(frames.is_some(), "body did not parse as NDJSON frames");
    assert!(
        matches!(
            frames.as_deref(),
            Some([ProgressFrame::Error {
                lease_id: LeaseId(42),
                class: FailureClass::ArtifactUnavailable,
                ..
            }])
        ),
        "unexpected frames: {frames:?}"
    );
}

#[test]
fn frames_carry_decodable_candidates_then_the_result_counts() {
    let Ok(dir) = TempDir::new() else {
        return;
    };
    seed_media(dir.path());

    let walked = scan_root(dir.path(), &[]);
    assert!(walked.is_ok(), "walk failed: {:?}", walked.as_ref().err());
    let Some(outcome) = walked.ok() else {
        return;
    };

    let built = build_frames(LeaseId(42), &outcome);
    assert!(built.is_ok(), "frame build failed");
    let Some(frames) = built.ok() else {
        return;
    };
    assert!(frames.len() >= 2, "expected progress + result frames");

    // Every progress payload must survive the strict structural decode the
    // agent will apply.
    for frame in &frames[..frames.len() - 1] {
        match frame {
            ProgressFrame::Progress {
                seq,
                payload: Some(payload),
                ..
            } => {
                let decoded = decode_candidate_progress(payload);
                assert!(decoded.is_ok(), "progress payload rejected: {decoded:?}");
                assert_eq!(*seq, 0);
            }
            other => assert_progress_frame(other),
        }
    }

    match &frames[frames.len() - 1] {
        ProgressFrame::Result { seq, payload, .. } => {
            let expected = serde_json::json!({
                "discovered_count": 2,
                "skipped_count": 0,
            });
            assert_eq!(payload, &expected);
            assert_eq!(*seq, u64::try_from(frames.len() - 1).unwrap_or(u64::MAX));
        }
        other => assert_progress_frame(other),
    }
}

fn assert_progress_frame(frame: &ProgressFrame) {
    assert!(
        matches!(frame, ProgressFrame::Progress { .. }),
        "unexpected terminal/other frame: {frame:?}"
    );
}

fn seed_media(dir: &Path) {
    for relative in ["movie/movie.mkv", "movie/movie.srt", "shorts/clip.mp4"] {
        let target = dir.join(relative);
        if let Some(parent) = target.parent() {
            assert!(std::fs::create_dir_all(parent).is_ok());
        }
        assert!(std::fs::write(target, b"x").is_ok());
    }
}

#[test]
fn empty_enumeration_terminates_with_only_a_zero_result_frame() {
    let Ok(dir) = TempDir::new() else {
        return;
    };

    let walked = scan_root(dir.path(), &[]);
    assert!(walked.is_ok(), "walk failed: {:?}", walked.as_ref().err());
    let Some(outcome) = walked.ok() else {
        return;
    };

    let built = build_frames(LeaseId(42), &outcome);
    let Some(frames) = built.ok() else {
        return;
    };

    assert_eq!(frames.len(), 1);
    assert!(
        matches!(
            frames.first(),
            Some(ProgressFrame::Result { payload, .. })
                if *payload == serde_json::json!({"discovered_count": 0, "skipped_count": 0})
        ),
        "unexpected frames: {frames:?}"
    );
}

fn synthetic_candidate(index: usize) -> ScanCandidate {
    ScanCandidate {
        primary: ScanCandidateFile {
            provider_relative_locator: format!("dir-{index}/movie-{index}.mkv"),
            provider_object_identity: format!("dev={index};ino={index};pad={}", "x".repeat(160)),
            size_bytes: u64::try_from(index).unwrap_or_default(),
            modified_at: "2026-01-01T00:00:00Z".to_owned(),
            kind: None,
        },
        sidecars: vec![ScanCandidateFile {
            provider_relative_locator: format!("dir-{index}/movie-{index}.srt"),
            provider_object_identity: format!("dev={index};ino={index};pad={}", "y".repeat(160)),
            size_bytes: u64::try_from(index).unwrap_or_default(),
            modified_at: "2026-01-01T00:00:00Z".to_owned(),
            kind: Some("external_subtitle".to_owned()),
        }],
    }
}

#[test]
fn batching_respects_both_candidate_and_byte_bounds() {
    // 300 two-file candidates (~75 KiB serialized) force splits on both the
    // 256-candidate bound and the 32 KiB byte budget.
    let candidates: Vec<ScanCandidate> = (0..300).map(synthetic_candidate).collect();

    let batched = batch_candidate_payloads(&candidates);
    assert!(
        batched.is_ok(),
        "batching failed: {:?}",
        batched.as_ref().err()
    );
    let Some(payloads) = batched.ok() else {
        return;
    };

    assert!(
        payloads.len() >= 3,
        "expected multiple batches, got {}",
        payloads.len()
    );
    let mut total = 0_usize;
    for payload in &payloads {
        let decoded = decode_candidate_progress(payload);
        assert!(
            decoded.is_ok(),
            "batch payload rejected by decode: {decoded:?}"
        );
        let Some(batch) = decoded.ok() else {
            return;
        };
        total += batch.len();
        assert!(batch.len() <= MAX_PROGRESS_CANDIDATES);
        let serialized = serde_json::to_vec(payload);
        assert!(serialized.is_ok(), "re-serialization failed");
        if let Ok(bytes) = serialized {
            assert!(bytes.len() <= MAX_PROGRESS_PAYLOAD_BYTES);
        }
    }
    assert_eq!(
        total,
        candidates.len(),
        "batching lost or duplicated candidates"
    );
}

#[test]
fn single_candidate_over_the_byte_budget_is_a_malformed_worker_result() {
    let mut oversized = synthetic_candidate(0);
    oversized.primary.provider_object_identity = format!(
        "dev=1;ino=1;pad={}",
        "x".repeat(MAX_PROGRESS_PAYLOAD_BYTES + 1)
    );

    let batched = batch_candidate_payloads(std::slice::from_ref(&oversized));
    assert!(
        matches!(&batched, Err(ScanWorkerError::MalformedWorkerResult { .. })),
        "expected MalformedWorkerResult, got {batched:?}"
    );
}
