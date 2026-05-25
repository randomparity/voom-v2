#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use voom_store::test_support::sqlite_url_for;

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn artifact_full_flow_outputs_committed_envelopes() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = tiny_media_fixture();
    let staging = dir.path().join("staged.mp4");
    let target = dir.path().join("committed.mp4");

    let scan = run(&mut scan_command(&seeded.url, &media), Some(0));
    let scanned = scan["data"]["files"][0].clone();
    let file_version_id = id(&scanned["file_version_id"]);
    let file_location_id = id(&scanned["file_location_id"]);

    let stage = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "stage-copy",
                "--file-version-id",
                &file_version_id.to_string(),
                "--source-location-id",
                &file_location_id.to_string(),
                "--staging-path",
            ])
            .arg(&staging),
        Some(0),
    );
    let artifact_handle_id = id(&stage["data"]["artifact"]["artifact_handle_id"]);
    let verify = run(
        artifact_command(&seeded.url).args([
            "artifact",
            "verify",
            "--artifact-handle-id",
            &artifact_handle_id.to_string(),
        ]),
        Some(0),
    );
    let commit = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target),
        Some(0),
    );
    let show = run(
        artifact_command(&seeded.url).args([
            "artifact",
            "show",
            "--artifact-handle-id",
            &artifact_handle_id.to_string(),
        ]),
        Some(0),
    );

    assert_eq!(show["data"]["artifact"]["state"], "committed");
    assert!(target.is_file());
    let mut json = Value::Array(vec![scan, stage, verify, commit, show]);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &[
            (media.as_path(), "[media]/tiny.mp4"),
            (dir.path(), "[artifact-dir]"),
            (staging.as_path(), "[artifact-dir]/staged.mp4"),
            (target.as_path(), "[artifact-dir]/committed.mp4"),
        ],
    );
    insta::assert_json_snapshot!("artifact_full_flow_outputs_committed_envelopes", json);
}

#[tokio::test]
async fn artifact_list_and_show_cover_all_inspection_states() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let staged = create_staged_artifact(&seeded.url, dir.path(), "staged");
    let verified = create_verified_artifact(&seeded.url, dir.path(), "verified");
    let committed = create_committed_artifact(&seeded.url, dir.path(), "committed");
    let failed = create_failed_artifact(&seeded.url, dir.path(), "failed");
    let recovery = create_verified_artifact(&seeded.url, dir.path(), "recovery");
    inject_recovery_required(
        &seeded.url,
        recovery.artifact_handle_id,
        recovery.verification_id.unwrap(),
        dir.path(),
    )
    .await;

    let mut envelopes = Vec::new();
    for state in [
        "staged",
        "verified",
        "committed",
        "failed",
        "recovery_required",
    ] {
        let list = run(
            artifact_command(&seeded.url).args(["artifact", "list", "--state", state]),
            Some(0),
        );
        assert_eq!(list["data"]["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(list["data"]["artifacts"][0]["state"], state);
        envelopes.push(list);
    }
    for artifact in [&staged, &verified, &committed, &failed, &recovery] {
        envelopes.push(run(
            artifact_command(&seeded.url).args([
                "artifact",
                "show",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
            ]),
            Some(0),
        ));
    }

    assert!(envelopes.iter().any(|json| {
        json["data"]["artifact"]["latest_verification"]["status"] == "failed"
            && json["data"]["artifact"]["latest_verification"]["id"].is_number()
    }));
    let recovery_show = envelopes.last().unwrap();
    let recovery_commit = &recovery_show["data"]["artifact"]["latest_commit"];
    assert_eq!(recovery_commit["state"], "recovery_required");
    assert!(recovery_commit["id"].is_number());
    assert!(recovery_commit["target_path"].is_string());
    assert!(recovery_commit["temp_path"].is_string());
    assert!(recovery_commit["recovery"]["target"]["exists"].is_boolean());
    assert!(recovery_commit["recovery"]["temp"]["exists"].is_boolean());
    assert!(recovery_commit["recovery"]["staging"]["exists"].is_boolean());

    let mut json = Value::Array(envelopes);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &path_redaction_inputs(
            dir.path(),
            &[
                (&staged, "staged"),
                (&verified, "verified"),
                (&committed, "committed"),
                (&failed, "failed"),
                (&recovery, "recovery"),
            ],
        ),
    );
    insta::assert_json_snapshot!("artifact_list_and_show_cover_all_inspection_states", json);
}

