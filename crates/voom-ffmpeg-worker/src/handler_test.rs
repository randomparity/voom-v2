use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use time::OffsetDateTime;
use voom_core::{ErrorCode, FailureClass, LeaseId, WorkerId};
use voom_worker_protocol::http::{HttpClient, HttpServer};
use voom_worker_protocol::{
    AUDIO_PROFILE_DEFAULT, AudioExpectedFacts, AudioStreamRef, ClientHandle, ExtractAudioInput,
    ExtractAudioOutput, ExtractAudioOutputDescriptor, ExtractAudioRequest, NdjsonOutcome,
    OperationDispatch, OperationFuture, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, ServerHandle, TranscodeAudioInput, TranscodeAudioOutput, TranscodeAudioRequest,
    TranscodeAudioSelection, TranscodeAudioSettings, TranscodeVideoExpectedFacts,
    TranscodeVideoInput, TranscodeVideoOutput, TranscodeVideoProfile, TranscodeVideoRequest,
    WorkerCredentials,
};

use crate::DEFAULT_PROCESS_TIMEOUT;

use super::*;

#[test]
fn ffmpeg_malformed_media_maps_to_non_retriable_malformed_media() {
    // The transient tool failures stay ExternalSystemUnavailable...
    let transient: TranscodeVideoError = FfmpegError::FfmpegFailed("boom".to_owned()).into();
    assert_eq!(
        transient.failure_class(),
        FailureClass::ExternalSystemUnavailable
    );
    assert!(transient.failure_class().is_retriable());
    // ...while a structural-input fault surfaces the permanent MalformedMedia
    // class + code (#248).
    let malformed: TranscodeVideoError =
        FfmpegError::MalformedMedia("Invalid data found when processing input".to_owned()).into();
    assert_eq!(malformed.failure_class(), FailureClass::MalformedMedia);
    assert_eq!(malformed.error_code(), ErrorCode::MalformedMedia);
    assert!(!malformed.failure_class().is_retriable());
}

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
async fn one_shot_nvenc_request_requires_configured_run_local_worker() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile.encoder = "hevc_nvenc".to_owned();
    request.profile.crf = None;
    request.profile.cq = Some(22);
    request.profile.preset = Some("p5".to_owned());

    let source = InputProbe {
        width: 1920,
        height: 1080,
        codec: "h264".to_owned(),
        pixel_format: "yuv420p".to_owned(),
        codec_profile: None,
        codec_level: None,
        video_stream_count: 1,
        forced_subtitle_ordinals: Vec::new(),
    };
    let err = validate_video_hardware_binding(&request, &config(dir.path()), &source).unwrap_err();

    assert!(
        err.to_string()
            .contains("voom worker run-local --kind ffmpeg --nvidia-device GPU-<uuid>")
    );
}

/// A `hevc_vaapi` profile with a VAAPI decode mode, as migration 0032 stores one.
fn vaapi_profile(decode: voom_core::VideoDecodeMode) -> TranscodeVideoProfile {
    TranscodeVideoProfile {
        name: "hevc-vaapi".to_owned(),
        target_codec: "hevc".to_owned(),
        encoder: "hevc_vaapi".to_owned(),
        crf: None,
        cq: None,
        qp: Some(24),
        bitrate_kbps: None,
        preset: None,
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: Some("nv12".to_owned()),
        max_width: None,
        max_height: None,
        decode,
        copy_compatible: false,
    }
}

fn vaapi_binding(root: &Path, decoders: Vec<String>) -> VaapiDeviceBinding {
    VaapiDeviceBinding {
        render_node: root.join("renderD129"),
        descriptor: voom_worker_protocol::VaapiVideoAcceleratorDescriptor {
            pci_address: "0000:f4:00.0".to_owned(),
            device_name: "radeonsi".to_owned(),
            driver_version: "Mesa Gallium 26.1.5 (radeonsi, strix_halo)".to_owned(),
            encoders: vec!["hevc_vaapi".to_owned()],
            decoders,
            max_sessions: 1,
        },
    }
}

/// A VAAPI transcode is only legal on a worker the scheduler bound to a device.
/// An unassigned one used to be treated as merely "non-software" and rejected with
/// the software worker's message, which told the operator nothing about VAAPI.
#[tokio::test]
async fn one_shot_vaapi_request_requires_configured_run_local_worker() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::default());

    let err = validate_video_hardware_binding(
        &request,
        &config(dir.path()),
        &input_probe_with_codec("h264"),
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("voom worker run-local --kind ffmpeg --vaapi-device"),
        "{err}"
    );
}

