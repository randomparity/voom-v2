use std::path::{Path, PathBuf};

use voom_core::{ErrorCode, FailureClass, LeaseId};
use voom_worker_protocol::{
    OperationDispatch, OperationFuture, OperationKind, OperationRequest, ProgressFrame,
    RemuxExpectedFacts, RemuxInput, RemuxOutput, RemuxRequest, RemuxSelection, RemuxStreamRef,
    RemuxTrackGroup,
};

use crate::observe::observe_file_facts;
use crate::preflight::MkvmergeConfig;

use super::*;

#[tokio::test]
async fn handler_rejects_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let input = root.join("input.mp4");
    let stage = root.join("stage");
    let output = stage.join("out.mkv");
    tokio::fs::create_dir_all(output.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&input, b"not real media").await.unwrap();
    tokio::fs::write(&output, b"stale").await.unwrap();

    let request = request_for_paths(&input, &stage, &output).await;
    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("output path already exists"));
}

#[cfg(unix)]
#[tokio::test]
async fn handler_rejects_dangling_output_symlink() {
    let fixture = remux_fixture().await;
    let output = PathBuf::from(&fixture.request.output.path);
    let target = output
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("outside")
        .join("out.mkv");
    std::os::unix::fs::symlink(&target, &output).unwrap();

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("output path already exists"));
    assert!(!target.exists());
}

#[tokio::test]
async fn handler_rejects_missing_input_with_artifact_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let stage = root.join("stage");
    let request =
        request_for_paths(&root.join("missing.mp4"), &stage, &stage.join("out.mkv")).await;

    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[tokio::test]
async fn handler_rejects_output_path_escape() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let input = root.join("input.mp4");
    let stage = root.join("stage");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = request_for_paths(&input, &stage, &root.join("out.mkv")).await;

    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("escapes staging root"));
}

#[tokio::test]
async fn handler_rejects_non_canonical_staging_root() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    let stage = PathBuf::from(&request.output.staging_root);
    request.output.staging_root = stage.join("../stage").to_string_lossy().into_owned();

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("staging root must be canonical"));
}

#[tokio::test]
async fn handler_rejects_no_video_selection() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection = RemuxSelection {
        keep_streams: vec![audio_ref("stream-1", 1)],
        default_streams: vec![],
        clear_default_streams: vec![],
        track_order: vec![RemuxTrackGroup::Audio],
        head_streams: vec![],
        forced_streams: vec![],
        clear_forced_streams: vec![],
    };

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("at least one video stream"));
}

#[tokio::test]
async fn handler_rejects_dropping_source_video_tracks_before_provider_run() {
    let fixture = remux_fixture_with_two_input_videos_and_forbidden_provider_run().await;
    let mut request = fixture.request;
    request.selection.keep_streams = vec![video_ref("stream-0", 0), audio_ref("stream-2", 2)];
    request.selection.default_streams = vec![audio_ref("stream-2", 2)];
    request.selection.clear_default_streams = vec![video_ref("stream-0", 0)];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("must keep all source video streams")
    );
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn handler_rejects_attachment_track_order_before_provider_run() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection.track_order = vec![RemuxTrackGroup::Video, RemuxTrackGroup::Attachment];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("unsupported attachment remux"));
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn handler_rejects_attachment_keep_stream_before_provider_run() {
    let fixture = remux_fixture_with_attachment_track_and_forbidden_provider_run().await;
    let mut request = fixture.request;
    request.selection.keep_streams = vec![video_ref("stream-0", 0), attachment_ref("stream-2", 2)];
    request.selection.default_streams = vec![];
    request.selection.clear_default_streams = vec![];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("unsupported attachment remux"));
    assert!(!tokio::fs::try_exists(&request.output.path).await.unwrap());
}

#[tokio::test]
async fn handler_rejects_default_streams_outside_keep_streams() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection.default_streams = vec![audio_ref("stream-2", 1)];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("default_streams must be a subset"));
}

#[tokio::test]
async fn handler_rejects_clear_default_streams_outside_keep_streams() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection.clear_default_streams = vec![audio_ref("stream-2", 1)];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(
        err.to_string()
            .contains("clear_default_streams must be a subset")
    );
}

#[tokio::test]
async fn handler_rejects_input_drift_after_provider_run() {
    let fixture = remux_fixture_with_fake_mkvmerge_that_mutates_input().await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
}

