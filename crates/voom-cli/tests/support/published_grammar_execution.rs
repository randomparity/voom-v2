#![allow(
    dead_code,
    reason = "execution helpers are shared as corpus scenarios are added serially"
)]

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_test_support::worker::hide_stale_fake_ffprobe_sibling;

use crate::local_worker::LocalWorker;
use crate::media_inspect::ffprobe;
use crate::process::{BoundedOutput, build_worker_package, run_bounded};
use crate::published_grammar_media::ScenarioMedia;

const BUILD_TIMEOUT: Duration = Duration::from_mins(5);
const PROCESS_TIMEOUT: Duration = Duration::from_mins(2);
const READY_TIMEOUT: Duration = Duration::from_mins(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

pub struct WorkerBinaryGuard {
    _ffprobe: voom_test_support::worker::FfprobeSiblingGuard,
}

struct ScenarioRun {
    _db: NamedTempFile,
    url: String,
    ffmpeg: LocalWorker,
    mkvtoolnix: LocalWorker,
}

pub fn prepare_worker_binaries() -> io::Result<WorkerBinaryGuard> {
    for package in [
        "voom-ffmpeg-worker",
        "voom-mkvtoolnix-worker",
        "voom-ffprobe-worker",
        "voom-verify-artifact-worker",
    ] {
        build_worker_package(package, BUILD_TIMEOUT)?;
    }
    Ok(WorkerBinaryGuard {
        _ffprobe: hide_stale_fake_ffprobe_sibling("published-grammar-corpus")?,
    })
}

pub fn execute_core(media: &ScenarioMedia) -> io::Result<()> {
    let mut run = ScenarioRun::start(&media.root)?;
    let scan = run.scan(&media.library)?;
    require(scan["data"]["summary"]["ingested"] == 1, "C1 scan ingested")?;
    require(scan["data"]["summary"]["failed"] == 0, "C1 scan failures")?;
    let version_id = run.create_policy("published-grammar-core", "published-grammar-core.voom")?;
    let input_id = run.create_input("published-grammar-core-input", 1)?;
    let preview = run.preview(version_id, input_id)?;
    assert_core_preview(&preview)?;
    let staging = media.root.join("stage");
    let output = media.root.join("output");
    let execute = run.execute(version_id, input_id, &staging, &output)?;
    assert_core_execute(&execute)?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "C1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "C1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    assert_core_inspections(&run, &execute, job_id)?;
    assert_core_output(&output)?;
    run.shutdown()?;
    Ok(())
}

impl ScenarioRun {
    fn start(root: &Path) -> io::Result<Self> {
        let db = NamedTempFile::new_in(root)?;
        let url = format!("sqlite://{}", db.path().display());
        let init = run_cli(&url, &["init"])?;
        assert_ok_envelope(&init, "init", "init")?;
        let mut ffmpeg = LocalWorker::spawn(&url, "ffmpeg")?;
        let mut mkvtoolnix = LocalWorker::spawn(&url, "mkvtoolnix")?;
        ffmpeg.wait_for_ready(READY_TIMEOUT)?;
        mkvtoolnix.wait_for_ready(READY_TIMEOUT)?;
        Ok(Self {
            _db: db,
            url,
            ffmpeg,
            mkvtoolnix,
        })
    }

    fn scan(&self, library: &Path) -> io::Result<Value> {
        self.ok(
            &["scan", "--path", &library.display().to_string()],
            "scan",
            "scan",
        )
    }

    fn create_policy(&self, slug: &str, file_name: &str) -> io::Result<u64> {
        let source = policy_fixture(file_name)?;
        let json = self.ok(
            &[
                "policy",
                "create",
                "--slug",
                slug,
                "--file",
                &source.display().to_string(),
            ],
            "policy",
            "policy create",
        )?;
        number(&json["data"]["version"]["version_id"], "policy version id")
    }

    fn create_input(&self, slug: &str, expected_count: u64) -> io::Result<u64> {
        let json = self.ok(
            &[
                "policy",
                "input",
                "create-from-scan",
                "--all",
                "--slug",
                slug,
            ],
            "policy",
            "policy input create-from-scan",
        )?;
        require(
            json["data"]["input_set"]["included_count"] == expected_count,
            format!("input set expected {expected_count} members: {json}"),
        )?;
        number(
            &json["data"]["input_set"]["input_set_id"],
            "policy input set id",
        )
    }

    fn preview(&self, version_id: u64, input_id: u64) -> io::Result<Value> {
        self.ok(
            &[
                "compliance",
                "report",
                "--policy-version-id",
                &version_id.to_string(),
                "--input-set-id",
                &input_id.to_string(),
            ],
            "compliance",
            "compliance preview",
        )
    }

    fn execute(
        &self,
        version_id: u64,
        input_id: u64,
        staging: &Path,
        output: &Path,
    ) -> io::Result<Value> {
        let version_id = version_id.to_string();
        let input_id = input_id.to_string();
        let staging = staging.display().to_string();
        let output = output.display().to_string();
        let args = [
            "compliance",
            "execute",
            "--policy-version-id",
            &version_id,
            "--input-set-id",
            &input_id,
            "--staging-root",
            &staging,
            "--output-dir",
            &output,
        ];
        let result = self.ok(&args, "compliance", "compliance execute");
        result.map_err(|error| {
            io::Error::other(format!(
                "{error}\nDurable state after execute failure:\n{}",
                self.failure_state()
            ))
        })
    }

    fn ok(&self, args: &[&str], command: &str, what: &str) -> io::Result<Value> {
        let output = run_cli(&self.url, args)?;
        assert_ok_envelope(&output, command, what)
    }

    fn shutdown(&mut self) -> io::Result<()> {
        let ffmpeg_id = self.ffmpeg.worker_id();
        let mkvtoolnix_id = self.mkvtoolnix.worker_id();
        let ffmpeg = self.ffmpeg.shutdown(SHUTDOWN_TIMEOUT)?;
        let mkvtoolnix = self.mkvtoolnix.shutdown(SHUTDOWN_TIMEOUT)?;
        assert_retired(&ffmpeg, ffmpeg_id, "ffmpeg")?;
        assert_retired(&mkvtoolnix, mkvtoolnix_id, "mkvtoolnix")
    }

    fn failure_state(&self) -> String {
        [
            ["job", "list"].as_slice(),
            ["ticket", "list"].as_slice(),
            ["artifact", "list"].as_slice(),
            ["event", "list"].as_slice(),
        ]
        .iter()
        .map(|args| match run_cli(&self.url, args) {
            Ok(output) => format!(
                "{}:\nstdout={}\nstderr={}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => format!("{}: {error}", args.join(" ")),
        })
        .collect::<Vec<_>>()
        .join("\n")
    }
}

fn assert_core_preview(preview: &Value) -> io::Result<()> {
    let nodes = array(&preview["data"]["plan"]["nodes"], "C1 preview nodes")?;
    let kinds = nodes
        .iter()
        .filter_map(|node| node["operation_kind"].as_str())
        .collect::<BTreeSet<_>>();
    require(
        kinds == BTreeSet::from(["remux", "transcode_video", "verify_artifact"]),
        format!("C1 preview operation kinds: {kinds:?}; preview={preview}"),
    )?;
    require(
        nodes.iter().all(|node| node["status"] != "blocked"),
        format!("C1 preview contains blocked node: {preview}"),
    )?;
    require(
        preview["data"]["plan"]["diagnostics"]
            .as_array()
            .is_some_and(Vec::is_empty),
        format!("C1 preview diagnostics: {preview}"),
    )
}

fn assert_core_execute(execute: &Value) -> io::Result<()> {
    require(
        execute["data"]["summary"]["failure_count"] == 0,
        format!("C1 execution failures: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], "C1 phases")?;
    let names = phases
        .iter()
        .filter_map(|phase| phase["phase_name"].as_str())
        .collect::<Vec<_>>();
    require(
        names == ["containerize", "encode", "verify"],
        format!("C1 phase names: {names:?}"),
    )?;
    let files = array(&execute["data"]["file_phases"], "C1 file phases")?;
    require(files.len() == 3, format!("C1 file phase count: {execute}"))?;
    require(
        files[0]["outcome"] == "committed" && files[1]["outcome"] == "committed",
        format!("C1 mutation outcomes: {execute}"),
    )?;
    require(
        files[2]["outcome"] == "verified",
        format!("C1 verification outcome: {execute}"),
    )?;
    let evidence = array(
        &execute["data"]["artifact_verifications"],
        "C1 verification evidence",
    )?;
    require(evidence.len() == 1, format!("C1 evidence count: {execute}"))?;
    require(
        evidence[0]["status"] == "succeeded"
            && evidence[0]["expected_checksum"] == evidence[0]["observed_checksum"]
            && evidence[0]["expected_size_bytes"] == evidence[0]["observed_size_bytes"],
        format!("C1 verification evidence mismatch: {execute}"),
    )
}

fn assert_stored_matches(execute: &Value, stored: &Value) -> io::Result<()> {
    for field in ["summary", "phases", "file_phases", "artifact_verifications"] {
        require(
            execute["data"][field] == stored["data"][field],
            format!("stored report differs at {field}"),
        )?;
    }
    Ok(())
}

fn assert_core_inspections(run: &ScenarioRun, execute: &Value, job_id: u64) -> io::Result<()> {
    let job = run.ok(
        &["job", "show", "--job-id", &job_id.to_string()],
        "job",
        "job show",
    )?;
    require(
        job["data"]["job"]["state"] == "succeeded",
        format!("C1 job: {job}"),
    )?;
    let tickets = run.ok(&["ticket", "list"], "ticket", "ticket list")?;
    let tickets = array(&tickets["data"]["tickets"], "C1 tickets")?;
    let job_tickets = tickets
        .iter()
        .filter(|ticket| ticket["job_id"] == job_id)
        .collect::<Vec<_>>();
    require(
        !job_tickets.is_empty()
            && job_tickets
                .iter()
                .all(|ticket| ticket["state"] == "succeeded"),
        "C1 durable tickets must all succeed",
    )?;
    let handle_id = number(
        &execute["data"]["artifact_verifications"][0]["artifact_handle_id"],
        "C1 artifact handle id",
    )?;
    let artifact = run.ok(
        &[
            "artifact",
            "show",
            "--artifact-handle-id",
            &handle_id.to_string(),
        ],
        "artifact.show",
        "artifact show",
    )?;
    require(
        artifact["data"]["artifact"]["state"] == "committed"
            && artifact["data"]["artifact"]["latest_verification"]["status"] == "succeeded",
        format!("C1 artifact inspection: {artifact}"),
    )?;
    let events = run.ok(
        &["event", "list", "--kind", "artifact.verification_succeeded"],
        "event",
        "verification event list",
    )?;
    let verification_id = execute["data"]["artifact_verifications"][0]["verification_id"].clone();
    require(
        array(&events["data"]["events"], "C1 verification events")?
            .iter()
            .any(|event| event["payload"]["verification_id"] == verification_id),
        format!("C1 verification event: {events}"),
    )
}

fn assert_core_output(output_dir: &Path) -> io::Result<()> {
    let files = files_under(output_dir)?;
    require(files.len() == 1, format!("C1 output files: {files:?}"))?;
    let probe = ffprobe(&files[0])?;
    let formats = probe["format"]["format_name"].as_str().unwrap_or_default();
    require(
        formats.split(',').any(|format| format == "matroska"),
        format!("C1 output container: {formats}"),
    )?;
    let video = probe["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|stream| stream["codec_type"] == "video")
        .ok_or_else(|| io::Error::other("C1 output has no video stream"))?;
    require(
        video["codec_name"] == "hevc",
        format!("C1 output video: {video}"),
    )
}

fn run_cli(url: &str, args: &[&str]) -> io::Result<BoundedOutput> {
    run_bounded(
        Command::new(env!("CARGO_BIN_EXE_voom"))
            .args(["--database-url", url])
            .args(args),
        PROCESS_TIMEOUT,
    )
}

fn assert_ok_envelope(output: &BoundedOutput, command: &str, what: &str) -> io::Result<Value> {
    if output.timed_out || !output.status.success() {
        return Err(io::Error::other(output.diagnostics(what)));
    }
    let json: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        io::Error::other(format!(
            "{what} stdout is not one JSON envelope: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        ))
    })?;
    require(
        json["command"] == command,
        format!("{what} command: {json}"),
    )?;
    require(json["status"] == "ok", format!("{what} status: {json}"))?;
    require(
        json["warnings"].as_array().is_some_and(Vec::is_empty),
        format!("{what} envelope warnings: {json}"),
    )?;
    Ok(json)
}

fn assert_retired(envelope: &Value, worker_id: u64, kind: &str) -> io::Result<()> {
    require(
        envelope["command"] == "worker"
            && envelope["status"] == "ok"
            && envelope["data"]["status"] == "retired"
            && envelope["data"]["worker_id"] == worker_id,
        format!("{kind} retirement envelope: {envelope}"),
    )
}

fn policy_fixture(file_name: &str) -> io::Result<PathBuf> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = crate_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("voom-cli crate has no workspace root"))?;
    Ok(workspace
        .join("crates/voom-control-plane/tests/fixtures/policies")
        .join(file_name))
}

fn files_under(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn array<'a>(value: &'a Value, what: &str) -> io::Result<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| io::Error::other(format!("{what} is not an array: {value}")))
}

fn number(value: &Value, what: &str) -> io::Result<u64> {
    value
        .as_u64()
        .ok_or_else(|| io::Error::other(format!("{what} is not a number: {value}")))
}

fn require(condition: bool, message: impl std::fmt::Display) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.to_string()))
    }
}