/// The assignment must name the device this worker actually bound. VAAPI identity
/// is the PCI address (ADR 0052 §1), so both it and the derived token are checked:
/// a mismatch means the scheduler leased a different device than the one
/// `-vaapi_device` would open.
#[tokio::test]
async fn vaapi_assignment_for_another_device_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::default());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:03:00.0",
        "0000:03:00.0",
    ));
    let config = config(dir.path()).with_vaapi_device(vaapi_binding(
        dir.path(),
        vec!["h264".to_owned(), "hevc".to_owned()],
    ));

    let err = validate_video_hardware_binding(&request, &config, &input_probe_with_codec("h264"))
        .unwrap_err();

    assert!(err.to_string().contains("0000:03:00.0"), "{err}");
    assert!(err.to_string().contains("0000:f4:00.0"), "{err}");
}

#[tokio::test]
async fn assigned_vaapi_work_on_an_unbound_worker_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::default());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));

    let err = validate_video_hardware_binding(
        &request,
        &config(dir.path()),
        &input_probe_with_codec("h264"),
    )
    .unwrap_err();

    assert!(err.to_string().contains("unbound"), "{err}");
}

/// Advertised decode capability is probe-proven per codec (ADR 0052 §2), so a
/// `vaapi`-decode request for a codec this driver build never decoded must fail
/// rather than let ffmpeg discover it mid-encode.
#[tokio::test]
async fn vaapi_decode_requires_a_probe_proven_decoder_for_the_source_codec() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::vaapi());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));

    validate_video_hardware_binding(&request, &config, &input_probe_with_codec("hevc")).unwrap();
    let err = validate_video_hardware_binding(&request, &config, &input_probe_with_codec("av1"))
        .unwrap_err();

    assert!(err.to_string().contains("av1"), "{err}");
    assert!(err.to_string().contains("0000:f4:00.0"), "{err}");
}

/// `h265` is the alias `vaapi_video_decode_codec` folds onto `hevc`, and the planner
/// and scheduler both compare through it. An exact string test here accepted a
/// narrower set than they did, so a source ffprobe spells that way was planned,
/// scheduled, and then refused by the worker for a decoder the device had proven.
#[tokio::test]
async fn vaapi_decode_accepts_the_source_codec_alias_the_planner_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::vaapi());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));

    validate_video_hardware_binding(&request, &config, &input_probe_with_codec("h265")).unwrap();
    validate_video_hardware_binding(&request, &config, &input_probe_with_codec("HEVC")).unwrap();

    // Still refuses a codec the device genuinely never probed.
    let err = validate_video_hardware_binding(&request, &config, &input_probe_with_codec("av1"))
        .unwrap_err();
    assert!(err.to_string().contains("av1"), "{err}");
}

/// A VAAPI-decoded source reaches the encoder as hardware frames at the depth the
/// *decoder* chose, because `vaapi_filter_args` deliberately emits no `-vf` on
/// that path. Pairing a 10-bit source with an 8-bit surface therefore cannot be
/// reconciled anywhere downstream: on real hardware `hevc_vaapi` answers
/// `No usable encoding profile found`, which reaches the operator as a worker
/// crash wrapping an `FFmpeg` dump instead of a typed, actionable config error.
#[tokio::test]
async fn vaapi_decode_rejects_a_source_whose_bit_depth_the_surface_cannot_carry() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::vaapi());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));
    let mut ten_bit = input_probe_with_codec("hevc");
    ten_bit.pixel_format = "yuv420p10le".to_owned();

    let err = validate_video_hardware_binding(&request, &config, &ten_bit).unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::ConfigInvalid { .. }),
        "expected ConfigInvalid, got: {err}"
    );
    assert!(err.to_string().contains("yuv420p10le"), "{err}");
    assert!(err.to_string().contains("nv12"), "{err}");
}

/// The check pins depth, not format spelling: `p010` surfaces carry the 10-bit
/// source the previous test rejects, and an absent `pixel_format` must default to
/// nv12 exactly as `vaapi_surface_format` does — reading it as "no declared
/// depth" would reject every profile that omits the field.
#[tokio::test]
async fn vaapi_decode_accepts_each_surface_that_matches_the_source_bit_depth() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::vaapi());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));
    let eight_bit = input_probe_with_codec("hevc");
    let mut ten_bit = input_probe_with_codec("hevc");
    ten_bit.pixel_format = "yuv420p10le".to_owned();

    validate_video_hardware_binding(&request, &config, &eight_bit).unwrap();

    request.profile.pixel_format = Some("p010".to_owned());
    validate_video_hardware_binding(&request, &config, &ten_bit).unwrap();

    request.profile.pixel_format = None;
    validate_video_hardware_binding(&request, &config, &eight_bit).unwrap();
}

