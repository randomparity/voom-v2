use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use voom_core::{ErrorCode, FailureClass};

use super::*;

#[tokio::test]
async fn nonexistent_explicit_ffprobe_path_maps_to_external_system_unavailable() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = dir.path().join("clip.bin");
    let write_result = std::fs::write(&media_path, b"not media");
    assert!(write_result.is_ok());
    let config = FfprobeConfig::from_env_pairs([(
        FFPROBE_BIN_ENV,
        dir.path().join("does-not-exist").as_os_str(),
    )]);

    let result = run_ffprobe_json(&media_path, &config).await;

    assert!(matches!(
        result.as_ref().map_err(FfprobeError::failure_class),
        Err(FailureClass::ExternalSystemUnavailable)
    ));
    assert!(matches!(
        result.as_ref().map_err(FfprobeError::error_code),
        Err(ErrorCode::ExternalSystemUnavailable)
    ));
}

#[tokio::test]
async fn helper_process_invalid_json_maps_to_malformed_worker_result() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let fake_ffprobe = write_fake_ffprobe(dir.path(), "printf 'not-json\\n'\nexit 0\n");
    let media_path = dir.path().join("clip.bin");
    let write_result = std::fs::write(&media_path, b"not media");
    assert!(write_result.is_ok());
    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    let result = run_ffprobe_json(&media_path, &config).await;

    assert!(matches!(
        result.as_ref().map_err(FfprobeError::failure_class),
        Err(FailureClass::MalformedWorkerResult)
    ));
    assert!(matches!(
        result.as_ref().map_err(FfprobeError::error_code),
        Err(ErrorCode::MalformedWorkerResult)
    ));
}

#[tokio::test]
async fn ffprobe_config_captures_provider_version_from_helper() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let fake_ffprobe = write_fake_ffprobe(
        dir.path(),
        "printf '{\"format\":{\"format_name\":\"mov,mp4\"},\"streams\":[]}\\n'\n",
    );

    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    assert_eq!(config.provider_version(), "test-helper");
}

#[test]
fn ffprobe_config_version_probe_times_out_quickly() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let fake_ffprobe = write_executable(
        dir.path(),
        "#!/bin/sh\n\
         if [ \"${1:-}\" = '-version' ]; then exec sleep 5; fi\n\
         printf '{\"format\":{\"format_name\":\"mov,mp4\"},\"streams\":[]}\\n'\n",
    );
    let started = std::time::Instant::now();

    let config = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(config.provider_version(), "unknown");
}

fn write_fake_ffprobe(dir: &Path, body: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         {body}"
    );
    write_executable(dir, &script)
}

fn write_executable(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("ffprobe");
    let write_result = std::fs::write(&path, script);
    assert!(write_result.is_ok());
    let metadata_result = std::fs::metadata(&path);
    assert!(metadata_result.is_ok());
    let Ok(metadata) = metadata_result else {
        return path;
    };
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    let chmod_result = std::fs::set_permissions(&path, permissions);
    assert!(chmod_result.is_ok());
    path
}