#[tokio::test]
async fn artifact_failure_envelopes_are_actionable() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let unverified = create_staged_artifact(&seeded.url, dir.path(), "unverified");
    let drift = create_verified_artifact(&seeded.url, dir.path(), "drift");
    std::fs::write(&drift.staging_path, b"changed bytes").unwrap();
    let existing_target = create_verified_artifact(&seeded.url, dir.path(), "existing");
    let existing_target_path = dir.path().join("already-exists.mp4");
    std::fs::write(&existing_target_path, b"already here").unwrap();
    let failed = create_failed_artifact(&seeded.url, dir.path(), "verify-failed");
    let recovery_failure = create_verified_artifact(&seeded.url, dir.path(), "recovery-failure");
    let recovery_target = dir.path().join(format!("{}.mp4", "x".repeat(240)));

    let missing = run(
        artifact_command(&seeded.url).args(["artifact", "show", "--artifact-handle-id", "999999"]),
        Some(2),
    );
    let failed_verification = run(
        artifact_command(&seeded.url).args([
            "artifact",
            "show",
            "--artifact-handle-id",
            &failed.artifact_handle_id.to_string(),
        ]),
        Some(0),
    );
    let unverified_commit = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &unverified.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(dir.path().join("unverified-target.mp4")),
        Some(2),
    );
    let drift_commit = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &drift.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(dir.path().join("drift-target.mp4")),
        Some(2),
    );
    let target_exists = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &existing_target.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&existing_target_path),
        Some(2),
    );
    let recovery_required = run(
        artifact_command(&seeded.url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &recovery_failure.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&recovery_target),
        Some(2),
    );

    assert!(failed_verification["data"]["artifact"]["latest_verification"]["id"].is_number());
    assert_eq!(
        failed_verification["data"]["artifact"]["latest_verification"]["status"],
        "failed"
    );
    assert_eq!(missing["error"]["code"], "NOT_FOUND");
    assert_eq!(unverified_commit["error"]["code"], "CONFIG_INVALID");
    assert_eq!(drift_commit["error"]["code"], "ARTIFACT_CHECKSUM_MISMATCH");
    assert_eq!(target_exists["error"]["code"], "CONFIG_INVALID");
    assert_eq!(recovery_required["error"]["code"], "COMMIT_FAILURE");
    assert!(recovery_required["data"]["artifact"]["commit_record_id"].is_number());
    assert!(recovery_required["data"]["artifact"]["target_path"].is_string());
    assert!(recovery_required["data"]["artifact"]["temp_path"].is_string());
    assert!(
        recovery_required["data"]["artifact"]["recovery_required"]["target_exists"].is_boolean()
    );
    assert!(recovery_required["data"]["artifact"]["recovery_required"]["temp_exists"].is_boolean());
    assert!(
        recovery_required["data"]["artifact"]["recovery_required"]["staging_exists"].is_boolean()
    );

    let mut json = Value::Array(vec![
        missing,
        failed_verification,
        unverified_commit,
        drift_commit,
        target_exists,
        recovery_required,
    ]);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &path_redaction_inputs(
            dir.path(),
            &[
                (&unverified, "unverified"),
                (&drift, "drift"),
                (&existing_target, "existing"),
                (&failed, "verify-failed"),
                (&recovery_failure, "recovery-failure"),
            ],
        ),
    );
    redact_path_set(
        &mut json,
        &[(
            existing_target_path.as_path(),
            "[artifact-dir]/already-exists.mp4",
        )],
    );
    redact_path_set(
        &mut json,
        &[(recovery_target.as_path(), "[artifact-dir]/long-target.mp4")],
    );
    redact_long_target_names(&mut json);
    insta::assert_json_snapshot!("artifact_failure_envelopes_are_actionable", json);
}