/// Software decode uploads through `format=<surface>,hwupload`, which converts the
/// frame before it reaches the device. A depth change there is the operator asking
/// for exactly that conversion, so the check must not fire.
#[tokio::test]
async fn software_decode_into_a_vaapi_encoder_still_allows_a_depth_change() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    request.profile = vaapi_profile(voom_core::VideoDecodeMode::default());
    request.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        "vaapi:pci-0000:f4:00.0",
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));
    let mut ten_bit = input_probe_with_codec("hevc");
    ten_bit.pixel_format = "yuv420p10le".to_owned();

    validate_video_hardware_binding(&request, &config, &ten_bit).unwrap();
}

/// ADR 0049 §5, applied to the new backend: a device-bound worker does not run
/// software video work, so it cannot occupy a GPU with an encode any worker could
/// have done. The software branch tests `config.accelerator()`, so a config that
/// did not represent a bound VAAPI device would read as unaccelerated here and
/// **accept** the assignment — the silent software fallback #409 forbids, with the
/// GPU idle and the queue none the wiser.
///
/// The same software request must still be accepted by a genuinely unbound worker,
/// with or without an explicit software assignment: refusing there would strand
/// every software transcode.
#[tokio::test]
async fn a_vaapi_bound_worker_refuses_software_video_work() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    let mut request = request(dir.path(), &input).await;
    let bound =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));
    let unbound = config(dir.path());

    let err = validate_video_hardware_binding(&request, &bound, &input_probe_with_codec("hevc"))
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("software video work requires an unbound software worker"),
        "{err}"
    );
    validate_video_hardware_binding(&request, &unbound, &input_probe_with_codec("hevc")).unwrap();
    request.hardware_assignment = Some(VideoHardwareAssignment::software());
    validate_video_hardware_binding(&request, &unbound, &input_probe_with_codec("hevc")).unwrap();
    assert!(
        validate_video_hardware_binding(&request, &bound, &input_probe_with_codec("hevc")).is_err()
    );
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
async fn dash_leading_input_path_is_config_invalid_not_unavailable() {
    // M14: a path beginning with '-' is parsed by ffmpeg as an option, not a
    // filename. The input path is not staging-validated (only existence-checked),
    // so without an explicit guard a leading-'-' input would slip through as a
    // missing file (ArtifactUnavailable) rather than a rejected contract.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.input.path = "-injected.mkv".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[test]
fn reject_option_like_path_flags_only_leading_dash() {
    assert!(reject_option_like_path("p", Path::new("-foo.mkv")).is_err());
    assert!(reject_option_like_path("p", Path::new("--")).is_err());
    // An absolute staging path (the normal case) begins with '/', and a
    // leading-'-' component *inside* an absolute path is harmless because the
    // whole arg no longer begins with '-'.
    assert!(reject_option_like_path("p", Path::new("/stage/-foo.mkv")).is_ok());
    assert!(reject_option_like_path("p", Path::new("/stage/out.mkv")).is_ok());
    assert!(reject_option_like_path("p", Path::new("out.mkv")).is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn existing_video_output_symlink_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = request(dir.path(), &input).await;
    std::os::unix::fs::symlink(dir.path().join("missing-target.mkv"), &request.output.path)
        .unwrap();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("already exists"));
}

#[tokio::test]
async fn unsupported_output_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    // mp4 is now supported; use avi which is not supported
    request.output.container = "avi".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn unsupported_profile_contract_is_rejected_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    // libx264 is not a recognized encoder — descriptor validation rejects it
    request.profile.encoder = "libx264".to_owned();

    let err = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    let message = err.to_string();
    assert!(
        message.contains("default-hevc") && message.contains("unknown encoder `libx264`"),
        "unexpected error: {err}"
    );
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn unavailable_encoder_is_config_invalid_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.profile = TranscodeVideoProfile {
        name: "av1-archive".to_owned(),
        target_codec: "av1".to_owned(),
        encoder: "libaom-av1".to_owned(),
        crf: Some(35),
        cq: None,
        qp: None,
        bitrate_kbps: None,
        preset: Some("8".to_owned()),
        tune: None,
        codec_profile: None,
        codec_level: None,
        pixel_format: None,
        max_width: None,
        max_height: None,
        copy_compatible: false,
        decode: voom_core::VideoDecodeMode::default(),
    };
    request.output.video_codec = "av1".to_owned();
    let config = config(dir.path())
        .with_available_video_encoders(["libx265".to_owned(), "libsvtav1".to_owned()]);

    let err = handle_transcode_video(&request, &config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string().contains("libaom-av1") && err.to_string().contains("not available"),
        "unexpected error: {err}"
    );
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

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
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

#[tokio::test]
async fn streaming_operation_acknowledges_and_reports_progress_before_completion() {
    const PROGRESS_IDLE_DEADLINE_MS: u32 = 40;

    let credentials = WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 1,
        secret: "secret".to_owned().into(),
    };
    let operation_release = Arc::new(tokio::sync::Notify::new());
    let handler_release = Arc::clone(&operation_release);
    let handler = Arc::new(move |request: OperationRequest| {
        let operation_release = Arc::clone(&handler_release);
        Box::pin(async move {
            stream_operation(
                StreamingOperation {
                    lease_id: request.lease_id,
                    accepted_at: OffsetDateTime::now_utc(),
                    progress_idle_deadline_ms: PROGRESS_IDLE_DEADLINE_MS,
                    started_message: "started",
                    active_message: "active",
                },
                async move {
                    operation_release.notified().await;
                    Ok(serde_json::json!({"status": "done"}))
                },
            )
        }) as OperationFuture
    });
    let running = HttpServer::new(credentials.clone(), handler)
        .serve("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let client = HttpClient::with_timeouts(
        running.bound,
        Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let request = OperationRequest {
        operation: OperationKind::TranscodeVideo,
        lease_id: LeaseId(42),
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 100,
        progress_idle_deadline_ms: PROGRESS_IDLE_DEADLINE_MS,
    };

    let mut dispatch = client
        .dispatch(&credentials, "streaming-ack", request)
        .await
        .unwrap();

    let mut progress_count = 0;
    loop {
        let outcome = tokio::time::timeout(Duration::from_secs(1), dispatch.frames.next_frame())
            .await
            .unwrap()
            .unwrap();
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {
                progress_count += 1;
                if progress_count == 2 {
                    operation_release.notify_one();
                }
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { .. }) => break,
            other => {
                assert!(matches!(
                    other,
                    NdjsonOutcome::Terminated(ProgressFrame::Result { .. })
                ));
                break;
            }
        }
    }
    assert!(progress_count >= 2);
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

#[tokio::test]
async fn transcode_audio_operation_decodes_typed_payload() {
    let request = OperationRequest {
        operation: OperationKind::TranscodeAudio,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
            .await
            .unwrap(),
    );

    let ProgressFrame::Error { message, .. } = frames.last().unwrap() else {
        panic!("expected terminal error");
    };
    assert!(message.contains("transcode_audio payload decode"));
}

#[tokio::test]
async fn extract_audio_operation_decodes_typed_payload() {
    let request = OperationRequest {
        operation: OperationKind::ExtractAudio,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let (config, _config_dir) = config_path();
    let frames = dispatch_frames(
        handle_operation_with_test_config(request, config)
            .await
            .unwrap(),
    );

    let ProgressFrame::Error { message, .. } = frames.last().unwrap() else {
        panic!("expected terminal error");
    };
    assert!(message.contains("extract_audio payload decode"));
}

#[tokio::test]
async fn transcode_audio_existing_output_path_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");
    tokio::fs::write(&request.output.path, b"exists")
        .await
        .unwrap();

    let err = handle_transcode_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn transcode_audio_accepts_eac3_target() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "eac3");

    let result = handle_transcode_audio(
        &request,
        &audio_config(dir.path(), "matroska", "eac3", "stream-1", "eng", "Main", 1),
    )
    .await
    .unwrap();

    assert_eq!(result.output_audio_codecs, vec!["eac3".to_owned()]);
}