#[tokio::test]
async fn handler_maps_mkvmerge_nonzero_exit_to_external_system_unavailable() {
    let fixture = remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_provider_failure()).await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert!(err.to_string().contains("status 23"));
}

#[tokio::test]
async fn handler_maps_mkvmerge_timeout_to_external_system_unavailable() {
    let mut fixture =
        remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_provider_timeout()).await;
    fixture.config.timeout = std::time::Duration::from_millis(10);

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ExternalSystemUnavailable);
    assert!(err.to_string().contains("timed out"));
}

#[tokio::test]
async fn handler_rejects_selected_stream_mismatch() {
    let fixture = remux_fixture_with_output_probe(vec!["stream-0"], vec![]).await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("selected stream mismatch"));
}

#[tokio::test]
async fn handler_rejects_default_track_mismatch() {
    let fixture = remux_fixture_with_output_probe(vec!["stream-0", "stream-1"], vec![]).await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("default stream mismatch"));
}

#[tokio::test]
async fn handler_rejects_non_mkv_output_facts() {
    let fixture = remux_fixture_with_output_container("mp4").await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("output container"));
}

#[tokio::test]
async fn handler_rejects_no_video_output_tracks() {
    let fixture = remux_fixture_with_output_specs(vec![
        output_track("audio", false),
        output_track("audio", true),
    ])
    .await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(
        err.to_string()
            .contains("output must include at least one video")
    );
}

#[tokio::test]
async fn handler_rejects_wrong_selected_stream_kind_order() {
    let fixture = remux_fixture_with_output_specs(vec![
        output_track("audio", false),
        output_track("video", true),
    ])
    .await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(err.to_string().contains("selected stream mismatch"));
}

#[tokio::test]
async fn handler_rejects_wrong_same_kind_selected_track() {
    let fixture = remux_fixture_with_input_specs_and_output_specs(
        vec![
            input_track("video"),
            input_track_with_language("audio", "eng"),
            input_track_with_language("audio", "spa"),
        ],
        vec![
            output_track("video", false),
            output_track_with_language("audio", true, "spa"),
        ],
    )
    .await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(
        err.to_string()
            .contains("selected stream identity mismatch")
    );
}

#[tokio::test]
async fn handler_rejects_ambiguous_same_kind_selected_track_identity() {
    let fixture = remux_fixture_with_input_specs_and_output_specs(
        vec![
            input_track("video"),
            input_track("audio"),
            input_track("audio"),
        ],
        vec![output_track("video", false), output_track("audio", true)],
    )
    .await;

    let err = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::MalformedWorkerResult);
    assert!(
        err.to_string()
            .contains("selected stream identity is ambiguous")
    );
}

#[tokio::test]
async fn handler_accepts_video_keep_when_track_order_omits_video() {
    let fixture = remux_fixture_with_output_specs(vec![
        output_track("audio", true),
        output_track("video", false),
    ])
    .await;
    let mut request = fixture.request;
    request.selection.track_order = vec![RemuxTrackGroup::Audio];

    let result = handle_remux(&request, &fixture.config).await.unwrap();

    assert_eq!(result.kept_snapshot_stream_ids, ["stream-0", "stream-1"]);
}

#[tokio::test]
async fn handler_accepts_output_reordered_by_track_order() {
    let fixture = remux_fixture_with_output_specs(vec![
        output_track("audio", true),
        output_track("video", false),
    ])
    .await;
    let mut request = fixture.request;
    request.selection.track_order = vec![RemuxTrackGroup::Audio, RemuxTrackGroup::Video];

    let result = handle_remux(&request, &fixture.config).await.unwrap();

    assert_eq!(result.kept_snapshot_stream_ids, ["stream-0", "stream-1"]);
    assert_eq!(result.default_snapshot_stream_ids, ["stream-1"]);
}