#[derive(Debug)]
struct Seeded {
    _tmp: NamedTempFile,
    url: String,
}

#[derive(Debug)]
struct ArtifactFixture {
    artifact_handle_id: u64,
    staging_path: PathBuf,
    target_path: Option<PathBuf>,
    verification_id: Option<u64>,
}

async fn seed() -> Seeded {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    Seeded { _tmp: tmp, url }
}

fn create_staged_artifact(url: &str, dir: &Path, name: &str) -> ArtifactFixture {
    let media = tiny_media_fixture();
    let scan = run(&mut scan_command(url, &media), Some(0));
    let file_version_id = id(&scan["data"]["files"][0]["file_version_id"]);
    let file_location_id = id(&scan["data"]["files"][0]["file_location_id"]);
    let staging_path = dir.join(format!("{name}-staged.mp4"));
    let stage = run(
        artifact_command(url)
            .args([
                "artifact",
                "stage-copy",
                "--file-version-id",
                &file_version_id.to_string(),
                "--source-location-id",
                &file_location_id.to_string(),
                "--staging-path",
            ])
            .arg(&staging_path),
        Some(0),
    );

    ArtifactFixture {
        artifact_handle_id: id(&stage["data"]["artifact"]["artifact_handle_id"]),
        staging_path,
        target_path: None,
        verification_id: None,
    }
}

fn create_verified_artifact(url: &str, dir: &Path, name: &str) -> ArtifactFixture {
    let mut artifact = create_staged_artifact(url, dir, name);
    let verify = run(
        artifact_command(url).args([
            "artifact",
            "verify",
            "--artifact-handle-id",
            &artifact.artifact_handle_id.to_string(),
        ]),
        Some(0),
    );
    assert_eq!(verify["data"]["artifact"]["status"], "succeeded");
    artifact.verification_id = Some(id(&verify["data"]["artifact"]["verification_id"]));
    artifact
}

fn create_committed_artifact(url: &str, dir: &Path, name: &str) -> ArtifactFixture {
    let mut artifact = create_verified_artifact(url, dir, name);
    let target_path = dir.join(format!("{name}-committed.mp4"));
    let commit = run(
        artifact_command(url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target_path),
        Some(0),
    );
    assert_eq!(commit["data"]["artifact"]["state"], "committed");
    artifact.target_path = Some(target_path);
    artifact
}

fn create_failed_artifact(url: &str, dir: &Path, name: &str) -> ArtifactFixture {
    let mut artifact = create_staged_artifact(url, dir, name);
    std::fs::write(&artifact.staging_path, b"changed bytes").unwrap();
    let verify = run(
        artifact_command(url).args([
            "artifact",
            "verify",
            "--artifact-handle-id",
            &artifact.artifact_handle_id.to_string(),
        ]),
        Some(0),
    );
    assert_eq!(verify["data"]["artifact"]["status"], "failed");
    artifact.verification_id = Some(id(&verify["data"]["artifact"]["verification_id"]));
    artifact
}

