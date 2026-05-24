#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use voom_store::test_support::sqlite_url_for;

#[tokio::test]
async fn scan_file_success_outputs_envelope_and_persists_snapshot() {
    let seeded = seed().await;
    let media = tiny_media_fixture();

    let output = scan_command(&seeded.url, &media).output().unwrap();

    assert_status(&output, Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    redact_common(&mut json);
    redact_paths(&mut json, &[path_redaction(&media, "[media]/tiny.mp4")]);
    redact_content_hashes(&mut json);
    insta::assert_json_snapshot!(
        "scan_file_success_outputs_envelope_and_persists_snapshot",
        json
    );

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "workers", 1).await;
    assert_table_count(&pool, "worker_capabilities", 1).await;
    assert_table_count(&pool, "worker_grants", 1).await;
    assert_table_count(&pool, "file_assets", 1).await;
    assert_table_count(&pool, "file_versions", 1).await;
    assert_table_count(&pool, "file_locations", 1).await;
    assert_table_count(&pool, "media_snapshots", 1).await;
}

#[tokio::test]
async fn scan_directory_reports_unsupported_entries_as_skipped() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = dir.path().join("tiny.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let note = dir.path().join("note.txt");
    std::fs::write(&note, b"not media").unwrap();

    let output = scan_command(&seeded.url, dir.path()).output().unwrap();

    assert_status(&output, Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["summary"]["skipped"], 1);
    redact_common(&mut json);
    redact_paths(
        &mut json,
        &[
            path_redaction(dir.path(), "[scan-dir]"),
            path_redaction(&media, "[scan-dir]/tiny.mp4"),
            path_redaction(&note, "[scan-dir]/note.txt"),
        ],
    );
    redact_content_hashes(&mut json);
    insta::assert_json_snapshot!(
        "scan_directory_reports_unsupported_entries_as_skipped",
        json
    );

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "media_snapshots", 1).await;
}

#[tokio::test]
async fn scan_unsupported_explicit_file_is_bad_args() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let note = dir.path().join("note.txt");
    std::fs::write(&note, b"not media").unwrap();

    let output = scan_command(&seeded.url, &note).output().unwrap();

    assert_status(&output, Some(1));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
    redact_common(&mut json);
    redact_paths(&mut json, &[path_redaction(&note, "[scan-dir]/note.txt")]);
    insta::assert_json_snapshot!("scan_unsupported_explicit_file_is_bad_args", json);

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "workers", 0).await;
    assert_table_count(&pool, "media_snapshots", 0).await;
}

#[tokio::test]
async fn scan_reuses_builtin_ffprobe_worker_row() {
    let seeded = seed().await;
    let media = tiny_media_fixture();

    let first = scan_command(&seeded.url, &media).output().unwrap();
    assert_status(&first, Some(0));
    let second = scan_command(&seeded.url, &media).output().unwrap();

    assert_status(&second, Some(0));
    let mut json = envelope(second.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    redact_common(&mut json);
    redact_paths(&mut json, &[path_redaction(&media, "[media]/tiny.mp4")]);
    redact_content_hashes(&mut json);
    insta::assert_json_snapshot!("scan_reuses_builtin_ffprobe_worker_row", json);

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    let worker_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE name = ?")
        .bind("builtin.ffprobe")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(worker_count, 1);
    let probed_by: Vec<i64> =
        sqlx::query_scalar("SELECT DISTINCT probed_by FROM media_snapshots ORDER BY probed_by")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(probed_by, vec![1]);
    assert_table_count(&pool, "media_snapshots", 2).await;
}

#[tokio::test]
async fn scan_content_drift_fails_without_snapshot() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = dir.path().join("drift.mp4");
    std::fs::write(&media, b"media before probe").unwrap();
    let fake_ffprobe = write_drifting_ffprobe(dir.path());

    let output = scan_command(&seeded.url, &media)
        .env("VOOM_FFPROBE_BIN", &fake_ffprobe)
        .output()
        .unwrap();

    assert_status(&output, Some(2));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "ARTIFACT_CHECKSUM_MISMATCH");
    assert_eq!(
        json["data"]["files"][0]["error"]["failure_class"],
        "artifact_checksum_mismatch"
    );
    redact_common(&mut json);
    redact_paths(
        &mut json,
        &[
            path_redaction(&media, "[scan-dir]/drift.mp4"),
            path_redaction(&fake_ffprobe, "[scan-dir]/ffprobe"),
        ],
    );
    redact_content_hashes(&mut json);
    insta::assert_json_snapshot!("scan_content_drift_fails_without_snapshot", json);

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "media_snapshots", 0).await;
}