#[tokio::test]
async fn handler_accepts_reordered_defaults_in_output_order() {
    let fixture = remux_fixture_with_input_specs_and_output_specs(
        vec![
            input_track("video"),
            input_track("audio"),
            input_track("subtitles"),
        ],
        vec![
            output_track("subtitles", true),
            output_track("video", false),
            output_track("audio", true),
        ],
    )
    .await;
    let mut request = fixture.request;
    request.selection.keep_streams = vec![
        video_ref("stream-0", 0),
        audio_ref("stream-1", 1),
        subtitle_ref("stream-2", 2),
    ];
    request.selection.default_streams = vec![audio_ref("stream-1", 1), subtitle_ref("stream-2", 2)];
    request.selection.clear_default_streams = vec![video_ref("stream-0", 0)];
    request.selection.track_order = vec![
        RemuxTrackGroup::Subtitle,
        RemuxTrackGroup::Video,
        RemuxTrackGroup::Audio,
    ];

    let result = handle_remux(&request, &fixture.config).await.unwrap();

    assert_eq!(
        result.kept_snapshot_stream_ids,
        ["stream-0", "stream-1", "stream-2"]
    );
    assert_eq!(result.default_snapshot_stream_ids, ["stream-1", "stream-2"]);
}

#[tokio::test]
async fn handler_rejects_overwrite_true() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.output.overwrite = true;

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("overwrite must be false"));
}

#[tokio::test]
async fn handler_rejects_unsupported_output_container() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.output.container = "mp4".to_owned();

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("container must be mkv"));
}

#[tokio::test]
async fn handler_rejects_duplicate_keep_snapshot_stream_id() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection.keep_streams = vec![video_ref("stream-0", 0), audio_ref("stream-0", 1)];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("duplicate snapshot_stream_id"));
}

#[tokio::test]
async fn handler_rejects_duplicate_keep_provider_index() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.selection.keep_streams = vec![video_ref("stream-0", 0), audio_ref("stream-1", 0)];

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("duplicate provider_stream_index"));
}

#[tokio::test]
async fn handler_rejects_expected_size_hash_mismatch() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.input.expected.content_hash = "blake3:not-the-current-content".to_owned();

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(err.to_string().contains("expected size/hash"));
}

#[tokio::test]
async fn handler_rejects_expected_modified_at_mismatch() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.input.expected.modified_at = Some("2000-01-01T00:00:00Z".to_owned());

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(err.to_string().contains("expected modified_at"));
}

#[tokio::test]
async fn handler_rejects_expected_local_file_key_when_observer_cannot_verify_it() {
    let fixture = remux_fixture().await;
    let mut request = fixture.request;
    request.input.expected.local_file_key = Some("dev:ino".to_owned());

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactChecksumMismatch);
    assert!(err.to_string().contains("expected local_file_key"));
}

#[tokio::test]
async fn handler_returns_success_result_echoing_selected_ids() {
    let fixture = remux_fixture().await;

    let result = handle_remux(&fixture.request, &fixture.config)
        .await
        .unwrap();

    assert_eq!(result.kept_snapshot_stream_ids, ["stream-0", "stream-1"]);
    assert_eq!(result.default_snapshot_stream_ids, ["stream-1"]);
    assert_eq!(result.output_container, "mkv");
}

#[tokio::test]
async fn malformed_request_payload_is_accepted_then_terminal_error() {
    let request = OperationRequest {
        operation: OperationKind::Remux,
        lease_id: LeaseId(42),
        payload: serde_json::json!({"input": 1}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    };

    let frames = dispatch_frames(
        handle_operation_with_test_config(request, MkvmergeConfig::for_tests())
            .await
            .unwrap(),
    );

    assert_terminal_error(
        frames.last().unwrap(),
        FailureClass::MalformedWorkerResult,
        ErrorCode::MalformedWorkerResult,
    );
}

struct RemuxFixture {
    _temp: tempfile::TempDir,
    request: RemuxRequest,
    config: MkvmergeConfig,
}

async fn remux_fixture() -> RemuxFixture {
    remux_fixture_with_output_probe(vec!["stream-0", "stream-1"], vec!["stream-1"]).await
}

async fn remux_fixture_with_output_probe(
    kept_ids: Vec<&str>,
    default_ids: Vec<&str>,
) -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body(&kept_ids, &default_ids, "mkv", false)).await
}

async fn remux_fixture_with_output_container(container: &str) -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body(
        &["stream-0", "stream-1"],
        &["stream-1"],
        container,
        false,
    ))
    .await
}

async fn remux_fixture_with_output_specs(output_specs: Vec<OutputTrackSpec>) -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_output_specs(
        &output_specs,
        "mkv",
        false,
    ))
    .await
}