async fn inject_recovery_required(
    url: &str,
    artifact_handle_id: u64,
    verification_id: u64,
    dir: &Path,
) {
    let pool = voom_store::connect(url).await.unwrap();
    let source_file_version_id: i64 =
        sqlx::query_scalar("SELECT file_version_id FROM artifact_handles WHERE id = ?")
            .bind(i64::try_from(artifact_handle_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let target_path = dir.join("recovery-target.mp4");
    let temp_path = dir.join("recovery-target.mp4.voom.tmp");
    std::fs::write(&target_path, b"promoted bytes").unwrap();
    std::fs::write(&temp_path, b"temp bytes").unwrap();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, failure_class, error_code, \
          message, recovery_reason, temp_path, report, started_at, promotion_started_at, finished_at) \
         VALUES (?, ?, ?, ?, NULL, NULL, 'recovery_required', 'database_unavailable', \
          'DB_UNREACHABLE', 'injected recovery for CLI inspection', 'promotion_started', ?, \
          '{\"test\":true}', '2026-05-25T00:00:00Z', '2026-05-25T00:00:01Z', '2026-05-25T00:00:02Z')",
    )
    .bind(i64::try_from(artifact_handle_id).unwrap())
    .bind(source_file_version_id)
    .bind(i64::try_from(verification_id).unwrap())
    .bind(target_path.display().to_string())
    .bind(temp_path.display().to_string())
    .execute(&pool)
    .await
    .unwrap();
}

fn scan_command(url: &str, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .args(["--database-url", url, "scan", "--path"])
        .arg(path)
        .env("VOOM_FFPROBE_WORKER_BIN", built_ffprobe_worker_binary())
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary());
    command
}

fn artifact_command(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .arg("--database-url")
        .arg(url)
        .env(
            "VOOM_VERIFY_ARTIFACT_WORKER_BIN",
            built_verify_worker_binary(),
        )
        .env("VOOM_FFPROBE_WORKER_BIN", built_ffprobe_worker_binary())
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary());
    command
}

fn built_ffprobe_worker_binary() -> &'static PathBuf {
    built_worker_binary("voom-ffprobe-worker")
}

fn built_verify_worker_binary() -> &'static PathBuf {
    built_worker_binary("voom-verify-artifact-worker")
}

fn built_worker_binary(package: &'static str) -> &'static PathBuf {
    static FFPROBE: OnceLock<PathBuf> = OnceLock::new();
    static VERIFY: OnceLock<PathBuf> = OnceLock::new();
    let cell = if package == "voom-ffprobe-worker" {
        &FFPROBE
    } else {
        &VERIFY
    };
    cell.get_or_init(|| {
        let mut command = Command::new("cargo");
        command.args(["build", "-p", package, "--bin", package]);
        if let Some(target) = cargo_build_target() {
            command.args(["--target", &target]);
        }
        let status = command.current_dir(workspace_root()).status().unwrap();
        assert!(status.success(), "cargo build for {package} failed");
        let binary = target_debug_dir().join(format!("{package}{}", std::env::consts::EXE_SUFFIX));
        assert!(
            binary.is_file(),
            "worker binary not found at {}",
            binary.display()
        );
        binary
    })
}

fn target_debug_dir() -> PathBuf {
    let debug_dir = explicit_target_dir().unwrap_or_else(current_exe_target_dir);
    if let Some(target) = cargo_build_target() {
        return debug_dir.join(target).join("debug");
    }
    debug_dir.join("debug")
}

fn explicit_target_dir() -> Option<PathBuf> {
    std::env::var_os("CARGO_TARGET_DIR").map(|target_dir| {
        let target_dir = PathBuf::from(target_dir);
        if target_dir.is_absolute() {
            target_dir
        } else {
            workspace_root().join(target_dir)
        }
    })
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

fn success_ffprobe_binary() -> &'static PathBuf {
    static BIN: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    &BIN.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let path = write_success_ffprobe(dir.path());
        (dir, path)
    })
    .1
}

fn write_success_ffprobe(dir: &Path) -> PathBuf {
    let script = format!(
        "#!/usr/bin/env sh\n\
         set -eu\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         cat <<'JSON'\n\
         {BASIC_FFPROBE_JSON}\n\
         JSON\n"
    );
    write_executable(dir, "ffprobe", &script)
}

fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn run(command: &mut Command, expected: Option<i32>) -> Value {
    let output = command.output().unwrap();
    assert_status(&output, expected);
    envelope(output.stdout)
}

