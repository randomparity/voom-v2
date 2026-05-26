use super::*;

use std::ffi::{OsStr, OsString};

#[test]
fn default_ffmpeg_worker_command_prefers_current_exe_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");
    let worker = dir.path().join("voom-ffmpeg-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert_eq!(command.env, Vec::<(OsString, OsString)>::new());
}

#[test]
fn default_ffmpeg_worker_command_searches_profile_dir_from_test_deps_dir() {
    let dir = tempfile::tempdir().unwrap();
    let deps_dir = dir.path().join("deps");
    std::fs::create_dir(&deps_dir).unwrap();
    let current_exe = deps_dir.join("transcode_dispatch_test");
    let worker = dir.path().join("voom-ffmpeg-worker");
    std::fs::write(&worker, b"").unwrap();

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, worker.as_os_str());
    assert_eq!(command.env, Vec::<(OsString, OsString)>::new());
}

#[test]
fn default_ffmpeg_worker_command_falls_back_to_path_when_sibling_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let current_exe = dir.path().join("voom");

    let command = bundled_ffmpeg_worker_command_from(None, Ok(current_exe));

    assert_eq!(command.program, OsStr::new("voom-ffmpeg-worker"));
}