#[tokio::test]
async fn transcode_audio_rejects_unsupported_codec_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "flac");

    let err = handle_transcode_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn transcode_audio_rejects_unknown_profile_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request =
        transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "eac3");
    request.audio.profile = "premium".to_owned();

    let err = handle_transcode_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn synthesize_audio_without_target_channels_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request =
        transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "aac");
    request.audio.add_track = true;
    request.audio.target_channels = None;

    let err = handle_transcode_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn extract_audio_output_path_outside_staging_root_is_config_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = extract_audio_request(dir.path(), &input, audio_expected(&input).await);
    request.output.path = dir
        .path()
        .join("../escape.ogg")
        .to_string_lossy()
        .into_owned();

    let err = handle_extract_audio(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
}

#[tokio::test]
async fn transcode_audio_rejects_selected_stream_id_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");

    let err = handle_transcode_audio(
        &request,
        &audio_config(dir.path(), "matroska", "opus", "stream-9", "eng", "Main", 1),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn transcode_audio_rejects_selected_output_ordering_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request =
        transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");
    request.selection.selected_streams.push(AudioStreamRef {
        snapshot_stream_id: "stream-3".to_owned(),
        provider_stream_index: 3,
    });

    let err = handle_transcode_audio(&request, &audio_config_two_outputs_reversed(dir.path()))
        .await
        .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn transcode_audio_rejects_preservation_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = transcode_audio_request(dir.path(), &input, audio_expected(&input).await, "opus");

    let err = handle_transcode_audio(
        &request,
        &audio_config(dir.path(), "matroska", "opus", "stream-1", "fra", "Main", 1),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn extract_audio_rejects_dropped_source_language_or_title() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = extract_audio_request(dir.path(), &input, audio_expected(&input).await);

    let err = handle_extract_audio(
        &request,
        &audio_extract_config(dir.path(), None, Some("Main")),
    )
    .await
    .unwrap_err();

    assert_eq!(err.failure_class(), FailureClass::MalformedWorkerResult);
}

#[tokio::test]
async fn extract_audio_executes_plural_outputs_in_source_order() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = plural_extract_audio_request(dir.path(), &input, audio_expected(&input).await);

    let result = handle_extract_audio(
        &request,
        &audio_extract_plural_config(
            dir.path(),
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
        ),
    )
    .await
    .unwrap();

    let outputs = result.outputs.unwrap();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].output_id, "extract_output_1");
    assert_eq!(outputs[0].selection.snapshot_stream_id, "stream-1");
    assert_eq!(outputs[1].output_id, "extract_output_2");
    assert_eq!(outputs[1].selection.snapshot_stream_id, "stream-2");
    assert_eq!(outputs[1].output_language.as_deref(), Some("jpn"));
    assert_eq!(outputs[1].output_title.as_deref(), Some("Second"));
    assert!(Path::new(&outputs[0].path).is_file());
    assert!(Path::new(&outputs[1].path).is_file());
}

