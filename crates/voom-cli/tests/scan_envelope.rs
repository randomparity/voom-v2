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
use voom_policy::load_policy_fixture;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::worker::cargo_bin_or_build;

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn scan_file_success_outputs_envelope_and_persists_snapshot() {
    let seeded = seed().await;
    let media = tiny_media_fixture();

    let output = scan_command(&seeded.url, &media).output().unwrap();

    assert_status(&output, Some(0));
    let mut json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
    assert!(
        json["warnings"][0]
            .as_str()
            .unwrap()
            .contains("VOOM_FFPROBE_BIN is set; scan ffprobe binary: ")
    );
    redact_common(&mut json);
    redact_path_set(&mut json, &[(media.as_path(), "[media]/tiny.mp4")]);
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
async fn scan_file_success_finds_worker_beside_cli_without_worker_env() {
    let seeded = seed().await;
    let media = tiny_media_fixture();

    let output = scan_command_without_worker_env(&seeded.url, &media)
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["summary"]["ingested"], 1);
    assert_eq!(json["data"]["summary"]["probed"], 1);
    assert_eq!(json["data"]["summary"]["snapshots_recorded"], 1);
    assert_eq!(json["data"]["files"][0]["probe_worker_id"], 1);
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
    redact_path_set(
        &mut json,
        &[
            (dir.path(), "[scan-dir]"),
            (media.as_path(), "[scan-dir]/tiny.mp4"),
            (note.as_path(), "[scan-dir]/note.txt"),
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
async fn scan_directory_outputs_durable_sidecar_links() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = dir.path().join("Movie.Name.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let sidecar = dir.path().join("Movie.Name.eng.srt");
    std::fs::write(&sidecar, b"1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();

    let output = scan_command(&seeded.url, dir.path()).output().unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "scan");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["summary"]["discovered"], 2);
    assert_eq!(json["data"]["summary"]["ingested"], 2);
    assert_eq!(json["data"]["summary"]["snapshots_recorded"], 1);
    assert_eq!(json["data"]["summary"]["skipped"], 0);
    let file = &json["data"]["files"][0];
    assert_eq!(
        file["path"],
        media.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(file["bundle_member_role"], "primary_video");
    assert!(file["bundle_id"].as_u64().unwrap() > 0);
    assert_eq!(file["sidecars"].as_array().unwrap().len(), 1);
    assert_eq!(
        file["sidecars"][0]["path"],
        sidecar.canonicalize().unwrap().display().to_string()
    );
    assert_eq!(file["sidecars"][0]["bundle_id"], file["bundle_id"]);
    assert_eq!(
        file["sidecars"][0]["bundle_member_role"],
        "external_subtitle"
    );
    assert!(
        file["sidecars"][0]["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "file_assets", 2).await;
    assert_table_count(&pool, "media_snapshots", 1).await;
    assert_table_count(&pool, "asset_bundle_members", 2).await;
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
    redact_path_set(&mut json, &[(note.as_path(), "[scan-dir]/note.txt")]);
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
    redact_path_set(&mut json, &[(media.as_path(), "[media]/tiny.mp4")]);
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
    redact_path_set(
        &mut json,
        &[
            (media.as_path(), "[scan-dir]/drift.mp4"),
            (fake_ffprobe.as_path(), "[scan-dir]/ffprobe"),
        ],
    );
    redact_content_hashes(&mut json);
    insta::assert_json_snapshot!("scan_content_drift_fails_without_snapshot", json);

    let pool = voom_store::connect(&seeded.url).await.unwrap();
    assert_table_count(&pool, "media_snapshots", 0).await;
}

#[tokio::test]
async fn policy_input_create_from_scan_outputs_ids_for_scanned_file() {
    let seeded = seed().await;
    let media = tiny_media_fixture();
    let scan = scan_command(&seeded.url, &media).output().unwrap();
    assert_status(&scan, Some(0));
    let scan_json = envelope(scan.stdout);
    let file = &scan_json["data"]["files"][0];
    let file_version_id = file["file_version_id"].as_u64().unwrap().to_string();
    let media_snapshot_id = file["media_snapshot_id"].as_u64().unwrap().to_string();

    let output = policy_input_from_scan_command(
        &seeded.url,
        "scan-h264",
        &file_version_id,
        &media_snapshot_id,
        "mp4",
        "h264",
    )
    .output()
    .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["input_set"]["input_set_id"].as_u64().unwrap() > 0);
    assert_eq!(json["data"]["input_set"]["slug"], "scan-h264");
    assert_eq!(json["data"]["input_set"]["source_kind"], "imported");
    assert_eq!(
        json["data"]["input_set"]["file_version_id"],
        file["file_version_id"]
    );
    assert_eq!(
        json["data"]["input_set"]["media_snapshot_id"],
        file["media_snapshot_id"]
    );
}

#[tokio::test]
async fn policy_input_create_from_scan_can_feed_plan_show() {
    let seeded = seed().await;
    let media = tiny_media_fixture();
    let scan = scan_command(&seeded.url, &media).output().unwrap();
    assert_status(&scan, Some(0));
    let scan_json = envelope(scan.stdout);
    let file = &scan_json["data"]["files"][0];
    let file_version_id = file["file_version_id"].as_u64().unwrap().to_string();
    let media_snapshot_id = file["media_snapshot_id"].as_u64().unwrap().to_string();
    let cp = voom_control_plane::ControlPlane::open(&seeded.url)
        .await
        .unwrap();
    let policy = cp
        .create_policy_document(
            "video-transcode-hevc",
            &load_policy_fixture("fixtures/policies/video-transcode-hevc.voom").unwrap(),
        )
        .await
        .unwrap();
    let create = policy_input_from_scan_command(
        &seeded.url,
        "scan-h264-plan",
        &file_version_id,
        &media_snapshot_id,
        "mp4",
        "h264",
    )
    .output()
    .unwrap();
    assert_status(&create, Some(0));
    let create_json = envelope(create.stdout);
    let input_set_id = create_json["data"]["input_set"]["input_set_id"]
        .as_u64()
        .unwrap()
        .to_string();

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "plan",
            "show",
            "--policy-version-id",
            &policy.version.id.0.to_string(),
            "--input-set-id",
            &input_set_id,
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "plan");
    assert_eq!(json["status"], "ok");
    assert_eq!(
        json["data"]["plan"]["input"]["input_set_id"],
        input_set_id.parse::<u64>().unwrap()
    );
}

#[tokio::test]
async fn policy_input_create_from_scan_all_builds_whole_library() {
    let seeded = seed().await;
    let dir = TempDir::new().unwrap();
    let media = dir.path().join("Movie.Name.mp4");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let sidecar = dir.path().join("Movie.Name.eng.srt");
    std::fs::write(&sidecar, b"1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();
    let scan = scan_command(&seeded.url, dir.path()).output().unwrap();
    assert_status(&scan, Some(0));

    let output = policy_input_whole_scan_command(&seeded.url, "whole")
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    assert!(json["data"]["input_set"]["input_set_id"].as_u64().unwrap() > 0);
    assert_eq!(json["data"]["input_set"]["slug"], "whole");
    assert_eq!(json["data"]["input_set"]["included_count"], 1);
    assert_eq!(json["data"]["input_set"]["skipped_count"], 1);
}

#[tokio::test]
async fn policy_input_create_from_scan_all_conflicts_with_single_file_args() {
    let seeded = seed().await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "whole",
            "--all",
            "--file-version-id",
            "1",
            "--media-snapshot-id",
            "1",
            "--container",
            "mp4",
            "--video-codec",
            "h264",
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn policy_input_create_from_scan_without_a_mode_is_bad_args() {
    let seeded = seed().await;

    let output = Command::new(env!("CARGO_BIN_EXE_voom"))
        .args([
            "--database-url",
            &seeded.url,
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "whole",
        ])
        .output()
        .unwrap();

    assert_status(&output, Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn policy_input_create_from_scan_missing_rows_is_not_found() {
    let seeded = seed().await;

    let output = policy_input_from_scan_command(
        &seeded.url,
        "missing-scan",
        "999998",
        "999999",
        "mp4",
        "h264",
    )
    .output()
    .unwrap();

    assert_status(&output, Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
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
        .env("VOOM_FFPROBE_WORKER_BIN", built_worker_binary())
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary());
    command
}

fn scan_command_without_worker_env(url: &str, path: &Path) -> Command {
    let _worker_binary = built_worker_binary();
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .args(["--database-url", url, "scan", "--path"])
        .arg(path)
        .env_remove("VOOM_FFPROBE_WORKER_BIN")
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary())
        .env("PATH", "/usr/bin:/bin");
    command
}

fn policy_input_from_scan_command(
    url: &str,
    slug: &str,
    file_version_id: &str,
    media_snapshot_id: &str,
    container: &str,
    video_codec: &str,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "input",
        "create-from-scan",
        "--slug",
        slug,
        "--file-version-id",
        file_version_id,
        "--media-snapshot-id",
        media_snapshot_id,
        "--container",
        container,
        "--video-codec",
        video_codec,
    ]);
    command
}

fn policy_input_whole_scan_command(url: &str, slug: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "input",
        "create-from-scan",
        "--slug",
        slug,
        "--all",
    ]);
    command
}

fn built_worker_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| cargo_bin_or_build("voom-ffprobe-worker", "voom-ffprobe-worker").unwrap())
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

fn write_drifting_ffprobe(dir: &Path) -> PathBuf {
    write_executable(
        dir,
        "ffprobe",
        "#!/usr/bin/env sh\n\
         set -eu\n\
         if [ \"${1:-}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         last=''\n\
         for arg in \"$@\"; do last=\"$arg\"; done\n\
         printf drift >> \"$last\"\n\
         printf '{\"format\":{\"format_name\":\"mov,mp4\",\"duration\":\"1.0\",\"bit_rate\":\"1\"},\"streams\":[]}\\n'\n",
    )
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
    redact_path_set(
        json,
        &[(success_ffprobe_binary().as_path(), "[ffprobe-bin]")],
    );
}

fn redact_path_set(value: &mut Value, paths: &[(&Path, &str)]) {
    let mut replacements = paths
        .iter()
        .flat_map(|(path, replacement)| path_redactions(path, replacement))
        .collect::<Vec<_>>();
    replacements.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));
    redact_paths(value, &replacements);
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
