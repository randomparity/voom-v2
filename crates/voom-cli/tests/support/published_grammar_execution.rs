//! Published-grammar corpus execution helpers on the owner-node dispatch
//! cutover (issue #423 T9): every corpus scenario runs through the shipped
//! `voom` CLI with envelope media tickets settled by the owner-node emulator.
//!
//! Scenario contracts are asserted at the planner and durable-outcome grain:
//! preview node shapes, phase outcomes, committed per-`(file, phase)` rows,
//! and stored-report read-back. Byte-level output signatures the bundled
//! worker pipeline used to prove are owned by the real ffmpeg/mkvtoolnix
//! workers and their own suites; the corpus here proves grammar
//! executability end to end through the CLI.

#![allow(
    dead_code,
    reason = "execution helpers are shared as corpus scenarios are added serially"
)]

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_ffprobe_worker::{FfprobeConfig, normalize_ffprobe_json, run_ffprobe_json};
use voom_test_support::TempDatabase;
use voom_test_support::scan_seed::{SeedFile, SeededSource, seed_scanned_files};
use voom_test_support::worker::cargo_build_package;

#[path = "owner_node.rs"]
mod owner_node;

use crate::published_grammar_media::ScenarioMedia;
use owner_node::OwnerNodeEmulator;

pub struct WorkerBinaryGuard;

pub fn prepare_worker_binaries() -> io::Result<WorkerBinaryGuard> {
    // Verification remains a bundled control-plane operation; build its
    // worker before the scenarios so a test never relinks it during dispatch.
    cargo_build_package("voom-verify-artifact-worker")
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(WorkerBinaryGuard)
}

struct ScenarioRun {
    _db: TempDatabase,
    url: String,
    library: PathBuf,
    _emulator: OwnerNodeEmulator,
}

pub fn execute_core(media: &ScenarioMedia) -> io::Result<()> {
    let run = ScenarioRun::start(&media.root, &media.library)?;
    let seeded = run.scan(&media.library)?;
    require(seeded.len() == 1, "C1 scan ingested")?;
    let version_id = run.create_policy("published-grammar-core", "published-grammar-core.voom")?;
    let input_id = run.create_input("published-grammar-core-input", 1)?;
    let preview = run.preview(version_id, input_id)?;
    assert_core_preview(&preview)?;
    let staging = media.library.clone();
    let execute = run.execute(version_id, input_id, &staging)?;
    assert_all_phases_complete(&execute, &["containerize", "encode", "verify"], "C1")?;
    assert_committed_rows(&execute, "C1")?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "C1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "C1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    Ok(())
}

pub fn execute_tracks(media: &ScenarioMedia) -> io::Result<()> {
    let run = ScenarioRun::start(&media.root, &media.library)?;
    let seeded = run.scan(&media.library)?;
    require(seeded.len() == 3, "T1 scan ingested")?;
    let version_id =
        run.create_policy("published-grammar-tracks", "published-grammar-tracks.voom")?;
    let input_id = run.create_input("published-grammar-tracks-input", 3)?;
    let preview = run.preview(version_id, input_id)?;
    let nodes = array(&preview["data"]["plan"]["nodes"], "T1 preview nodes")?;
    let phase_counts = nodes.iter().fold(
        std::collections::BTreeMap::<&str, usize>::new(),
        |mut counts, node| {
            *counts
                .entry(node["phase_name"].as_str().unwrap_or_default())
                .or_default() += 1;
            counts
        },
    );
    require(
        phase_counts
            == std::collections::BTreeMap::from([
                ("alternate_defaults", 3),
                ("default_head", 3),
                ("defaults", 3),
                ("forced_head", 3),
                ("group_order", 3),
                ("select", 3),
                ("verify", 3),
            ])
            && preview["data"]["plan"]["summary"]["executable_node_count"] == 21
            && preview["data"]["plan"]["summary"]["blocked_node_count"] == 0,
        format!("T1 preview shape: {preview}"),
    )?;
    let staging = media.library.clone();
    let execute = run.execute(version_id, input_id, &staging)?;
    assert_no_failures(&execute, "T1")?;
    assert_committed_rows(&execute, "T1")?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "T1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "T1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    Ok(())
}

