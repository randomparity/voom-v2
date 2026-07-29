//! End-to-end operator acceptance test: drive the real-media compliance
//! pipeline entirely through the shipped `voom` CLI, using the real
//! multi-process topology rather than in-process test helpers.
//!
//! The topology under test is two `voom worker run-local` child processes
//! (`--kind ffmpeg` and `--kind mkvtoolnix`), each a separately spawned `voom`
//! process that registers a bundled mutation worker and supervises it in the
//! foreground, plus a `voom compliance execute` process that dispatches the
//! `[Remux]` then `[TranscodeVideo]` plan to those workers. Every process shares ONE
//! on-disk `SQLite` database via `VOOM_DATABASE_URL`.
//!
//! Execution is the oracle: rather than asserting an assumed artifact shape, the
//! test inspects what `execute` actually committed (the per-`(file, phase)` rows
//! and the on-disk `--output-dir`) and asserts that.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly and preserve paths/stderr for diagnosis"
)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use voom_test_support::worker::hide_stale_fake_ffprobe_sibling;

#[path = "support/local_worker.rs"]
mod local_worker;
#[path = "support/process.rs"]
mod process;

use local_worker::LocalWorker;
use process::{BoundedOutput, build_worker_package, run_bounded};

/// The sample policy remuxes to MKV, then transcodes video to HEVC in a dependent
/// phase. For an h264/mp4 source this exercises both local mutation workers.
const POLICY: &str = "policy \"remux-hevc\" {\n  \
     phase remux {\n    container mkv\n  }\n  \
     phase transcode {\n    depends_on: [remux]\n    transcode video to hevc\n  }\n}\n";

const READY_TIMEOUT: Duration = Duration::from_mins(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const PROCESS_TIMEOUT: Duration = Duration::from_mins(2);
const BUILD_TIMEOUT: Duration = Duration::from_mins(5);

#[tokio::test(flavor = "multi_thread")]
async fn operator_runs_real_media_pipeline_through_cli() {
    prepare_worker_binaries();
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("operator-execution-e2e").unwrap();
    let (_tmp, root, library, url) = prepare_operator_fixture();
    assert_ok(&run_voom(&url, &["init"]), "init");

    let mut ffmpeg = LocalWorker::spawn(&url, "ffmpeg").unwrap();
    let mut mkvtoolnix = LocalWorker::spawn(&url, "mkvtoolnix").unwrap();
    ffmpeg.wait_for_ready(READY_TIMEOUT).unwrap();
    mkvtoolnix.wait_for_ready(READY_TIMEOUT).unwrap();
    let (policy_version_id, input_set_id) = create_library_policy(&url, &root, &library);

    let out_dir = root.join("out");
    let staging_root = root.join("stage");
    let execute = run_execute_with_concurrent_reader(
        &url,
        policy_version_id,
        input_set_id,
        &staging_root,
        &out_dir,
    );
    assert_execute_committed(&execute, &out_dir);

    let ffmpeg_id = ffmpeg.worker_id();
    let mkvtoolnix_id = mkvtoolnix.worker_id();
    assert_retired_envelope(
        &ffmpeg.shutdown(SHUTDOWN_TIMEOUT).unwrap(),
        ffmpeg_id,
        "ffmpeg",
    );
    assert_retired_envelope(
        &mkvtoolnix.shutdown(SHUTDOWN_TIMEOUT).unwrap(),
        mkvtoolnix_id,
        "mkvtoolnix",
    );

    let final_list = run_voom(&url, &["worker", "list"]);
    let final_json = assert_ok(&final_list, "worker list (post-shutdown)");
    assert_no_live_worker(&final_json, ffmpeg_id);
    assert_no_live_worker(&final_json, mkvtoolnix_id);
}

fn create_library_policy(url: &str, root: &Path, library: &Path) -> (u64, u64) {
    let scan = run_voom(url, &["scan", "--path", &library.display().to_string()]);
    let scan_json = assert_ok(&scan, "scan");
    assert_eq!(
        scan_json["data"]["summary"]["ingested"], 1,
        "exactly the one video is ingested: {scan_json}"
    );
    assert_eq!(
        scan_json["data"]["summary"]["skipped"], 1,
        "notes.txt is skipped at scan as an unsupported extension: {scan_json}"
    );

    let policy_file = root.join("remux-and-hevc.voom");
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

fn prepare_worker_binaries() {
    for package in [
        "voom-ffmpeg-worker",
        "voom-mkvtoolnix-worker",
        "voom-ffprobe-worker",
        "voom-verify-artifact-worker",
    ] {
        build_worker_package(package, BUILD_TIMEOUT).unwrap();
    }
}

fn prepare_operator_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let library = root.join("library");
    std::fs::create_dir(&library).unwrap();
    generate_h264_fixture(&library.join("Movie.mp4"));
    std::fs::write(library.join("notes.txt"), b"just some notes, not a video\n").unwrap();
    let db = tempfile::NamedTempFile::new_in(&root).unwrap();
    let url = format!("sqlite://{}", db.path().display());
    (tmp, root, library, url)
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
    staging_root: &Path,
    out_dir: &Path,
) -> BoundedOutput {
    let exec_url = url.to_owned();
    let staging = staging_root.display().to_string();
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
/// a single final MKV in `--output-dir`.
fn assert_execute_committed(execute: &BoundedOutput, out_dir: &Path) {
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

fn assert_retired_envelope(envelope: &Value, worker_id: u64, kind: &str) {
    assert_eq!(envelope["command"], "worker", "{kind} shutdown envelope");
    assert_eq!(
        envelope["status"], "ok",
        "{kind} must retire cleanly: {envelope}"
    );
    assert_eq!(
        envelope["data"]["status"], "retired",
        "{kind} run-local must report retirement: {envelope}"
    );
    assert_eq!(
        envelope["data"]["worker_id"].as_u64().unwrap(),
        worker_id,
        "{kind} retirement must name the worker it started"
    );
}

fn assert_no_live_worker(list_json: &Value, worker_id: u64) {
    let workers = list_json["data"]["workers"].as_array().unwrap();
    let live = workers.iter().find(|worker| {
        worker["id"].as_u64() == Some(worker_id)
            && matches!(worker["status"].as_str(), Some("registered" | "active"))
    });
    assert!(
        live.is_none(),
        "worker {worker_id} must not be live after shutdown: {list_json}"
    );
}

/// Invoke the shipped `voom` binary against the shared DB. The database URL is
/// passed via `VOOM_DATABASE_URL` so every process in the topology agrees.
fn run_voom(url: &str, args: &[&str]) -> BoundedOutput {
    run_bounded(
        Command::new(env!("CARGO_BIN_EXE_voom"))
            .env("VOOM_DATABASE_URL", url)
            .args(args),
        PROCESS_TIMEOUT,
    )
    .unwrap()
}

/// Assert the command exited 0 with an `ok` envelope on stdout, returning it.
fn assert_ok(output: &BoundedOutput, what: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        output.diagnostics(what)
    );
    assert!(!output.timed_out, "{}", output.diagnostics(what));
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

fn generate_h264_fixture(path: &Path) {
    let output = run_bounded(
        Command::new("ffmpeg").args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x32:rate=1",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            path.to_str().unwrap(),
        ]),
        PROCESS_TIMEOUT,
    )
    .unwrap();
    assert!(
        !output.timed_out && output.status.success(),
        "{}",
        output.diagnostics("ffmpeg fixture generation")
    );
}
