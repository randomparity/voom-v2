#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "integration tests fail loudly and preserve stdout/stderr for diagnosis"
)]

//! #282 end-to-end: a manual use-lease acquired through `voom lease acquire`
//! blocks a real `voom artifact commit` via the #270 commit safety gate, and
//! `voom lease force-release` unblocks it. Everything is driven through the
//! shipped `voom` binary against one shared on-disk `SQLite` database; identity
//! rows are seeded through the durable scan-session chain (`scan_seed`) and
//! verify / the post-commit reprobe use built worker binaries with a canned
//! ffprobe (no ffmpeg required).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;
use tempfile::TempDir;
use voom_control_plane::ControlPlane;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, seed_scanned_files};
use voom_test_support::worker::cargo_bin_or_build;

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn manual_lock_blocks_commit_and_force_release_unblocks_it() {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();

    let dir = TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let source = dir.path().join("tiny.mp4");
    std::fs::copy(tiny_media_fixture(), &source).unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE library_roots SET provider_locator = ?, display_locator = ? WHERE id = ?")
        .bind(dir.path().display().to_string())
        .bind(dir.path().display().to_string())
        .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let staging = dir.path().join("staged.mp4");
    let target = dir.path().join("committed.mp4");

    // Seed the fixture's identity rows, then stage + verify an artifact ready
    // to commit.
    let cp = ControlPlane::open(&url).await.unwrap();
    let seeded_source = seed_scanned_files(
        &cp,
        &url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &[SeedFile {
            locator: "tiny.mp4",
            path: &source,
            probe_snapshot: basic_mp4_probe_snapshot(),
        }],
    )
    .await
    .unwrap();
    let seeded_source = &seeded_source[0];
    let file_version_id = seeded_source.file_version_id.0;
    let file_location_id = seeded_source.file_location_id.0;

    let stage = run(
        cmd(&url)
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
        0,
    );
    let artifact_handle_id = id(&stage["data"]["artifact"]["artifact_handle_id"]);
    let verify = run(
        cmd(&url)
            .args([
                "artifact",
                "verify",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--staging-root",
            ])
            .arg(dir.path()),
        0,
    );
    assert_eq!(verify["data"]["artifact"]["status"], "succeeded");

    // Acquire a manual lock on the source version scope.
    let acquire = run(
        cmd(&url).args([
            "lease",
            "acquire",
            "--scope-type",
            "version",
            "--scope-id",
            &file_version_id.to_string(),
            "--issuer-ref",
            "operator-alice",
        ]),
        0,
    );
    let lease_id = id(&acquire["data"]["id"]);
    assert_eq!(acquire["data"]["kind"], "manual_lock");
    assert_eq!(acquire["data"]["blocking_mode"], "blocking");

    // `lease list` surfaces the live lock and its age.
    let list = run(cmd(&url).args(["lease", "list"]), 0);
    let locks = list["data"]["locks"].as_array().unwrap();
    assert_eq!(locks.len(), 1, "the one live manual lock: {list}");
    assert_eq!(locks[0]["id"].as_u64().unwrap(), lease_id);
    assert!(
        locks[0]["age_seconds"].is_number(),
        "list surfaces age for forgotten-hold spotting: {list}"
    );

    // The commit is blocked by the live lock, before the target is written.
    let blocked = run(
        cmd(&url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target),
        2,
    );
    assert_eq!(blocked["error"]["code"], "BLOCKED_BY_USE_LEASE");
    assert!(
        blocked["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&lease_id.to_string()),
        "the blocked commit names the offending lease: {blocked}"
    );
    assert!(
        !target.exists(),
        "a blocked commit must not install the target"
    );

    // Force-release the lock with an audited actor + reason.
    let forced = run(
        cmd(&url).args([
            "lease",
            "force-release",
            "--lease-id",
            &lease_id.to_string(),
            "--actor",
            "operator-bob",
            "--reason",
            "forgotten hold on a stuck job",
        ]),
        0,
    );
    assert_eq!(forced["data"]["release_reason"], "force_released");

    // With the lock gone, the same commit now succeeds and installs the target.
    let committed = run(
        cmd(&url)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target),
        0,
    );
    assert_eq!(committed["data"]["artifact"]["state"], "committed");
    assert!(target.is_file(), "the unblocked commit installs the target");

    // The force-release cleared the lock: `lease list` is now empty.
    let after = run(cmd(&url).args(["lease", "list"]), 0);
    assert!(
        after["data"]["locks"].as_array().unwrap().is_empty(),
        "the force-released lock is no longer live: {after}"
    );
}

/// A `voom` invocation against the shared DB, with the worker binaries and a
/// canned ffprobe wired in via env so scan / verify / reprobe run without
/// ffmpeg. Lease commands ignore the worker env; setting it is harmless.
fn cmd(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .args(["--database-url", url])
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        )
        .env(
            "VOOM_FFPROBE_WORKER_BIN",
            built_worker("voom-ffprobe-worker"),
        )
        .env(
            "VOOM_VERIFY_ARTIFACT_WORKER_BIN",
            built_worker("voom-verify-artifact-worker"),
        )
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary());
    command
}

fn built_worker(package: &'static str) -> PathBuf {
    cargo_bin_or_build(package, package).unwrap()
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
        use std::os::unix::fs::PermissionsExt as _;
        let dir = TempDir::new().unwrap();
        let script = format!(
            "#!/usr/bin/env sh\n\
             set -eu\n\
             if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
             cat <<'JSON'\n\
             {BASIC_FFPROBE_JSON}\n\
             JSON\n"
        );

        let path = dir.path().join("ffprobe");
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        (dir, path)
    })
    .1
}

/// Canned normalized probe snapshot matching what the real ffprobe worker
/// reports for the tiny fixture (`basic-mp4.json` once normalized).
fn basic_mp4_probe_snapshot() -> Value {
    serde_json::json!({
        "format": "sprint10-v1",
        "container": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
        },
        "streams": [
            {
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "width": 320,
                "height": 180,
            },
            {
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "channels": 2,
            },
        ],
    })
}

fn run(command: &mut Command, expected: i32) -> Value {
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

fn id(value: &Value) -> u64 {
    value.as_u64().unwrap()
}