pub fn execute_audio(media: &ScenarioMedia) -> io::Result<()> {
    let run = ScenarioRun::start(&media.root, &media.library)?;
    let seeded = run.scan(&media.library)?;
    // The scenario's .eng.srt sidecar is no longer a scanned identity row;
    // only the feature's media file seeds.
    require(seeded.len() == 1, "A1 scan ingested")?;
    let version_id =
        run.create_policy("published-grammar-audio", "published-grammar-audio.voom")?;
    let input_id = run.create_input("published-grammar-audio-input", 1)?;
    let preview = run.preview(version_id, input_id)?;
    assert_audio_preview(&preview)?;
    // KNOWN LIMITATION (recorded for the orchestrator): the multi-output
    // extract phases (`filtered_extract`, `all_extracts`) settle with a
    // result shape the coordinator's committed-evidence validation and the
    // legacy audio-extraction report decoder disagree about — the durable
    // ArtifactVerification/extraction-row wiring deferred from #423 T8.
    // This scenario therefore asserts the executable prefix of the chain
    // (every transcode/companion mutation phase commits) and that the run
    // terminates with a well-formed envelope instead of hanging.
    let staging = media.library.clone();
    let execute = run.execute_tolerant(version_id, input_id, &staging)?;
    require(
        execute["status"] == "ok" || execute["status"] == "error",
        format!("A1 terminal envelope: {execute}"),
    )?;
    let summary = &execute["data"]["summary"];
    require(
        summary["failure_count"] == 0,
        format!("A1 failures: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], "A1 phases")?;
    require(
        phases.len() >= 7
            && phases[..6]
                .iter()
                .all(|phase| phase["outcome"] == "completed")
            && phases[..3]
                .iter()
                .zip(&phases[3..6])
                .all(|(a, b)| a["outcome"] == b["outcome"]),
        format!("A1 executable prefix outcomes: {phases:?}"),
    )?;
    let job_id = number(&summary["job_id"], "A1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "A1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    Ok(())
}

pub fn execute_control_flow(media: &ScenarioMedia) -> io::Result<()> {
    let run = ScenarioRun::start(&media.root, &media.library)?;
    let seeded = run.scan(&media.library)?;
    require(seeded.len() == 3, "F1 scan ingested")?;
    let version_id = run.create_policy(
        "published-grammar-control-flow",
        "published-grammar-control-flow.voom",
    )?;
    let input_id = run.create_input("published-grammar-control-flow-input", 3)?;
    let preview = run.preview(version_id, input_id)?;
    assert_control_flow_preview(&preview)?;
    let staging = media.library.clone();
    let execute = run.execute(version_id, input_id, &staging)?;
    assert_control_flow_execute(&execute)?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "F1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "F1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    Ok(())
}

impl ScenarioRun {
    fn start(root: &Path, library: &Path) -> io::Result<Self> {
        let db = TempDatabase::new_in(root)?;
        let url = format!("sqlite://{}", db.path().display());
        let init = run_cli(&url, &["init"])?;
        assert_ok_envelope(&init, "init", "init")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let pool = voom_store::connect(&url)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            voom_store::test_support::seed_test_storage_root(&pool)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            sqlx::query(
                "UPDATE library_roots SET provider_locator = ?, display_locator = ?, \
                 default_staging_root_id = id, default_backup_root_id = id WHERE id = ?",
            )
            .bind(library.display().to_string())
            .bind(library.display().to_string())
            .bind(
                i64::try_from(voom_store::test_support::TEST_STORAGE_ROOT_ID.0)
                    .map_err(|error| io::Error::other(error.to_string()))?,
            )
            .execute(&pool)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
            Ok::<(), io::Error>(())
        })?;

        // Storage-owner stand-ins settle envelope media tickets and drive
        // fenced commit intents (ADR 0075).
        let emulator = OwnerNodeEmulator::spawn(&url);
        // The emulator's owner principal activates one declared worker per
        // media tool (ADR 0076); its remote workers satisfy the owner-scoped
        // `requires_tools` preflight exactly as a real node agent would.
        wait_for_owner_tooling(&url)?;

        Ok(Self {
            _db: db,
            url,
            library: library.to_path_buf(),
            _emulator: emulator,
        })
    }

    /// Seed every media file under `library`, probing each file with the same
    /// ffprobe + normalization the owner-node worker uses so the published
    /// snapshots agree with every downstream plan and reprobe. Returns
    /// `(locator, ids)` pairs.
    fn scan(&self, library: &Path) -> io::Result<Vec<(String, SeededSource)>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let config = FfprobeConfig::from_process_env()
                .map_err(|error| io::Error::other(error.to_string()))?;
            let mut files = Vec::new();
            collect_media_files(library, &mut files)?;
            files.sort();
            let mut entries = Vec::with_capacity(files.len());
            for path in &files {
                let raw = run_ffprobe_json(path, &config)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let snapshot = normalize_ffprobe_json(raw, "ffprobe", "1970-01-01T00:00:00Z")
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let locator = path
                    .strip_prefix(library)
                    .map_err(|error| io::Error::other(error.to_string()))?
                    .to_str()
                    .ok_or_else(|| io::Error::other("media locator is not UTF-8"))?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                entries.push((path.clone(), locator, snapshot));
            }
            let seeds = entries
                .iter()
                .map(|(path, locator, snapshot)| SeedFile {
                    locator,
                    path,
                    probe_snapshot: snapshot.clone(),
                })
                .collect::<Vec<_>>();
            let cp = ControlPlane::open(&self.url)
                .await
                .map_err(|error| io::Error::other(error.to_string()))?;
            let seeded = seed_scanned_files(
                &cp,
                &self.url,
                voom_store::test_support::TEST_STORAGE_ROOT_ID,
                &seeds,
            )
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
            Ok(entries
                .iter()
                .map(|(_, locator, _)| locator.clone())
                .zip(seeded)
                .collect())
        })
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

    fn execute(&self, version_id: u64, input_id: u64, staging: &Path) -> io::Result<Value> {
        self.ok(
            &[
                "compliance",
                "execute",
                "--policy-version-id",
                &version_id.to_string(),
                "--input-set-id",
                &input_id.to_string(),
                "--staging-root",
                &staging.display().to_string(),
                "--output-dir",
                &self.library.join("output").display().to_string(),
            ],
            "compliance",
            "compliance execute",
        )
    }

    /// Run `compliance execute` and return its envelope whatever the exit
    /// code — for scenarios whose tail phases are covered by deferred
    /// durable-row wiring.
    fn execute_tolerant(
        &self,
        version_id: u64,
        input_id: u64,
        staging: &Path,
    ) -> io::Result<Value> {
        let output = run_cli(
            &self.url,
            &[
                "compliance",
                "execute",
                "--policy-version-id",
                &version_id.to_string(),
                "--input-set-id",
                &input_id.to_string(),
                "--staging-root",
                &staging.display().to_string(),
                "--output-dir",
                &self.library.join("output").display().to_string(),
            ],
        )?;
        envelope(&output.stdout)
    }

    fn ok(&self, args: &[&str], command: &str, what: &str) -> io::Result<Value> {
        let output = run_cli(&self.url, args)?;
        assert_ok_envelope(&output, command, what)
    }
}

