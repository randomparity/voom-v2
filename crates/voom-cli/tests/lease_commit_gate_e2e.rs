#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "integration tests fail loudly and preserve stdout/stderr for diagnosis"
)]

//! #282 end-to-end: a manual use-lease acquired through `voom lease acquire`
//! blocks a real `voom artifact commit` via the #270 commit safety gate, and
//! `voom lease force-release` unblocks it. Everything is driven through the
//! shipped `voom` binary against one shared on-disk `SQLite` database; scan,
//! verify, and the post-commit reprobe use the built worker binaries with a
//! canned ffprobe (no ffmpeg required).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::worker::cargo_bin_or_build;

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn manual_lock_blocks_commit_and_force_release_unblocks_it() {
    let tmp = NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();

    let dir = TempDir::new_in(std::env::current_dir().unwrap()).unwrap();
    let staging = dir.path().join("staged.mp4");
    let target = dir.path().join("committed.mp4");

    // Scan the fixture and stage + verify an artifact ready to commit.
    let scan = run(
        cmd(&url).args(["scan", "--path"]).arg(tiny_media_fixture()),
        0,
    );
    let file_version_id = id(&scan["data"]["files"][0]["file_version_id"]);
    let file_location_id = id(&scan["data"]["files"][0]["file_location_id"]);

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
