#![expect(
    clippy::expect_used,
    reason = "unit tests use expect for direct thread and process assertions"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use voom_core::{ErrorCode, FailureClass};

use super::*;

#[tokio::test]
async fn ffprobe_removed_after_config_maps_to_external_system_unavailable() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let media_path = dir.path().join("clip.bin");
    let write_result = std::fs::write(&media_path, b"not media");
    assert!(write_result.is_ok());
    let fake_ffprobe = write_fake_ffprobe(dir.path(), "exit 0\n");
    let Some(config) = configured_ffprobe(&fake_ffprobe) else {
        return;
    };
    assert!(std::fs::remove_file(fake_ffprobe).is_ok());

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
    let Some(config) = configured_ffprobe(&fake_ffprobe) else {
        return;
    };

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

    let Some(config) = configured_ffprobe(&fake_ffprobe) else {
        return;
    };

    assert_eq!(config.provider_version(), "test-helper");
}

#[test]
fn ffprobe_config_rejects_missing_dependency() {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };

    let result = FfprobeConfig::from_env_pairs([(
        FFPROBE_BIN_ENV,
        dir.path().join("missing-ffprobe").as_os_str(),
    )]);

    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };
    assert_eq!(config_error_kind(&error), ConfigErrorKind::Io);
    let FfprobeConfigError::Io {
        binary: _,
        operation,
        source: _,
    } = error
    else {
        return;
    };
    assert_eq!(operation, "start version check");
}

#[test]
fn ffprobe_config_timeout_kills_and_reaps_version_child() {
    let dir_result = tempfile::tempdir();
    assert!(dir_result.is_ok());
    let Ok(dir) = dir_result else {
        return;
    };
    let pid_file = dir.path().join("ffprobe.pid");
    let fake_ffprobe = write_executable(
        dir.path(),
        &format!(
            "#!/bin/sh\n\
             if [ \"${{1:-}}\" = '-version' ]; then \
             printf '%s' $$ > '{}'; exec sleep 60; fi\n\
             exit 1\n",
            pid_file.display()
        ),
    );
    let short_timeout = Duration::from_secs(2);
    let (pid, result) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| {
            FfprobeConfig::from_bin_with_version_timeout(
                fake_ffprobe.into_os_string(),
                short_timeout,
            )
        });
        let pid = wait_for_pid_file(&pid_file, Duration::from_secs(5));
        let result = probe.join().expect("version probe thread should not panic");
        (pid, result)
    });

    let error = result.expect_err("hung version probe should time out");
    assert!(error.to_string().contains("version check exceeded"));
    assert_eq!(config_error_kind(&error), ConfigErrorKind::Timeout);
    let FfprobeConfigError::Timeout { binary: _, timeout } = error else {
        return;
    };
    assert_eq!(timeout, short_timeout);
    assert_process_exited(pid.trim());
}

#[test]
fn ffprobe_config_rejects_nonzero_version_probe() {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let fake_ffprobe = write_executable(
        dir.path(),
        "#!/bin/sh\necho 'dependency unavailable' 1>&2\nexit 42\n",
    );

    let result = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };
    assert_eq!(config_error_kind(&error), ConfigErrorKind::Exit);
    let FfprobeConfigError::Exit {
        binary: _,
        status,
        stderr,
    } = error
    else {
        return;
    };
    assert_eq!(status.code(), Some(42));
    assert_eq!(stderr, "dependency unavailable");
}

#[test]
fn ffprobe_config_rejects_malformed_version_output() {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let fake_ffprobe = write_executable(
        dir.path(),
        "#!/bin/sh\nprintf 'unexpected version output\\n'\nexit 0\n",
    );

    let result = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, fake_ffprobe.as_os_str())]);

    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };
    assert_eq!(config_error_kind(&error), ConfigErrorKind::MalformedVersion);
    let FfprobeConfigError::MalformedVersion { binary: _, output } = error else {
        return;
    };
    assert_eq!(output, "unexpected version output\n");
}