#[tokio::test]
async fn extract_audio_allows_identical_facts_for_distinct_plural_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = plural_extract_audio_request(dir.path(), &input, audio_expected(&input).await);

    let result = handle_extract_audio(
        &request,
        &audio_extract_plural_config_with_metadata(
            dir.path(),
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
            true,
        ),
    )
    .await
    .unwrap();

    let outputs = result.outputs.unwrap();
    assert_ne!(outputs[0].output_id, outputs[1].output_id);
    assert_ne!(outputs[0].path, outputs[1].path);
    assert_eq!(outputs[0].output.size_bytes, outputs[1].output.size_bytes);
    assert_eq!(
        outputs[0].output.content_hash,
        outputs[1].output.content_hash
    );
    assert_eq!(outputs[0].output_language, outputs[1].output_language);
    assert_eq!(outputs[0].output_title, outputs[1].output_title);
}

#[tokio::test]
async fn extract_audio_rejects_plural_path_collision_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let marker = dir.path().join("ffmpeg-invoked");
    let mut request =
        plural_extract_audio_request(dir.path(), &input, audio_expected(&input).await);
    request.outputs.as_mut().unwrap()[1].output.path = request.outputs.as_ref().unwrap()[0]
        .output
        .path
        .to_uppercase();
    let ffmpeg = format!(
        "#!/bin/sh\nprintf invoked > '{}'\nexit 1\n",
        marker.display()
    );

    let error = handle_extract_audio(&request, &audio_extract_plural_config(dir.path(), &ffmpeg))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::ConfigInvalid);
    assert!(!marker.exists());
}

#[tokio::test]
async fn extract_audio_second_output_failure_returns_no_partial_result() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = plural_extract_audio_request(dir.path(), &input, audio_expected(&input).await);
    let ffmpeg = "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *second*) exit 9 ;;\n  *) printf output > \"$last\" ;;\nesac\n";

    let error = handle_extract_audio(&request, &audio_extract_plural_config(dir.path(), ffmpeg))
        .await
        .unwrap_err();

    assert_eq!(
        error.failure_class(),
        FailureClass::ExternalSystemUnavailable
    );
    let outputs = request.outputs.as_ref().unwrap();
    assert!(Path::new(&outputs[0].output.path).is_file());
    assert!(!Path::new(&outputs[1].output.path).exists());
}

// ---- Task 7.2 tests ----

#[tokio::test]
async fn copy_video_with_nonconforming_codec_fails_loudly() {
    // copy_video=true but ffprobe reports h264 (not the target hevc)
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    // ffprobe reports h264 for the input
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"h264\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(
            err,
            TranscodeVideoError::MalformedWorkerResult { .. }
                | TranscodeVideoError::ConfigInvalid { .. }
        ),
        "expected MalformedWorkerResult or ConfigInvalid, got: {err}"
    );
}

#[tokio::test]
async fn mp4_output_contract_now_accepted() {
    // mp4 was previously rejected; now it is a supported container
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.output.container = "mp4".to_owned();
    req.output.path = dir
        .path()
        .join("stage")
        .join("input.hevc.mp4")
        .to_string_lossy()
        .into_owned();
    // ffprobe returns mp4/hevc for output validation
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"mp4\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    // Should succeed — mp4 is now accepted
    let result = handle_transcode_video(&req, &config).await;
    assert!(
        result.is_ok(),
        "mp4 output should now be accepted: {result:?}"
    );
    let result = result.unwrap();
    assert_eq!(result.output_container, "mp4");
}

#[tokio::test]
async fn output_dims_and_pixfmt_populated_from_probe() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let req = request(dir.path(), &input).await;
    let config = config(dir.path());

    let result = handle_transcode_video(&req, &config).await.unwrap();
    assert_eq!(result.output_width, 1920);
    assert_eq!(result.output_height, 1080);
    assert_eq!(result.output_pixel_format, "yuv420p");
    assert!(!result.copied_video);
}

