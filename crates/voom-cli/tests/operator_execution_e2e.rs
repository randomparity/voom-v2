//! End-to-end operator acceptance test: drive the media compliance pipeline
//! entirely through the shipped `voom` CLI, on the owner-node dispatch cutover
//! (issue #423 T9; originally issue #166's multi-process topology).
//!
//! Every phase runs through separate `voom` child processes sharing ONE on-disk
//! `SQLite` database via `--database-url`: a `voom compliance execute` process
//! dispatches the `[Remux]` -> `[TranscodeVideo]` plan while the main thread
//! issues concurrent `voom worker list` reads against the same database.
//! Envelope-bearing media tickets are settled by the owner-node emulator
//! (`support/owner_node.rs`) standing in for the storage owner's agent, with
//! fenced commit intents driven to convergence by a simulated node.
//!
//! Execution is the oracle: rather than asserting an assumed artifact shape, the
//! test inspects what `execute` actually committed (the per-`(file, phase)` rows
//! and the promoted terminal artifact in `--output-dir`) and asserts that.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly and preserve paths/stderr for diagnosis"
)]

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use voom_control_plane::ControlPlane;
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, seed_scanned_files};
use voom_test_support::worker::{
    TestWorkerConfig, TestWorkerLaunch, cargo_bin_or_build, target_debug_binary,
};

#[path = "support/owner_node.rs"]
mod owner_node;

/// The sample policy remuxes to MKV, then transcodes video to HEVC in a dependent
/// phase: the proven dependent two-mutation shape.
const POLICY: &str = "policy \"remux-hevc\" {\n  \
     phase remux {\n    container mkv\n  }\n  \
     phase transcode {\n    depends_on: [remux]\n    transcode video to hevc\n  }\n}\n";

#[tokio::test(flavor = "multi_thread")]
async fn operator_runs_media_pipeline_through_cli() {
    let _verify_worker =
        cargo_bin_or_build("voom-verify-artifact-worker", "voom-verify-artifact-worker").unwrap();
    voom_test_support::worker::cargo_build_package("voom-ffmpeg-worker").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let library = root.join("library");
    std::fs::create_dir(&library).unwrap();
    std::fs::write(library.join("Movie.mp4"), b"operator e2e source bytes").unwrap();
    std::fs::write(library.join("notes.txt"), b"just some notes, not a video\n").unwrap();

    let db = TempDatabase::new_in(&root).unwrap();
    let url = format!("sqlite://{}", db.path().display());
    voom_store::init(&url).await.unwrap();
    assert_ok(&run_voom(&url, &["init"]), "init");
    let pool = voom_store::connect(&url).await.unwrap();
    voom_store::test_support::seed_test_storage_root(&pool)
        .await
        .unwrap();
    // Point the shared storage root at the library directory and make it its
    // own staging/backup default, so envelope destinations resolve inside the
    // operator-visible tree.
    voom_store::test_support::set_test_storage_root_path(&pool, &library)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE library_roots SET default_staging_root_id = id, \
         default_backup_root_id = id WHERE id = ?",
    )
    .bind(i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    // Storage-owner stand-ins: fenced commit-intent driver + media settlement.
    let _emulator = owner_node::OwnerNodeEmulator::spawn(&url);

    // Live capable workers satisfy the software-transcode preflight the same
    // way a real deployment does; the media tickets themselves are node-local.
    let cp = ControlPlane::open_with_pool(pool, std::sync::Arc::new(voom_core::SystemClock))
        .await
        .unwrap();
    let mut ffmpeg = TestWorkerLaunch::start(
        &cp,
        TestWorkerConfig::synthetic(
            target_debug_binary("voom-ffmpeg-worker"),
            "operator-e2e-ffmpeg",
            "operator-e2e-ffmpeg-secret",
            "transcode_video",
        ),
    )
    .await
    .unwrap();

    let (policy_version_id, input_set_id) = create_library_policy(&cp, &url, &library).await;

    let out_dir = library.join("out");
    let execute = run_execute_with_concurrent_reader(
        &url,
        policy_version_id,
        input_set_id,
        &library,
        &out_dir,
    );
    assert_execute_committed(&execute, &out_dir);
    ffmpeg.shutdown().unwrap();
}