async fn remux_fixture_with_input_specs_and_output_specs(
    input_specs: Vec<InputTrackSpec>,
    output_specs: Vec<OutputTrackSpec>,
) -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_input_and_output_specs(
        &input_specs,
        &output_specs,
        "mkv",
    ))
    .await
}

async fn remux_fixture_with_fake_mkvmerge_that_mutates_input() -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body(
        &["stream-0", "stream-1"],
        &["stream-1"],
        "mkv",
        true,
    ))
    .await
}

async fn remux_fixture_with_two_input_videos_and_forbidden_provider_run() -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_two_input_videos_forbidden_provider_run())
        .await
}

async fn remux_fixture_with_attachment_track_and_forbidden_provider_run() -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body_with_attachment_track_forbidden_provider_run())
        .await
}

async fn remux_fixture_with_mkvmerge(body: &str) -> RemuxFixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let input = root.join("input.mp4");
    let stage = root.join("stage");
    let output = stage.join("out.mkv");
    tokio::fs::create_dir_all(&stage).await.unwrap();
    tokio::fs::write(&input, b"input").await.unwrap();
    let command = stub_bin(&root, "mkvmerge", body);
    let request = request_for_paths(&input, &stage, &output).await;
    RemuxFixture {
        _temp: temp,
        request,
        config: MkvmergeConfig {
            command,
            provider_version: "mkvmerge v80.0 ('Roundabout') 64-bit".to_owned(),
            timeout: std::time::Duration::from_secs(5),
        },
    }
}

async fn request_for_paths(
    input: &Path,
    staging_root: impl AsRef<Path>,
    output: &Path,
) -> RemuxRequest {
    let staging_root = staging_root.as_ref();
    tokio::fs::create_dir_all(staging_root).await.unwrap();
    let expected = if tokio::fs::try_exists(input).await.unwrap() {
        let observed = observe_file_facts(input).await.unwrap();
        RemuxExpectedFacts {
            size_bytes: observed.size_bytes,
            content_hash: observed.content_hash,
            modified_at: observed.modified_at,
            local_file_key: observed.local_file_key,
        }
    } else {
        RemuxExpectedFacts {
            size_bytes: 1,
            content_hash: "blake3:missing".to_owned(),
            modified_at: None,
            local_file_key: None,
        }
    };
    RemuxRequest {
        input: RemuxInput {
            path: input.to_string_lossy().into_owned(),
            expected,
        },
        output: RemuxOutput {
            staging_root: staging_root.to_string_lossy().into_owned(),
            path: output.to_string_lossy().into_owned(),
            container: "mkv".to_owned(),
            overwrite: false,
        },
        selection: RemuxSelection {
            keep_streams: vec![video_ref("stream-0", 0), audio_ref("stream-1", 1)],
            default_streams: vec![audio_ref("stream-1", 1)],
            clear_default_streams: vec![video_ref("stream-0", 0)],
            track_order: vec![RemuxTrackGroup::Video, RemuxTrackGroup::Audio],
            head_streams: vec![],
            forced_streams: vec![],
            clear_forced_streams: vec![],
        },
    }
}

fn video_ref(snapshot_stream_id: &str, provider_stream_index: u32) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: snapshot_stream_id.to_owned(),
        provider_stream_index,
    }
}

fn audio_ref(snapshot_stream_id: &str, provider_stream_index: u32) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: snapshot_stream_id.to_owned(),
        provider_stream_index,
    }
}

fn subtitle_ref(snapshot_stream_id: &str, provider_stream_index: u32) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: snapshot_stream_id.to_owned(),
        provider_stream_index,
    }
}

fn attachment_ref(snapshot_stream_id: &str, provider_stream_index: u32) -> RemuxStreamRef {
    RemuxStreamRef {
        snapshot_stream_id: snapshot_stream_id.to_owned(),
        provider_stream_index,
    }
}