#[tokio::test]
async fn expected_source_pixel_format_mismatch_fails_before_ffmpeg() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut request = request(dir.path(), &input).await;
    request.input.video_pixel_format = Some("yuv420p10le".to_owned());

    let error = handle_transcode_video(&request, &config(dir.path()))
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        TranscodeVideoError::MalformedWorkerResult { .. }
    ));
    assert!(error.to_string().contains("source pixel format"));
    assert!(!Path::new(&request.output.path).exists());
}

#[tokio::test]
async fn copy_video_sets_copied_video_flag() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    // ffprobe returns hevc/mkv — matches the target codec
    let config = config(dir.path());

    let result = handle_transcode_video(&req, &config).await.unwrap();
    assert!(result.copied_video);
}

/// `-c:v copy` runs no encoder, so a stream copy under a VAAPI profile is not a hardware
/// operation and must be allowed when the source already conforms. The source's pixel
/// format is a *file* format (`yuv420p`), while the profile names the `nv12` surface the
/// encoder would have consumed; comparing those two refused every legitimate copy.
///
/// The worker still requires the VAAPI assignment even for a copy — the scheduler leased
/// this device for this ticket, and accepting work for another device would break the
/// per-device model regardless of whether an encoder runs.
#[tokio::test]
async fn copy_video_is_allowed_under_a_vaapi_profile_when_the_source_conforms() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    req.profile = vaapi_copy_profile();
    req.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        vaapi_hardware_token("0000:f4:00.0"),
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));

    let result = handle_transcode_video(&req, &config).await.unwrap();

    assert!(result.copied_video);
    assert_eq!(result.output_pixel_format, "yuv420p");
}

/// The mapping is not a blanket exemption: a source whose file format is not what the
/// requested surface writes still cannot be copied.
#[tokio::test]
async fn copy_video_is_refused_under_a_vaapi_profile_when_the_source_bit_depth_differs() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    let mut profile = vaapi_copy_profile();
    // A p010 surface writes yuv420p10le; the stubbed source is yuv420p.
    profile.pixel_format = Some("p010".to_owned());
    req.profile = profile;
    req.hardware_assignment = Some(VideoHardwareAssignment::vaapi(
        vaapi_hardware_token("0000:f4:00.0"),
        "0000:f4:00.0",
    ));
    let config =
        config(dir.path()).with_vaapi_device(vaapi_binding(dir.path(), vec!["hevc".to_owned()]));

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::MalformedWorkerResult { .. }),
        "expected MalformedWorkerResult, got: {err}"
    );
    assert!(err.to_string().contains("yuv420p10le"), "{err}");
}

fn vaapi_copy_profile() -> TranscodeVideoProfile {
    let mut profile = TranscodeVideoProfile::default_hevc();
    profile.name = "hevc-vaapi-copy".to_owned();
    profile.encoder = "hevc_vaapi".to_owned();
    profile.crf = None;
    profile.qp = Some(24);
    profile.preset = None;
    profile.pixel_format = Some("nv12".to_owned());
    profile.copy_compatible = true;
    profile
}

#[tokio::test]
async fn copy_video_with_constrained_profile_but_unknown_source_profile_fails_loudly() {
    // Profile constrains codec_profile=main10, but the source probe reports no
    // profile field (None). We cannot prove conformance → must fail loudly.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let mut req = request(dir.path(), &input).await;
    req.copy_video = true;
    req.profile.codec_profile = Some("main10".to_owned());
    req.profile.pixel_format = Some("yuv420p10le".to_owned());
    // ffprobe reports hevc (matches codec) but emits NO "profile" key → None.
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p10le\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::MalformedWorkerResult { .. }),
        "expected MalformedWorkerResult for unknown source codec_profile, got: {err}"
    );
    assert!(
        err.to_string().contains("codec_profile"),
        "error should mention codec_profile: {err}"
    );
}

#[tokio::test]
async fn multi_video_stream_source_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.mkv");
    tokio::fs::write(&input, b"input").await.unwrap();
    let req = request(dir.path(), &input).await;
    // ffprobe reports two video streams.
    let config = config_with_probe(
        dir.path(),
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"},{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":640,\"height\":360,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );

    let err = handle_transcode_video(&req, &config).await.unwrap_err();

    assert!(
        matches!(err, TranscodeVideoError::ConfigInvalid { .. }),
        "expected ConfigInvalid for multi-video-stream source, got: {err}"
    );
    assert!(
        err.to_string().contains('2'),
        "error should name the video stream count: {err}"
    );
}

fn config_with_probe(root: &Path, probe_script: &str) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(root, "ffprobe", probe_script);
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