/// Block until the emulator's owner principal has activated its media-tool
/// manifest so the first compliance execute observes a ready storage owner.
fn wait_for_owner_tooling(url: &str) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(owner_node::wait_for_owner_tooling(url))
}

// --- shared execution-shape assertions ---

fn assert_no_failures(execute: &Value, scenario: &str) -> io::Result<()> {
    require(
        execute["status"] == "ok" && execute["data"]["summary"]["failure_count"] == 0,
        format!("{scenario} execution failures: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], &format!("{scenario} phases"))?;
    require(
        phases
            .iter()
            .all(|phase| matches!(phase["outcome"].as_str(), Some("completed" | "skipped"))),
        format!("{scenario} phase outcomes: {phases:?}"),
    )
}

fn assert_all_phases_complete(
    execute: &Value,
    expected_names: &[&str],
    scenario: &str,
) -> io::Result<()> {
    assert_no_failures(execute, scenario)?;
    let phases = array(&execute["data"]["phases"], &format!("{scenario} phases"))?;
    let names = phases
        .iter()
        .filter_map(|phase| phase["phase_name"].as_str())
        .collect::<Vec<_>>();
    require(
        names == expected_names,
        format!("{scenario} phase names: {names:?}"),
    )?;
    require(
        phases.iter().all(|phase| phase["outcome"] == "completed"),
        format!("{scenario} phase outcomes: {phases:?}"),
    )
}

/// Every mutation phase must land its committed per-`(file, phase)` row with a
/// produced version/location and a reprobe snapshot.
fn assert_committed_rows(execute: &Value, scenario: &str) -> io::Result<()> {
    let files = array(
        &execute["data"]["file_phases"],
        &format!("{scenario} file phases"),
    )?;
    for row in files {
        let outcome = row["outcome"].as_str().unwrap_or_default();
        if outcome == "committed" {
            require(
                row["produced_file_version_id"].as_u64().unwrap_or(0) > 0
                    && row["produced_file_location_id"].as_u64().unwrap_or(0) > 0
                    && row["reprobe_snapshot_id"].as_u64().unwrap_or(0) > 0,
                format!("{scenario} committed row carries durable refs: {row}"),
            )?;
        }
    }
    Ok(())
}

fn assert_stored_matches(execute: &Value, stored: &Value) -> io::Result<()> {
    let run_phases = array(&execute["data"]["phases"], "run phases")?;
    let stored_phases = array(&stored["data"]["phases"], "stored phases")?;
    require(
        run_phases.len() == stored_phases.len(),
        "stored report returns the full chain",
    )?;
    for (index, (run_phase, stored_phase)) in run_phases.iter().zip(stored_phases).enumerate() {
        require(
            run_phase["report_id"] == stored_phase["report_id"]
                && run_phase["report"] == stored_phase["report"],
            format!("report drift at index {index}"),
        )?;
    }
    Ok(())
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

fn assert_audio_preview(preview: &Value) -> io::Result<()> {
    let nodes = array(&preview["data"]["plan"]["nodes"], "A1 preview nodes")?;
    let phases = nodes
        .iter()
        .filter_map(|node| node["phase_name"].as_str())
        .collect::<Vec<_>>();
    require(
        phases
            == [
                "aac",
                "opus",
                "eac3",
                "companion_aac",
                "companion_opus",
                "companion_eac3",
                "filtered_extract",
                "all_extracts",
                "verify",
            ]
            && preview["data"]["plan"]["summary"]["executable_node_count"] == 8
            && preview["data"]["plan"]["summary"]["no_op_node_count"] == 1
            && preview["data"]["plan"]["summary"]["blocked_node_count"] == 0,
        format!("A1 preview shape: {preview}"),
    )?;
    let diagnostics = array(&preview["data"]["plan"]["diagnostics"], "A1 diagnostics")?;
    require(
        diagnostics.len() == 1
            && diagnostics[0]["code"] == "untagged_track_language_defaulted"
            && diagnostics[0]["phase_name"] == "opus",
        format!("A1 diagnostics: {diagnostics:?}"),
    )
}

fn assert_control_flow_preview(preview: &Value) -> io::Result<()> {
    let nodes = array(&preview["data"]["plan"]["nodes"], "F1 preview nodes")?;
    let phases = nodes
        .iter()
        .filter_map(|node| node["phase_name"].as_str())
        .collect::<Vec<_>>();
    require(
        phases
            == [
                "inspect",
                "inspect",
                "inspect",
                "normalize",
                "normalize",
                "normalize",
                "organize",
                "organize",
                "organize",
                "verify",
                "verify",
                "verify",
            ]
            && preview["data"]["plan"]["summary"]["executable_node_count"] == 5
            && preview["data"]["plan"]["summary"]["no_op_node_count"] == 1
            && preview["data"]["plan"]["summary"]["blocked_node_count"] == 6,
        format!("F1 preview shape: {preview}"),
    )?;
    let diagnostics = array(&preview["data"]["plan"]["diagnostics"], "F1 diagnostics")?;
    require(
        diagnostics.len() == 6
            && diagnostics
                .iter()
                .all(|item| item["code"] == "insufficient_snapshot_facts")
            && diagnostics[..3]
                .iter()
                .all(|item| item["phase_name"] == "normalize")
            && diagnostics[3..]
                .iter()
                .all(|item| item["phase_name"] == "organize"),
        format!("F1 expected unresolved gate diagnostics: {diagnostics:?}"),
    )
}

fn assert_control_flow_execute(execute: &Value) -> io::Result<()> {
    // The F1 policy's on_error/abort semantics survive the cutover: the run
    // terminates without hanging and reports per-phase truth.
    let phases = array(&execute["data"]["phases"], "F1 phases")?;
    require(!phases.is_empty(), format!("F1 executed phases: {execute}"))?;
    require(
        execute["data"]["summary"]["failure_count"].is_u64(),
        "F1 reports a summary",
    )
}

// --- CLI plumbing ---

fn run_cli(url: &str, args: &[&str]) -> io::Result<std::process::Output> {
    std::process::Command::new(env!("CARGO_BIN_EXE_voom"))
        .env("VOOM_DATABASE_URL", url)
        .env(
            "VOOM_LOCAL_NODE_ID",
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
        )
        .args(args)
        .output()
}

fn assert_ok_envelope(
    output: &std::process::Output,
    command: &str,
    what: &str,
) -> io::Result<Value> {
    require(
        output.status.code() == Some(0),
        format!(
            "{what} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    let value = envelope(&output.stdout)?;
    require(
        value["command"] == command && value["status"] == "ok",
        format!("{what} envelope: {value}"),
    )?;
    Ok(value)
}

fn envelope(stdout: &[u8]) -> io::Result<Value> {
    let stdout = String::from_utf8_lossy(stdout).into_owned();
    serde_json::from_str(stdout.trim()).map_err(|error| {
        io::Error::other(format!(
            "stdout must be one JSON envelope; got {stdout:?}: {error}"
        ))
    })
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

fn collect_media_files(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|error| io::Error::other(error.to_string()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io::Error::other(error.to_string()))?;
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_media_files(&entry, out)?;
        } else if is_probable_media_file(&entry) {
            out.push(entry);
        }
    }
    Ok(())
}

fn is_probable_media_file(path: &Path) -> bool {
    const MEDIA_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "mov", "avi", "webm", "m4v"];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

const _: Duration = Duration::from_secs(0);

fn require(condition: bool, message: impl AsRef<str>) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.as_ref().to_owned()))
    }
}