fn assert_status(output: &Output, expected: Option<i32>) {
    assert_eq!(
        output.status.code(),
        expected,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

fn id(value: &Value) -> u64 {
    value.as_u64().unwrap()
}

fn redact_artifact_snapshot(json: &mut Value, db_url: &str, paths: &[(&Path, &str)]) {
    redact_common(json, db_url);
    redact_path_set(json, paths);
    redact_temp_path_names(json);
    redact_hashes(json);
    redact_worker_ids(json);
    redact_local_file_keys(json);
}

fn redact_common(json: &mut Value, db_url: &str) {
    replace_string(json, db_url, "[db-url]");
    replace_key_value(
        json,
        "config_path",
        &Value::String("[config-path]".to_owned()),
    );
}

fn redact_path_set(value: &mut Value, paths: &[(&Path, &str)]) {
    let mut replacements = paths
        .iter()
        .flat_map(|(path, replacement)| path_redactions(path, replacement))
        .collect::<Vec<_>>();
    replacements.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));
    for (needle, replacement) in replacements {
        replace_string(value, &needle, &replacement);
    }
}

fn path_redaction_inputs<'a>(
    dir: &'a Path,
    _artifacts: &[(&'a ArtifactFixture, &'a str)],
) -> Vec<(&'a Path, &'a str)> {
    vec![(dir, "[artifact-dir]")]
}

fn path_redactions(path: &Path, replacement: &str) -> Vec<(String, String)> {
    let replacement = replacement.to_owned();
    let mut redactions = vec![(path.display().to_string(), replacement.clone())];
    if let Ok(canonical) = path.canonicalize() {
        let canonical = canonical.display().to_string();
        if redactions.iter().all(|(needle, _)| needle != &canonical) {
            redactions.push((canonical, replacement));
        }
    }
    redactions
}

fn redact_hashes(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                redact_hashes(item);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "content_hash" | "checksum" | "expected_checksum" | "observed_checksum"
                ) && item.is_string()
                {
                    *item = Value::String("[hash]".to_owned());
                } else {
                    redact_hashes(item);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn redact_temp_path_names(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Some(start) = text.find(".voom-tmp.") {
                let prefix = &text[..start];
                let suffix = &text[start..];
                let mut parts = suffix.rsplitn(3, '.').collect::<Vec<_>>();
                if parts.len() == 3
                    && parts[0].chars().all(|c| c.is_ascii_digit())
                    && parts[1].chars().all(|c| c.is_ascii_digit())
                {
                    parts.reverse();
                    *text = format!("{prefix}{}.[temp]", parts[0]);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_temp_path_names(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_temp_path_names(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_long_target_names(value: &mut Value) {
    let long_name = "x".repeat(240);
    replace_string(
        value,
        &format!("[artifact-dir]/{long_name}.mp4"),
        "[artifact-dir]/long-target.mp4",
    );
    replace_string(
        value,
        &format!("[artifact-dir]/.voom-tmp.{long_name}.mp4.[temp]"),
        "[artifact-dir]/.voom-tmp.long-target.mp4.[temp]",
    );
}

fn redact_worker_ids(value: &mut Value) {
    replace_key_value(value, "worker_id", &Value::String("[worker-id]".to_owned()));
    replace_key_value(
        value,
        "probe_worker_id",
        &Value::String("[worker-id]".to_owned()),
    );
}

fn redact_local_file_keys(value: &mut Value) {
    replace_key_value(
        value,
        "local_file_key",
        &Value::String("[local-file-key]".to_owned()),
    );
}

fn replace_key_value(value: &mut Value, key: &str, replacement: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                replace_key_value(item, key, replacement);
            }
        }
        Value::Object(map) => {
            for (item_key, item) in map {
                if item_key == key {
                    *item = replacement.clone();
                } else {
                    replace_key_value(item, key, replacement);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn replace_string(value: &mut Value, needle: &str, replacement: &str) {
    match value {
        Value::String(text) => *text = text.replace(needle, replacement),
        Value::Array(items) => {
            for item in items {
                replace_string(item, needle, replacement);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                replace_string(item, needle, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