#[test]
fn malformed_media_stderr_matches_structural_faults_only() {
    assert!(is_malformed_media_stderr(
        "clip.mkv: Invalid data found when processing input"
    ));
    assert!(is_malformed_media_stderr("moov atom not found"));
    assert!(is_malformed_media_stderr("Error opening input file"));
    // Transient / build-dependent diagnostics must NOT be treated as permanent.
    assert!(!is_malformed_media_stderr("clip.mkv: End of file"));
    assert!(!is_malformed_media_stderr("partial file"));
    assert!(!is_malformed_media_stderr("Unknown format"));
    assert!(!is_malformed_media_stderr(
        "Could not find codec parameters"
    ));
    assert!(!is_malformed_media_stderr(""));
}

#[tokio::test]
async fn nonzero_exit_with_malformed_signature_maps_to_malformed_media() {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let media_path = dir.path().join("clip.mkv");
    assert!(std::fs::write(&media_path, b"garbage").is_ok());
    // A real-shaped ffprobe failure: non-zero exit, structural-fault stderr.
    let ffprobe = write_fake_ffprobe(
        dir.path(),
        "echo 'clip.mkv: Invalid data found when processing input' 1>&2\nexit 1\n",
    );
    let Some(config) = configured_ffprobe(&ffprobe) else {
        return;
    };

    let result = run_ffprobe_json(&media_path, &config).await;

    assert!(
        matches!(
            result.as_ref().map_err(FfprobeError::failure_class),
            Err(FailureClass::MalformedMedia)
        ),
        "expected MalformedMedia, got {result:?}"
    );
    assert!(matches!(
        result.as_ref().map_err(FfprobeError::error_code),
        Err(ErrorCode::MalformedMedia)
    ));
}

#[tokio::test]
async fn nonzero_exit_without_signature_stays_external_system_unavailable() {
    let Ok(dir) = tempfile::tempdir() else {
        return;
    };
    let media_path = dir.path().join("clip.mkv");
    assert!(std::fs::write(&media_path, b"garbage").is_ok());
    // A transient-looking failure (no structural-fault signature) stays retriable.
    let ffprobe = write_fake_ffprobe(dir.path(), "echo 'clip.mkv: End of file' 1>&2\nexit 1\n");
    let Some(config) = configured_ffprobe(&ffprobe) else {
        return;
    };

    let result = run_ffprobe_json(&media_path, &config).await;

    assert!(
        matches!(
            result.as_ref().map_err(FfprobeError::failure_class),
            Err(FailureClass::ExternalSystemUnavailable)
        ),
        "expected ExternalSystemUnavailable, got {result:?}"
    );
}

fn write_fake_ffprobe(dir: &Path, body: &str) -> PathBuf {
    let script = format!(
        "#!/bin/sh\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         {body}"
    );
    write_executable(dir, &script)
}

fn configured_ffprobe(path: &Path) -> Option<FfprobeConfig> {
    let result = FfprobeConfig::from_env_pairs([(FFPROBE_BIN_ENV, path.as_os_str())]);
    assert!(result.is_ok());
    result.ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigErrorKind {
    Io,
    Timeout,
    Exit,
    MalformedVersion,
}

const fn config_error_kind(error: &FfprobeConfigError) -> ConfigErrorKind {
    match error {
        FfprobeConfigError::Io {
            binary: _,
            operation: _,
            source: _,
        } => ConfigErrorKind::Io,
        FfprobeConfigError::Timeout {
            binary: _,
            timeout: _,
        } => ConfigErrorKind::Timeout,
        FfprobeConfigError::Exit {
            binary: _,
            status: _,
            stderr: _,
        } => ConfigErrorKind::Exit,
        FfprobeConfigError::MalformedVersion {
            binary: _,
            output: _,
        } => ConfigErrorKind::MalformedVersion,
    }
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

fn wait_for_pid_file(path: &Path, timeout: Duration) -> String {
    let started = Instant::now();
    loop {
        if let Ok(pid) = std::fs::read_to_string(path) {
            return pid;
        }
        assert!(
            started.elapsed() < timeout,
            "ffprobe helper did not create PID file {}",
            path.display()
        );
        std::thread::yield_now();
    }
}

fn assert_process_exited(pid: &str) {
    let status = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid)
        .stderr(Stdio::null())
        .status()
        .expect("process existence check should launch");
    assert!(!status.success(), "child process {pid} still exists");
}