struct Seeded {
    _tmp: NamedTempFile,
    url: String,
}

async fn seed() -> Seeded {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    Seeded { _tmp: tmp, url }
}

fn scan_command(url: &str, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .args(["--database-url", url, "scan", "--path"])
        .arg(path)
        .env("VOOM_FFPROBE_WORKER_BIN", built_worker_binary());
    command
}

fn built_worker_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let mut command = Command::new("cargo");
        command.args([
            "build",
            "-p",
            "voom-ffprobe-worker",
            "--bin",
            "voom-ffprobe-worker",
        ]);
        if let Some(target) = cargo_build_target() {
            command.args(["--target", &target]);
        }
        let status = command.current_dir(workspace_root()).status().unwrap();
        assert!(status.success(), "cargo build for ffprobe worker failed");

        let binary = target_debug_dir().join(format!(
            "voom-ffprobe-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert!(
            binary.is_file(),
            "built ffprobe worker binary not found at {}",
            binary.display()
        );
        binary
    })
}

fn target_debug_dir() -> PathBuf {
    let debug_dir = if let Some(target_dir) = explicit_target_dir() {
        target_dir
    } else {
        current_exe_target_dir()
    };

    if let Some(target) = cargo_build_target() {
        return debug_dir.join(target).join("debug");
    }
    debug_dir.join("debug")
}

fn explicit_target_dir() -> Option<PathBuf> {
    if let Some(target_dir) = std::env::var_os("CARGO_TARGET_DIR") {
        let target_dir = PathBuf::from(target_dir);
        return Some(if target_dir.is_absolute() {
            target_dir
        } else {
            workspace_root().join(target_dir)
        });
    }
    None
}

fn current_exe_target_dir() -> PathBuf {
    let current_exe = std::env::current_exe().unwrap();
    let deps_dir = current_exe.parent().unwrap();
    if deps_dir.file_name().is_some_and(|name| name == "deps") {
        let profile_dir = deps_dir.parent().unwrap();
        if cargo_build_target().is_some() {
            return profile_dir.parent().and_then(Path::parent).map_or_else(
                || profile_dir.parent().unwrap().to_path_buf(),
                Path::to_path_buf,
            );
        }
        return profile_dir.parent().unwrap().to_path_buf();
    }
    deps_dir
        .parent()
        .map_or_else(|| deps_dir.to_path_buf(), Path::to_path_buf)
}

fn cargo_build_target() -> Option<String> {
    std::env::var("CARGO_BUILD_TARGET")
        .ok()
        .filter(|target| !target.is_empty())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn tiny_media_fixture() -> PathBuf {
    workspace_root()
        .join("crates/voom-ffprobe-worker/fixtures/media/tiny.mp4")
        .canonicalize()
        .unwrap()
}

fn write_drifting_ffprobe(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join("ffprobe");
    std::fs::write(
        &path,
        "#!/usr/bin/env sh\n\
         set -eu\n\
         last=''\n\
         for arg in \"$@\"; do last=\"$arg\"; done\n\
         printf drift >> \"$last\"\n\
         printf '{\"format\":{\"format_name\":\"mov,mp4\",\"duration\":\"1.0\",\"bit_rate\":\"1\"},\"streams\":[]}\\n'\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

async fn assert_table_count(pool: &sqlx::SqlitePool, table: &str, expected: i64) {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let count: i64 = sqlx::query_scalar(&sql).fetch_one(pool).await.unwrap();
    assert_eq!(count, expected, "unexpected row count for {table}");
}

fn assert_status(output: &Output, expected: Option<i32>) {
    assert_eq!(
        output.status.code(),
        expected,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

fn redact_common(json: &mut Value) {
    json["local"]["db_url"] = Value::String("[db-url]".to_owned());
    json["local"]["config_path"] = Value::String("[config-path]".to_owned());
}

fn path_redaction(path: &Path, replacement: &str) -> (String, String) {
    (path.display().to_string(), replacement.to_owned())
}

fn redact_paths(value: &mut Value, replacements: &[(String, String)]) {
    match value {
        Value::String(text) => {
            for (needle, replacement) in replacements {
                *text = text.replace(needle, replacement);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_paths(item, replacements);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_paths(item, replacements);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_content_hashes(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                redact_content_hashes(item);
            }
        }
        Value::Object(map) => {
            if map.get("content_hash").is_some_and(Value::is_string) {
                map.insert(
                    "content_hash".to_owned(),
                    Value::String("[content-hash]".to_owned()),
                );
            }
            for item in map.values_mut() {
                redact_content_hashes(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}