fn fake_mkvmerge_body(
    output_kept_ids: &[&str],
    output_default_ids: &[&str],
    output_container: &str,
    mutate_input: bool,
) -> String {
    let output_tracks = output_kept_ids
        .iter()
        .enumerate()
        .map(|(index, stream_id)| {
            let default = output_default_ids.contains(stream_id);
            let track_type = if *stream_id == "stream-0" {
                "video"
            } else {
                "audio"
            };
            format!(
                r#"{{"id":{},"type":"{}","properties":{{"default_track":{},"number":{}}}}}"#,
                index + 20,
                track_type,
                default,
                index + 1
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--identify" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    *out.mkv)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"{output_container}"}}}},"tracks":[{output_tracks}]}}
JSON
      ;;
    *)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"MP4"}}}},"tracks":[{{"id":7,"type":"video","properties":{{"number":1}}}},{{"id":12,"type":"audio","properties":{{"number":2}}}}]}}
JSON
      ;;
  esac
  exit 0
fi
last=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--output" ]; then out="$arg"; fi
  last="$arg"
  prev="$arg"
done
printf output > "$out"
{mutate_line}
"#,
        mutate_line = if mutate_input {
            "printf changed >> \"$last\""
        } else {
            ":"
        }
    )
}

#[derive(Debug, Clone, Copy)]
struct InputTrackSpec {
    kind: &'static str,
    language: Option<&'static str>,
}

fn input_track(kind: &'static str) -> InputTrackSpec {
    InputTrackSpec {
        kind,
        language: None,
    }
}

fn input_track_with_language(kind: &'static str, language: &'static str) -> InputTrackSpec {
    InputTrackSpec {
        kind,
        language: Some(language),
    }
}

#[derive(Debug, Clone, Copy)]
struct OutputTrackSpec {
    kind: &'static str,
    default: bool,
    language: Option<&'static str>,
}

fn output_track(kind: &'static str, default: bool) -> OutputTrackSpec {
    OutputTrackSpec {
        kind,
        default,
        language: None,
    }
}

fn output_track_with_language(
    kind: &'static str,
    default: bool,
    language: &'static str,
) -> OutputTrackSpec {
    OutputTrackSpec {
        kind,
        default,
        language: Some(language),
    }
}

fn fake_mkvmerge_body_with_output_specs(
    output_specs: &[OutputTrackSpec],
    output_container: &str,
    mutate_input: bool,
) -> String {
    let output_tracks = output_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let language = language_property(spec.language);
            format!(
                r#"{{"id":{},"type":"{}","properties":{{"default_track":{},"number":{}{language}}}}}"#,
                index + 20,
                spec.kind,
                spec.default,
                index + 1,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fake_mkvmerge_body_from_output_tracks(&output_tracks, output_container, mutate_input)
}

fn fake_mkvmerge_body_with_input_and_output_specs(
    input_specs: &[InputTrackSpec],
    output_specs: &[OutputTrackSpec],
    output_container: &str,
) -> String {
    let input_tracks = input_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let language = language_property(spec.language);
            format!(
                r#"{{"id":{},"type":"{}","properties":{{"number":{}{language}}}}}"#,
                index + 7,
                spec.kind,
                index + 1,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let output_tracks = output_specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let language = language_property(spec.language);
            format!(
                r#"{{"id":{},"type":"{}","properties":{{"default_track":{},"number":{}{language}}}}}"#,
                index + 20,
                spec.kind,
                spec.default,
                index + 1,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    fake_mkvmerge_body_from_input_and_output_tracks(&input_tracks, &output_tracks, output_container)
}

fn language_property(language: Option<&str>) -> String {
    language.map_or_else(String::new, |language| {
        format!(r#","language":"{language}""#)
    })
}

fn fake_mkvmerge_body_from_input_and_output_tracks(
    input_tracks: &str,
    output_tracks: &str,
    output_container: &str,
) -> String {
    format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--identify" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    *out.mkv)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"{output_container}"}}}},"tracks":[{output_tracks}]}}
JSON
      ;;
    *)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"MP4"}}}},"tracks":[{input_tracks}]}}
JSON
      ;;
  esac
  exit 0
fi
last=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--output" ]; then out="$arg"; fi
  last="$arg"
  prev="$arg"
done
printf output > "$out"
"#
    )
}