async fn create_library_policy(cp: &ControlPlane, url: &str, library: &Path) -> (u64, u64) {
    // Seed the one video's identity rows through the scan seeding chain;
    // notes.txt never becomes a file-version because only media files are
    // seeded.
    let source = library.join("Movie.mp4");
    let seeded = seed_scanned_files(
        cp,
        url,
        voom_store::test_support::TEST_STORAGE_ROOT_ID,
        &[SeedFile {
            locator: "Movie.mp4",
            path: &source,
            probe_snapshot: json!({
                "format": "sprint10-v1",
                "container": { "format_name": "mov,mp4,m4a,3gp,3g2,mj2" },
                "streams": [{
                    "index": 0,
                    "kind": "video",
                    "codec_name": "h264",
                    "width": 32,
                    "height": 32,
                    "disposition": { "default": true, "forced": false, "commentary": false },
                }],
            }),
            sidecars: Vec::new(),
        }],
    )
    .await
    .unwrap();
    assert_eq!(seeded.len(), 1, "exactly the one video is ingested");

    let policy_file = library.join("remux-and-hevc.voom");
    std::fs::write(&policy_file, POLICY).unwrap();
    let policy = run_voom(
        url,
        &[
            "policy",
            "create",
            "--slug",
            "remux-hevc",
            "--file",
            &policy_file.display().to_string(),
        ],
    );
    let policy_json = assert_ok(&policy, "policy create");
    let policy_version_id = policy_json["data"]["version"]["version_id"]
        .as_u64()
        .unwrap();

    let input = run_voom(
        url,
        &[
            "policy",
            "input",
            "create-from-scan",
            "--all",
            "--slug",
            "lib1",
        ],
    );
    let input_json = assert_ok(&input, "policy input create-from-scan");
    let input_set = &input_json["data"]["input_set"];
    assert_eq!(
        input_set["included_count"], 1,
        "only the video file-version is included: {input_json}"
    );
    // notes.txt is excluded at scan (unsupported extension), so it never becomes
    // a live file-version. The whole-scan `skipped_count` counts live
    // file-versions whose latest snapshot lacks a video stream, of which there
    // are none here.
    assert_eq!(
        input_set["skipped_count"], 0,
        "whole-scan skips only live non-video file-versions; notes.txt was \
         already filtered at scan: {input_json}"
    );
    let input_set_id = input_set["input_set_id"].as_u64().unwrap();
    (policy_version_id, input_set_id)
}

/// Run `compliance execute` on a worker thread while the main thread issues
/// concurrent `voom worker list` reads against the same `SQLite` DB. Returns the
/// execute process output. Panics if no concurrent read landed while execute was
/// in flight (the test would otherwise not prove concurrency) or if any reader
/// failed to return an `ok` envelope.
fn run_execute_with_concurrent_reader(
    url: &str,
    policy_version_id: u64,
    input_set_id: u64,
    library: &Path,
    out_dir: &Path,
) -> std::process::Output {
    let exec_url = url.to_owned();
    // The staging flag mirrors the storage-root path (the library): the
    // coordinator's promotion plan pairs `<staging>/.committed/<op>` working
    // dirs with the operator output dir.
    let staging = library.display().to_string();
    let output = out_dir.display().to_string();
    let exec = std::thread::spawn(move || {
        run_voom(
            &exec_url,
            &[
                "compliance",
                "execute",
                "--policy-version-id",
                &policy_version_id.to_string(),
                "--input-set-id",
                &input_set_id.to_string(),
                "--staging-root",
                &staging,
                "--output-dir",
                &output,
            ],
        )
    });

    let mut concurrent_reads = 0_u32;
    while !exec.is_finished() {
        let list = run_voom(url, &["worker", "list"]);
        let list_json = assert_ok(&list, "worker list (concurrent)");
        assert_ne!(
            list_json["error"]["code"], "DB_UNREACHABLE",
            "concurrent reader must not be locked out of the shared DB: {list_json}"
        );
        concurrent_reads += 1;
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        concurrent_reads > 0,
        "expected at least one concurrent worker-list read during execute"
    );
    exec.join().unwrap()
}