// ---- End Task 7.2 tests ----

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
            video_codec: None,
            video_pixel_format: None,
        },
        output: TranscodeVideoOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.hevc.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            video_codec: "hevc".to_owned(),
            overwrite: false,
        },
        profile: TranscodeVideoProfile::default_hevc(),
        hardware_assignment: None,
        copy_video: false,
    }
}

fn config(root: &Path) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    // ffprobe returns the same JSON for both probe_input and probe_output calls.
    // Includes width/height/pix_fmt so both probes succeed.
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        "#!/bin/sh\ncat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"codec_type\":\"video\",\"codec_name\":\"hevc\",\"width\":1920,\"height\":1080,\"pix_fmt\":\"yuv420p\"}]}\nJSON\n",
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_config(
    root: &Path,
    container: &str,
    codec: &str,
    snapshot_id: &str,
    language: &str,
    title: &str,
    default: u8,
) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        &format!(
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *audio-stage*) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"{container}\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"{codec}\",\"channels\":6,\"tags\":{{\"snapshot_stream_id\":\"{snapshot_id}\",\"language\":\"{language}\",\"title\":\"{title}\"}},\"disposition\":{{\"default\":{default},\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\n  *) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":6,\"tags\":{{\"language\":\"eng\",\"title\":\"Main\"}},\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\nesac\n"
        ),
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_config_two_outputs_reversed(root: &Path) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *audio-stage*) cat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":3,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"channels\":2,\"tags\":{\"snapshot_stream_id\":\"stream-3\",\"language\":\"jpn\",\"title\":\"Commentary\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":1}},{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"channels\":6,\"tags\":{\"snapshot_stream_id\":\"stream-1\",\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}}]}\nJSON\n    ;;\n  *) cat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":6,\"tags\":{\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}},{\"index\":3,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"channels\":2,\"tags\":{\"language\":\"jpn\",\"title\":\"Commentary\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":1}}]}\nJSON\n    ;;\nesac\n",
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

fn audio_extract_config(root: &Path, language: Option<&str>, title: Option<&str>) -> FfmpegConfig {
    let ffmpeg = stub_bin(
        root,
        "ffmpeg",
        "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf output > \"$last\"\n",
    );
    let tags = match (language, title) {
        (Some(language), Some(title)) => {
            format!("\"tags\":{{\"language\":\"{language}\",\"title\":\"{title}\"}},")
        }
        (Some(language), None) => format!("\"tags\":{{\"language\":\"{language}\"}},"),
        (None, Some(title)) => format!("\"tags\":{{\"title\":\"{title}\"}},"),
        (None, None) => String::new(),
    };
    let ffprobe = stub_bin(
        root,
        "ffprobe",
        &format!(
            "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *extract-stage*) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"ogg\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"opus\",{tags}\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\n  *) cat <<'JSON'\n{{\"format\":{{\"format_name\":\"matroska\"}},\"streams\":[{{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"tags\":{{\"language\":\"eng\",\"title\":\"Main\"}},\"disposition\":{{\"default\":1,\"forced\":0,\"comment\":0}}}}]}}\nJSON\n    ;;\nesac\n"
        ),
    );
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

async fn audio_expected(input: &Path) -> AudioExpectedFacts {
    let observed = crate::observe_file_facts(input).await.unwrap();
    AudioExpectedFacts {
        size_bytes: observed.size_bytes,
        content_hash: observed.content_hash,
        modified_at: observed.modified_at,
        local_file_key: None,
    }
}

fn transcode_audio_request(
    root: &Path,
    input: &Path,
    expected: AudioExpectedFacts,
    target_codec: &str,
) -> TranscodeAudioRequest {
    let stage = root.join("audio-stage");
    std::fs::create_dir_all(&stage).unwrap();
    TranscodeAudioRequest {
        input: TranscodeAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: TranscodeAudioOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.audio.mkv").to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: TranscodeAudioSelection {
            selected_streams: vec![AudioStreamRef {
                snapshot_stream_id: "stream-1".to_owned(),
                provider_stream_index: 1,
            }],
        },
        audio: TranscodeAudioSettings {
            target_codec: target_codec.to_owned(),
            profile: AUDIO_PROFILE_DEFAULT.to_owned(),
            add_track: false,
            target_channels: None,
        },
    }
}

fn extract_audio_request(
    root: &Path,
    input: &Path,
    expected: AudioExpectedFacts,
) -> ExtractAudioRequest {
    let stage = root.join("extract-stage");
    std::fs::create_dir_all(&stage).unwrap();
    ExtractAudioRequest {
        input: ExtractAudioInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: ExtractAudioOutput {
            staging_root: stage.to_string_lossy().into_owned(),
            path: stage.join("input.audio.ogg").to_string_lossy().into_owned(),
            container: "ogg".to_owned(),
            audio_codec: "opus".to_owned(),
            overwrite: false,
        },
        selection: AudioStreamRef {
            snapshot_stream_id: "stream-1".to_owned(),
            provider_stream_index: 1,
        },
        outputs: None,
    }
}

