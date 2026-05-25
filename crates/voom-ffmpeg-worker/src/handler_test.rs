use std::path::{Path, PathBuf};

use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, TranscodeVideoExpectedFacts, TranscodeVideoInput, TranscodeVideoOutput,
    TranscodeVideoProfile, TranscodeVideoRequest,
};

use crate::DEFAULT_PROCESS_TIMEOUT;

use super::*;

#[tokio::test]
async fn missing_input_is_artifact_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let request = request(dir.path(), &dir.path().join("missing.mkv")).await;

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::ArtifactUnavailable);
}

#[tokio::test]
async fn output_path_escape_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.output.path = dir
        .path()
        .join("../escape.mkv")
        .to_string_lossy()
        .into_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn unsupported_output_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.output.container = "mp4".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("mkv"));
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn unsupported_profile_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.profile.encoder = "libx264".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("default-hevc"));
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn malformed_request_payload_is_accepted_then_terminal_error() {
    let request = OperationRequest {
        operation: OperationKind::TranscodeVideo,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config_path())
            .await
            .unwrap(),
    );

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::MalformedWorkerResult,
        ErrorCode::MalformedWorkerResult,
    );
}

#[tokio::test]
async fn unsupported_operation_returns_unknown_operation_protocol_error() {
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id: LeaseId(42),
        payload: serde_json::Value::Null,
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let err = handle_operation(request).await.unwrap_err();

    assert!(matches!(err, ProtocolError::UnknownOperation { .. }));
}

async fn request(root: &Path, input: &Path) -> TranscodeVideoRequest {
    let stage = root.join("stage");
    tokio::fs::create_dir(&stage).await.unwrap();
    let expected = if tokio::fs::try_exists(input).await.unwrap() {
        let observed = crate::observe_file_facts(input).await.unwrap();
        TranscodeVideoExpectedFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash,
            modified_at: observed.modified_at,
            local_file_key: None,
        }
    } else {
        TranscodeVideoExpectedFacts {
            size_bytes: 1,
            content_hash: "blake3:missing".to_owned(),
            modified_at: None,
            local_file_key: None,
        }
    };
    TranscodeVideoRequest {
        input: TranscodeVideoInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: TranscodeVideoOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.hevc.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
    }
}

fn config(root: &Path) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\"}]}\nJSON\n",
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn config_path() -> FfmpegConfig {
    let dir = tempfile::tempdir().unwrap().keep();
    config(&dir)
}

fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
}

fn handle_operation_with_test_config(
    req: OperationRequest,
    config: FfmpegConfig,
) -> OperationFuture {
    operation_handler(config)(req)
}

fn dispatch_frames(dispatch: OperationDispatch) -> Vec<ProgressFrame> {
    let voom_worker_protocol::http::OperationBody::Buffered(body) = dispatch.body else {
        panic!("ffmpeg worker should buffer test responses");
    };
    body.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn assert_terminal_error(frame: &ProgressFrame, class: FailureClass, code: ErrorCode) {
    let ProgressFrame::Error {
        class: actual_class,
        code: actual_code,
        message,
        payload,
        ..
    } = frame
    else {
        panic!("expected terminal error frame, got {frame:?}");
    };
    assert_eq!(*actual_class, class);
    assert_eq!(*actual_code, code);
    assert!(!message.trim().is_empty());
    assert!(payload.is_some());
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