/// Assert the execute run succeeded and inspect what it actually committed:
/// two completed phases, one committed per-`(file, phase)` row for each phase, and
/// the promoted terminal MKV in `--output-dir`.
fn assert_execute_committed(execute: &std::process::Output, out_dir: &Path) {
    let execute_json = assert_ok(execute, "compliance execute");
    assert_eq!(execute_json["command"], "compliance");

    // Both operations in the dependent [Remux] -> [TranscodeVideo] plan succeeded.
    let summary = &execute_json["data"]["summary"];
    assert_eq!(
        summary["failure_count"], 0,
        "no operation may fail: {execute_json}"
    );
    let per_op = &summary["per_operation"];
    assert_eq!(
        per_op["remux"]["success_count"], 1,
        "the remux operation must succeed: {execute_json}"
    );
    assert_eq!(
        per_op["transcode_video"]["success_count"], 1,
        "the transcode_video operation must succeed: {execute_json}"
    );

    let phases = execute_json["data"]["phases"].as_array().unwrap();
    assert_eq!(phases.len(), 2, "two policy phases: {execute_json}");
    assert_eq!(phases[0]["phase_name"], "remux");
    assert_eq!(phases[1]["phase_name"], "transcode");
    assert!(
        phases.iter().all(|phase| phase["outcome"] == "completed"),
        "both policy phases must complete: {execute_json}"
    );

    // Each mutation phase commits one per-`(file, phase)` row carrying the produced
    // version/location and a post-commit reprobe snapshot.
    let file_phases = execute_json["data"]["file_phases"].as_array().unwrap();
    assert_eq!(
        file_phases.len(),
        2,
        "the mutation chain commits one per-file row per phase: {execute_json}"
    );
    for committed in file_phases {
        assert_eq!(
            committed["outcome"], "committed",
            "each file phase must commit: {execute_json}"
        );
        assert!(
            committed["produced_file_version_id"].as_u64().unwrap() > 0,
            "a committed phase produces a new file version: {execute_json}"
        );
        assert!(
            committed["produced_file_location_id"].as_u64().unwrap() > 0,
            "a committed phase records the produced file location: {execute_json}"
        );
        assert!(
            committed["reprobe_snapshot_id"].as_u64().unwrap() > 0,
            "a committed phase records a post-commit reprobe snapshot: {execute_json}"
        );
    }

    let outputs = list_dir(out_dir);
    let mkvs: Vec<&String> = outputs
        .iter()
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mkv"))
        })
        .collect();
    assert_eq!(
        mkvs.len(),
        1,
        "exactly one committed MKV lands in the output dir; saw {outputs:?}"
    );
}

/// Invoke the shipped `voom` binary against the shared DB. The database URL is
/// passed via `VOOM_DATABASE_URL` so every process in the topology agrees.
fn run_voom(url: &str, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_DATABASE_URL", url)
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        )
        .args(args)
        .output()
        .unwrap()
}

/// Assert the command exited 0 with an `ok` envelope on stdout, returning it.
fn assert_ok(output: &std::process::Output, what: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = envelope(&output.stdout);
    assert_eq!(value["status"], "ok", "{what} must be ok: {value}");
    value
}

fn envelope(stdout: &[u8]) -> Value {
    let stdout = String::from_utf8(stdout.to_vec()).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
}

fn list_dir(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
