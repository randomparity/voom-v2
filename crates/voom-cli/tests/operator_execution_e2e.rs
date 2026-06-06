//! End-to-end operator acceptance test: drive the real-media compliance
//! pipeline entirely through the shipped `voom` CLI, using the real
//! multi-process topology rather than in-process test helpers.
//!
//! The topology under test is two `voom worker run-local` child processes
//! (`--kind ffmpeg` and `--kind mkvtoolnix`), each a separately spawned `voom`
//! process that registers a bundled mutation worker and supervises it in the
//! foreground, plus a `voom compliance execute` process that dispatches the
//! `[Remux, TranscodeVideo]` plan to those workers. Every process shares ONE
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

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use voom_test_support::worker::{cargo_build_package, hide_stale_fake_ffprobe_sibling};

/// The Task 1 sample policy: a single `normalize` phase that remuxes to MKV and
/// transcodes video to HEVC. For an h264/mp4 source this plans `[Remux,
/// TranscodeVideo]`, exercising BOTH local workers in one phase.
const POLICY: &str = "policy \"remux-hevc\" {\n  \
     phase normalize {\n    container mkv\n    transcode video to hevc\n  }\n}\n";

const READY_TIMEOUT: Duration = Duration::from_mins(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test(flavor = "multi_thread")]
async fn operator_runs_real_media_pipeline_through_cli() {
    // Bundled workers the live topology spawns: the two mutation workers we run
    // via `run-local`, plus the ffprobe + verify workers the control plane spawns
    // as siblings during scan/execute.
    cargo_build_package("voom-ffmpeg-worker").unwrap();
    cargo_build_package("voom-mkvtoolnix-worker").unwrap();
    cargo_build_package("voom-ffprobe-worker").unwrap();
    cargo_build_package("voom-verify-artifact-worker").unwrap();
    // Post-commit result probes must run REAL ffprobe against committed bytes;
    // hide any canned test-helper `ffprobe` stub a sibling test left in the
    // shared profile dir.
    let _ffprobe_guard = hide_stale_fake_ffprobe_sibling("operator-execution-e2e").unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let library = root.join("library");
    std::fs::create_dir(&library).unwrap();
    let movie = library.join("Movie.mp4");
    generate_h264_fixture(&movie);
    std::fs::write(library.join("notes.txt"), b"just some notes, not a video\n").unwrap();

    let db = tempfile::NamedTempFile::new_in(&root).unwrap();
    let url = format!("sqlite://{}", db.path().display());

    // 3. Apply migrations against the shared DB.
    let init = run_voom(&url, &["init"]);
    assert_ok(&init, "init");

    // 4. Spawn the two real worker processes and gate on their readiness lines.
    let mut ffmpeg = LocalWorker::spawn(&url, "ffmpeg");
    let mut mkvtoolnix = LocalWorker::spawn(&url, "mkvtoolnix");
    ffmpeg.wait_for_ready(READY_TIMEOUT);
    mkvtoolnix.wait_for_ready(READY_TIMEOUT);

    // 5. Scan the library directory. notes.txt is filtered at discovery as an
    // unsupported extension, so the video is ingested and the text file skipped.
    let scan = run_voom(&url, &["scan", "--path", &library.display().to_string()]);
    let scan_json = assert_ok(&scan, "scan");
    assert_eq!(
        scan_json["data"]["summary"]["ingested"], 1,
        "exactly the one video is ingested: {scan_json}"
    );
    assert_eq!(
        scan_json["data"]["summary"]["skipped"], 1,
        "notes.txt is skipped at scan as an unsupported extension: {scan_json}"
    );

    // 6. Create the policy; capture the accepted version id.
    let policy_file = root.join("remux-and-hevc.voom");
    std::fs::write(&policy_file, POLICY).unwrap();
    let policy = run_voom(
        &url,
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

    // 7. Build the whole-library input set from scan rows; capture its id.
    let input = run_voom(
        &url,
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

    // 8. + 9. Run `compliance execute` while a concurrent `worker list` reader
    // hits the same DB; execution is the oracle for what commits.
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

    // 10. Retire the workers by closing their stdin; assert each prints its final
    // retirement envelope and exits cleanly.
    let ffmpeg_id = ffmpeg.worker_id;
    let mkvtoolnix_id = mkvtoolnix.worker_id;
    assert_retired_envelope(&ffmpeg.shutdown(), ffmpeg_id, "ffmpeg");
    assert_retired_envelope(&mkvtoolnix.shutdown(), mkvtoolnix_id, "mkvtoolnix");

    // After both supervisors retired their workers, neither is live anymore.
    let final_list = run_voom(&url, &["worker", "list"]);
    let final_json = assert_ok(&final_list, "worker list (post-shutdown)");
    assert_no_live_worker(&final_json, ffmpeg_id);
    assert_no_live_worker(&final_json, mkvtoolnix_id);
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
) -> std::process::Output {
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
/// exactly one completed `normalize` phase, one committed per-`(file, phase)` row
/// for the lone video, and a single on-disk MKV in `--output-dir`.
fn assert_execute_committed(execute: &std::process::Output, out_dir: &Path) {
    let execute_json = assert_ok(execute, "compliance execute");
    assert_eq!(execute_json["command"], "compliance");

    // Both operations the [Remux, TranscodeVideo] plan dispatched succeeded.
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
    assert_eq!(phases.len(), 1, "one policy phase: {execute_json}");
    assert_eq!(phases[0]["phase_name"], "normalize");
    assert_eq!(
        phases[0]["outcome"], "completed",
        "the normalize phase must complete: {execute_json}"
    );

    // Execution is the oracle: the remux+transcode chain commits a SINGLE
    // per-`(file, phase)` row (one chained output, not two artifacts), carrying
    // the produced version/location and a post-commit reprobe snapshot.
    let file_phases = execute_json["data"]["file_phases"].as_array().unwrap();
    assert_eq!(
        file_phases.len(),
        1,
        "the [Remux, TranscodeVideo] chain commits a single per-file row: {execute_json}"
    );
    let committed = &file_phases[0];
    assert_eq!(
        committed["outcome"], "committed",
        "the file phase must commit: {execute_json}"
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

/// A `voom worker run-local` child process: a real `voom` invocation that binds
/// a bundled mutation worker, prints a readiness line, and supervises until its
/// stdin closes. stdout is read line-by-line off-thread (the readiness line then
/// the final retirement envelope); stderr is drained into a buffer for failure
/// diagnostics.
struct LocalWorker {
    kind: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    worker_id: u64,
}

impl LocalWorker {
    fn spawn(url: &str, kind: &'static str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_voom"))
            .env("VOOM_DATABASE_URL", url)
            .args(["worker", "run-local", "--kind", kind])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let drain = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut buf);
            drain.lock().unwrap().push_str(&buf);
        });

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            kind,
            child,
            stdin: Some(stdin),
            stdout_rx: rx,
            stderr: stderr_buf,
            worker_id: 0,
        }
    }

    /// Block until the child prints `{"status":"ready",...}`, recording its
    /// worker id. Panics (with captured stderr) if the child exits first or the
    /// readiness line never arrives within `timeout`.
    fn wait_for_ready(&mut self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!(
                    "run-local {} exited before ready (status={status}); stderr:\n{}",
                    self.kind,
                    self.stderr_snapshot()
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out waiting for {} readiness; stderr:\n{}",
                self.kind,
                self.stderr_snapshot()
            );
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(line) => {
                    let Ok(value) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if value["status"] == "ready" {
                        assert_eq!(value["kind"], self.kind, "ready line kind mismatch");
                        self.worker_id = value["worker_id"].as_u64().unwrap();
                        return;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => panic!(
                    "run-local {} closed stdout before ready; stderr:\n{}",
                    self.kind,
                    self.stderr_snapshot()
                ),
            }
        }
    }

    /// Close stdin (the supervisor's shutdown signal), drain the remaining stdout
    /// lines, and return the final JSON envelope the supervisor prints on retire.
    fn shutdown(&mut self) -> Value {
        drop(self.stdin.take());
        let mut last = None;
        while let Ok(line) = self.stdout_rx.recv_timeout(SHUTDOWN_TIMEOUT) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                last = Some(value);
            }
        }
        let status = self.child.wait().unwrap();
        assert!(
            status.success(),
            "run-local {} exited nonzero ({status}); stderr:\n{}",
            self.kind,
            self.stderr_snapshot()
        );
        last.unwrap_or_else(|| {
            panic!(
                "run-local {} printed no shutdown envelope; stderr:\n{}",
                self.kind,
                self.stderr_snapshot()
            )
        })
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr.lock().unwrap().clone()
    }
}

impl Drop for LocalWorker {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Invoke the shipped `voom` binary against the shared DB. The database URL is
/// passed via `VOOM_DATABASE_URL` so every process in the topology agrees.
fn run_voom(url: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_DATABASE_URL", url)
        .args(args)
        .output()
        .unwrap()
}

/// Assert the command exited 0 with an `ok` envelope on stdout, returning it.
fn assert_ok(output: &std::process::Output, what: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what} must exit 0; stdout={} stderr={}",
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

fn generate_h264_fixture(path: &Path) {
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
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
        ])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "ffmpeg fixture generation failed: {status}"
    );
}
