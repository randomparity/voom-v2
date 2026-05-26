use std::path::{Path, PathBuf};

use voom_core::ErrorCode;
use voom_worker_protocol::{
    RemuxExpectedFacts, RemuxInput, RemuxOutput, RemuxRequest, RemuxSelection, RemuxStreamRef,
    RemuxTrackGroup,
};

use crate::observe::observe_file_facts;
use crate::preflight::MkvmergeConfig;

use super::*;

#[tokio::test]
async fn handler_rejects_existing_output() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.mp4");
    let output = temp.path().join("stage").join("out.mkv");
    tokio::fs::create_dir_all(output.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&input, b"not real media").await.unwrap();
    tokio::fs::write(&output, b"stale").await.unwrap();

    let request = request_for_paths(&input, temp.path().join("stage"), &output).await;
    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("output path already exists"));
}

#[tokio::test]
async fn handler_rejects_missing_input_with_artifact_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let request = request_for_paths(
        &temp.path().join("missing.mp4"),
        temp.path().join("stage"),
        &temp.path().join("stage/out.mkv"),
    )
    .await;

    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ArtifactUnavailable);
}

#[tokio::test]
async fn handler_rejects_output_path_escape() {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.mp4");
    tokio::fs::write(&input, b"input").await.unwrap();
    let request = request_for_paths(
        &input,
        temp.path().join("stage"),
        &temp.path().join("out.mkv"),
    )
    .await;

    let err = handle_remux(&request, &MkvmergeConfig::for_tests())
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("escapes staging root"));
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
    };

    let err = handle_remux(&request, &fixture.config).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::ConfigInvalid);
    assert!(err.to_string().contains("at least one video stream"));
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

async fn remux_fixture_with_fake_mkvmerge_that_mutates_input() -> RemuxFixture {
    remux_fixture_with_mkvmerge(&fake_mkvmerge_body(
        &["stream-0", "stream-1"],
        &["stream-1"],
        "mkv",
        true,
    ))
    .await
}

async fn remux_fixture_with_mkvmerge(body: &str) -> RemuxFixture {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.mp4");
    let stage = temp.path().join("stage");
    let output = stage.join("out.mkv");
    tokio::fs::create_dir_all(&stage).await.unwrap();
    tokio::fs::write(&input, b"input").await.unwrap();
    let command = stub_bin(temp.path(), "mkvmerge", body);
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