fn plural_extract_audio_request(
    root: &Path,
    input: &Path,
    expected: AudioExpectedFacts,
) -> ExtractAudioRequest {
    let mut request = extract_audio_request(root, input, expected);
    let first = ExtractAudioOutputDescriptor {
        output_id: "extract_output_1".to_owned(),
        selection: request.selection.clone(),
        output: request.output.clone(),
    };
    let second = ExtractAudioOutputDescriptor {
        output_id: "extract_output_2".to_owned(),
        selection: AudioStreamRef {
            snapshot_stream_id: "stream-2".to_owned(),
            provider_stream_index: 2,
        },
        output: ExtractAudioOutput {
            staging_root: request.output.staging_root.clone(),
            path: root
                .join("extract-stage/input.second.audio.ogg")
                .to_string_lossy()
                .into_owned(),
            container: "ogg".to_owned(),
            audio_codec: "opus".to_owned(),
            overwrite: false,
        },
    };
    request.outputs = Some(vec![first, second]);
    request
}

fn audio_extract_plural_config(root: &Path, ffmpeg_body: &str) -> FfmpegConfig {
    audio_extract_plural_config_with_metadata(root, ffmpeg_body, false)
}

fn audio_extract_plural_config_with_metadata(
    root: &Path,
    ffmpeg_body: &str,
    identical_metadata: bool,
) -> FfmpegConfig {
    let ffmpeg = stub_bin(root, "ffmpeg", ffmpeg_body);
    let probe_body = "#!/bin/sh\nlast=\"\"\nfor arg in \"$@\"; do last=\"$arg\"; done\ncase \"$last\" in\n  *extract-stage*second*) cat <<'JSON'\n{\"format\":{\"format_name\":\"ogg\"},\"streams\":[{\"index\":0,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"tags\":{\"snapshot_stream_id\":\"stream-2\",\"language\":\"jpn\",\"title\":\"Second\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":0}}]}\nJSON\n    ;;\n  *extract-stage*) cat <<'JSON'\n{\"format\":{\"format_name\":\"ogg\"},\"streams\":[{\"index\":0,\"codec_type\":\"audio\",\"codec_name\":\"opus\",\"tags\":{\"snapshot_stream_id\":\"stream-1\",\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}}]}\nJSON\n    ;;\n  *) cat <<'JSON'\n{\"format\":{\"format_name\":\"matroska\"},\"streams\":[{\"index\":1,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"tags\":{\"language\":\"eng\",\"title\":\"Main\"},\"disposition\":{\"default\":1,\"forced\":0,\"comment\":0}},{\"index\":2,\"codec_type\":\"audio\",\"codec_name\":\"aac\",\"tags\":{\"language\":\"jpn\",\"title\":\"Second\"},\"disposition\":{\"default\":0,\"forced\":0,\"comment\":0}}]}\nJSON\n    ;;\nesac\n";
    let probe_body = if identical_metadata {
        probe_body
            .replace("\"language\":\"jpn\"", "\"language\":\"eng\"")
            .replace("\"title\":\"Second\"", "\"title\":\"Main\"")
    } else {
        probe_body.to_owned()
    };
    let ffprobe = stub_bin(root, "ffprobe", &probe_body);
    FfmpegConfig::new(
        ffmpeg,
        ffprobe,
        "ffmpeg version test".to_owned(),
        DEFAULT_PROCESS_TIMEOUT,
    )
}

/// Returns a config backed by stub binaries plus the `TempDir` guard. Hold the
/// guard for the test's duration so the tempdir is cleaned up afterward.
fn config_path() -> (FfmpegConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    (config, dir)
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

fn input_probe_with_codec(codec: &str) -> InputProbe {
    InputProbe {
        width: 1920,
        height: 1080,
        codec: codec.to_owned(),
        pixel_format: "yuv420p".to_owned(),
        codec_profile: None,
        codec_level: None,
        video_stream_count: 1,
        forced_subtitle_ordinals: Vec::new(),
    }
}

#[test]
fn validate_copy_codec_accepts_h265_alias_against_hevc_target() {
    let probe = input_probe_with_codec("h265");
    assert!(validate_copy_codec("hevc", &probe).is_ok());
}

#[test]
fn validate_copy_codec_accepts_hevc_against_h265_target() {
    let probe = input_probe_with_codec("hevc");
    assert!(validate_copy_codec("h265", &probe).is_ok());
}

#[test]
fn validate_copy_codec_rejects_mismatched_codec() {
    let probe = input_probe_with_codec("h264");
    let err = validate_copy_codec("hevc", &probe).unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
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