fn fake_mkvmerge_body_from_output_tracks(
    output_tracks: &str,
    output_container: &str,
    mutate_input: bool,
) -> String {
    format!(
        r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--identify" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    *out.mkv)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"{output_container}"}}}},"tracks":[{output_tracks}]}}
JSON
      ;;
    *)
      cat <<'JSON'
{{"container":{{"properties":{{"container_type":"MP4"}}}},"tracks":[{{"id":7,"type":"video","properties":{{"number":1}}}},{{"id":12,"type":"audio","properties":{{"number":2}}}}]}}
JSON
      ;;
  esac
  exit 0
fi
last=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--output" ]; then out="$arg"; fi
  last="$arg"
  prev="$arg"
done
printf output > "$out"
{mutate_line}
"#,
        mutate_line = if mutate_input {
            "printf changed >> \"$last\""
        } else {
            ":"
        }
    )
}

fn fake_mkvmerge_body_with_two_input_videos_forbidden_provider_run() -> String {
    r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--identify" ]; then
  cat <<'JSON'
{"container":{"properties":{"container_type":"MP4"}},"tracks":[{"id":7,"type":"video","properties":{"number":1}},{"id":8,"type":"video","properties":{"number":2}},{"id":12,"type":"audio","properties":{"number":3}}]}
JSON
  exit 0
fi
printf '%s\n' 'provider run forbidden' >&2
exit 42
"#
    .to_owned()
}

fn fake_mkvmerge_body_with_attachment_track_forbidden_provider_run() -> String {
    r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--identify" ]; then
  cat <<'JSON'
{"container":{"properties":{"container_type":"MP4"}},"tracks":[{"id":7,"type":"video","properties":{"number":1}},{"id":12,"type":"audio","properties":{"number":2}},{"id":99,"type":"attachments","properties":{"number":3}}],"attachments":[{"id":99,"file_name":"cover.jpg"}]}
JSON
  exit 0
fi
printf '%s\n' 'provider run forbidden' >&2
exit 42
"#
    .to_owned()
}

fn fake_mkvmerge_body_with_provider_failure() -> String {
    r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--identify" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    *out.mkv)
      cat <<'JSON'
{"container":{"properties":{"container_type":"mkv"}},"tracks":[{"id":20,"type":"video","properties":{"default_track":false,"number":1}},{"id":21,"type":"audio","properties":{"default_track":true,"number":2}}]}
JSON
      ;;
    *)
      cat <<'JSON'
{"container":{"properties":{"container_type":"MP4"}},"tracks":[{"id":7,"type":"video","properties":{"number":1}},{"id":12,"type":"audio","properties":{"number":2}}]}
JSON
      ;;
  esac
  exit 0
fi
printf '%s\n' 'mkvmerge failed deliberately' >&2
exit 23
"#
    .to_owned()
}

fn fake_mkvmerge_body_with_provider_timeout() -> String {
    r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--identify" ]; then
  last=""
  for arg in "$@"; do last="$arg"; done
  case "$last" in
    *out.mkv)
      cat <<'JSON'
{"container":{"properties":{"container_type":"mkv"}},"tracks":[{"id":20,"type":"video","properties":{"default_track":false,"number":1}},{"id":21,"type":"audio","properties":{"default_track":true,"number":2}}]}
JSON
      ;;
    *)
      cat <<'JSON'
{"container":{"properties":{"container_type":"MP4"}},"tracks":[{"id":7,"type":"video","properties":{"number":1}},{"id":12,"type":"audio","properties":{"number":2}}]}
JSON
      ;;
  esac
  exit 0
fi
sleep 2
"#
    .to_owned()
}

fn handle_operation_with_test_config(
    req: OperationRequest,
    config: MkvmergeConfig,
) -> OperationFuture {
    operation_handler(config)(req)
}

fn dispatch_frames(dispatch: OperationDispatch) -> Vec<ProgressFrame> {
    let body = match dispatch.body {
        voom_worker_protocol::http::OperationBody::Buffered(body) => body,
        other @ voom_worker_protocol::http::OperationBody::Streaming(_) => {
            assert!(
                matches!(
                    other,
                    voom_worker_protocol::http::OperationBody::Buffered(_)
                ),
                "mkvtoolnix worker should buffer test responses"
            );
            Vec::new()
        }
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
        assert!(
            matches!(frame, ProgressFrame::Error { .. }),
            "expected terminal error frame, got {frame:?}"
        );
        return;
    };
    assert_eq!(*actual_class, class);
    assert_eq!(*actual_code, code);
    assert!(!message.trim().is_empty());
    assert!(payload.is_some());
}

fn stub_bin(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    make_executable(&path);
    path
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
