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
use voom_test_support::TempDatabase;
use voom_test_support::worker::hide_stale_fake_ffprobe_sibling;

use crate::local_worker::LocalWorker;
use crate::media_inspect::{assert_stream_tone, ffprobe, mkvmerge_identify};
use crate::process::{BoundedOutput, build_worker_package, run_bounded};
use crate::published_grammar_media::{
    ALL_TONES_HZ, COMMENTARY_TONE_HZ, ENGLISH_TONE_HZ, SURROUND_TONE_HZ, ScenarioMedia,
    UNTAGGED_TONE_HZ,
};

const BUILD_TIMEOUT: Duration = Duration::from_mins(5);
const PROCESS_TIMEOUT: Duration = Duration::from_mins(2);
const READY_TIMEOUT: Duration = Duration::from_mins(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

pub struct WorkerBinaryGuard {
    _ffprobe: voom_test_support::worker::FfprobeSiblingGuard,
}

struct ScenarioRun {
    _db: TempDatabase,
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
    let mut run = ScenarioRun::start(&media.root, &media.library)?;
    let scan = run.scan(&media.library)?;
    require(scan["data"]["summary"]["ingested"] == 1, "C1 scan ingested")?;
    require(scan["data"]["summary"]["failed"] == 0, "C1 scan failures")?;
    let version_id = run.create_policy("published-grammar-core", "published-grammar-core.voom")?;
    let input_id = run.create_input("published-grammar-core-input", 1)?;
    let preview = run.preview(version_id, input_id)?;
    assert_core_preview(&preview)?;
    let staging = media.library.join("stage");
    let output = media.library.join("output");
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

pub fn execute_tracks(media: &ScenarioMedia) -> io::Result<()> {
    let mut run = ScenarioRun::start(&media.root, &media.library)?;
    let scan = run.scan(&media.library)?;
    require(scan["data"]["summary"]["ingested"] == 3, "T1 scan ingested")?;
    require(scan["data"]["summary"]["failed"] == 0, "T1 scan failures")?;
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
    let staging = media.library.join("stage");
    let output = media.library.join("output");
    let execute = run.execute(version_id, input_id, &staging, &output)?;
    assert_tracks_execute(&execute)?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "T1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "T1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    assert_successful_job(&run, job_id, 20, "T1")?;
    assert_tracks_output(&output)?;
    run.shutdown()?;
    Ok(())
}

pub fn execute_audio(media: &ScenarioMedia) -> io::Result<()> {
    let mut run = ScenarioRun::start(&media.root, &media.library)?;
    let scan = run.scan(&media.library)?;
    require(scan["data"]["summary"]["ingested"] == 2, "A1 scan ingested")?;
    require(scan["data"]["summary"]["failed"] == 0, "A1 scan failures")?;
    let version_id =
        run.create_policy("published-grammar-audio", "published-grammar-audio.voom")?;
    let input_id = run.create_input("published-grammar-audio-input", 1)?;
    let preview = run.preview(version_id, input_id)?;
    assert_audio_preview(&preview)?;
    let staging = media.library.join("stage");
    let output = media.library.join("output");
    let execute = run.execute(version_id, input_id, &staging, &output)?;
    assert_audio_execute(&execute)?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "A1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "A1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    assert_audio_inspections(&run, &execute, job_id)?;
    assert_audio_sidecars(&execute, &output)?;
    run.shutdown()?;
    Ok(())
}

pub fn execute_control_flow(media: &ScenarioMedia) -> io::Result<()> {
    let mut run = ScenarioRun::start(&media.root, &media.library)?;
    let scan = run.scan(&media.library)?;
    require(scan["data"]["summary"]["ingested"] == 3, "F1 scan ingested")?;
    require(scan["data"]["summary"]["failed"] == 0, "F1 scan failures")?;
    let fail_version_id = scanned_version_id(&scan, media.file("f1c")?)?;
    let modify_version_id = scanned_version_id(&scan, media.file("f1a")?)?;
    let version_id = run.create_policy(
        "published-grammar-control-flow",
        "published-grammar-control-flow.voom",
    )?;
    let input_id = run.create_input("published-grammar-control-flow-input", 3)?;
    let preview = run.preview(version_id, input_id)?;
    assert_control_flow_preview(&preview)?;
    let staging = media.library.join("stage");
    let output = media.library.join("output");
    let sentinel = staging
        .join(".committed")
        .join("remux")
        .join(format!("v{fail_version_id}"))
        .join("fail.remux.mkv");
    std::fs::create_dir_all(
        sentinel
            .parent()
            .ok_or_else(|| io::Error::other("F1 sentinel has no parent"))?,
    )?;
    let sentinel_bytes = b"published grammar F1 sentinel\n";
    std::fs::write(&sentinel, sentinel_bytes)?;
    let execute = run.execute_error(version_id, input_id, &staging, &output)?;
    require(
        execute["error"]["code"] == "CONFIG_INVALID",
        format!("F1 execution error: {execute}"),
    )?;
    require(
        std::fs::read(&sentinel)? == sentinel_bytes,
        "F1 sentinel bytes changed",
    )?;
    assert_control_flow_execute(&execute)?;
    let job_id = number(&execute["data"]["summary"]["job_id"], "F1 job id")?;
    let stored = run.ok(
        &["compliance", "report", "--job-id", &job_id.to_string()],
        "compliance",
        "F1 stored report",
    )?;
    assert_stored_matches(&execute, &stored)?;
    assert_control_flow_durable_state(&run, &execute, job_id)?;
    assert_control_flow_failure_event(&run, fail_version_id, &sentinel)?;
    assert_control_flow_success_events(&run, &execute, modify_version_id)?;
    run.shutdown()
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
            // Background stand-in for the storage-owner agent (ADR 0074).
            {
                let node = voom_test_support::commit_node::SimulatedOwnerNode::new()
                    .map_err(|error| io::Error::other(error.to_string()))?;
                node.install(&pool)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let driver_cp = voom_control_plane::ControlPlane::open(&url)
                    .await
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let driver_pool = pool.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async move {
                        loop {
                            let pending: Option<(i64, i64)> = sqlx::query_as(
                                "SELECT id, artifact_handle_id FROM artifact_commit_intents WHERE state = 'pending' ORDER BY id ASC LIMIT 1",
                            )
                            .fetch_optional(&driver_pool)
                            .await
                            .unwrap();
                            if let Some((_, handle)) = pending {
                                let _ = node
                                    .drive_pending_commit(
                                        &driver_cp,
                                        &driver_pool,
                                        voom_core::ArtifactHandleId(u64::try_from(handle).unwrap()),
                                    )
                                    .await;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                    });
                });
            }
            sqlx::query(
                "UPDATE library_roots SET provider_locator = ?, display_locator = ? WHERE id = ?",
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

    fn scan(&self, _library: &Path) -> io::Result<Value> {
        self.ok(
            &[
                "scan",
                "--root",
                &voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
            ],
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

    fn execute_error(
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
        let output = run_cli(
            &self.url,
            &[
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
            ],
        )?;
        assert_error_envelope(&output, "compliance", "F1 compliance execute")
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
    require(
        execute["data"]["summary"]["ticket_count"] == 5
            && execute["data"]["summary"]["failure_count"] == 1
            && execute["data"]["summary"]["progress"]["completed"] == 2
            && execute["data"]["summary"]["progress"]["failed"] == 1,
        format!("F1 summary: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], "F1 phases")?;
    let outcomes = phases
        .iter()
        .map(|phase| {
            (
                phase["phase_name"].as_str().unwrap_or_default(),
                phase["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    require(
        outcomes
            == [
                ("inspect", "partially-committed"),
                ("normalize", "partially-committed"),
                ("organize", "skipped"),
                ("verify", "completed"),
            ],
        format!("F1 phase outcomes: {outcomes:?}"),
    )?;
    let rows = array(&execute["data"]["file_phases"], "F1 file phases")?;
    let outcomes = rows
        .iter()
        .map(|row| {
            (
                row["phase_ordinal"].as_u64().unwrap_or(u64::MAX),
                row["branch_id"].as_str().unwrap_or_default(),
                row["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    require(
        outcomes
            == [
                (0, "already-normalized", "skipped"),
                (0, "fail", "blocked"),
                (0, "modify", "committed"),
                (1, "already-normalized", "skipped"),
                (1, "modify", "committed"),
                (2, "already-normalized", "skipped"),
                (2, "modify", "skipped"),
                (3, "already-normalized", "verified"),
                (3, "modify", "verified"),
            ],
        format!("F1 file outcomes: {outcomes:?}"),
    )?;
    let normalize_version = rows
        .iter()
        .find(|row| row["phase_ordinal"] == 1 && row["branch_id"] == "modify")
        .and_then(|row| row["produced_file_version_id"].as_u64())
        .ok_or_else(|| io::Error::other("F1 missing normalize result version"))?;
    let organize_checks = array(&phases[2]["report"]["checks"], "F1 organize gate checks")?;
    require(
        organize_checks.len() == 1
            && organize_checks[0]["target"]["id"] == normalize_version
            && organize_checks[0]["check_status"] == "compliant",
        format!("F1 modified gate did not select only the changed branch: {organize_checks:?}"),
    )?;
    let evidence = array(
        &execute["data"]["artifact_verifications"],
        "F1 verification evidence",
    )?;
    require(
        evidence.len() == 2
            && evidence.iter().all(|row| {
                row["status"] == "succeeded"
                    && row["expected_checksum"] == row["observed_checksum"]
                    && row["expected_size_bytes"] == row["observed_size_bytes"]
            }),
        format!("F1 verification evidence: {evidence:?}"),
    )
}

fn assert_control_flow_durable_state(
    run: &ScenarioRun,
    execute: &Value,
    job_id: u64,
) -> io::Result<()> {
    let job = run.ok(
        &["job", "show", "--job-id", &job_id.to_string()],
        "job",
        "F1 job",
    )?;
    require(
        job["data"]["job"]["state"] == "failed",
        format!("F1 durable job: {job}"),
    )?;
    let tickets = run.ok(&["ticket", "list"], "ticket", "F1 tickets")?;
    let tickets = array(&tickets["data"]["tickets"], "F1 tickets")?
        .iter()
        .filter(|ticket| ticket["job_id"] == job_id)
        .collect::<Vec<_>>();
    require(
        tickets.len() == 5
            && tickets
                .iter()
                .filter(|ticket| ticket["state"] == "failed")
                .count()
                == 1
            && tickets
                .iter()
                .filter(|ticket| ticket["state"] == "succeeded")
                .count()
                == 4,
        format!("F1 durable tickets: {tickets:?}"),
    )?;
    let verification_events = run.ok(
        &["event", "list", "--kind", "artifact.verification_succeeded"],
        "event",
        "F1 verification events",
    )?;
    let expected_ids = array(
        &execute["data"]["artifact_verifications"],
        "F1 verification rows",
    )?
    .iter()
    .map(|row| row["verification_id"].to_string())
    .collect::<BTreeSet<_>>();
    let actual_ids = array(
        &verification_events["data"]["events"],
        "F1 verification events",
    )?
    .iter()
    .map(|event| event["payload"]["verification_id"].to_string())
    .collect::<BTreeSet<_>>();
    require(
        expected_ids.is_subset(&actual_ids),
        format!("F1 verification events: {verification_events}"),
    )
}

fn assert_control_flow_failure_event(
    run: &ScenarioRun,
    fail_version_id: u64,
    sentinel: &Path,
) -> io::Result<()> {
    let events = run.ok(
        &["event", "list", "--kind", "artifact.remux_failed"],
        "event",
        "F1 remux failure",
    )?;
    let events = array(&events["data"]["events"], "F1 remux failure")?;
    require(
        events.len() == 1
            && events[0]["subject_id"] == fail_version_id
            && events[0]["payload"]["source_file_version_id"] == fail_version_id
            && events[0]["payload"]["artifact_handle_id"].is_null()
            && events[0]["payload"]["artifact_location_id"].is_null()
            && events[0]["payload"]["error_code"] == "CONFIG_INVALID"
            && events[0]["payload"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(&sentinel.display().to_string())),
        format!("F1 durable failure event: {events:?}"),
    )
}

fn assert_control_flow_success_events(
    run: &ScenarioRun,
    execute: &Value,
    modify_version_id: u64,
) -> io::Result<()> {
    let rows = array(&execute["data"]["file_phases"], "F1 lineage rows")?;
    let inspect_version = rows
        .iter()
        .find(|row| row["phase_ordinal"] == 0 && row["branch_id"] == "modify")
        .and_then(|row| row["produced_file_version_id"].as_u64())
        .ok_or_else(|| io::Error::other("F1 missing inspect result version"))?;
    let normalize_version = rows
        .iter()
        .find(|row| row["phase_ordinal"] == 1 && row["branch_id"] == "modify")
        .and_then(|row| row["produced_file_version_id"].as_u64())
        .ok_or_else(|| io::Error::other("F1 missing normalize result version"))?;
    require(
        modify_version_id != inspect_version && inspect_version != normalize_version,
        format!(
            "F1 version lineage did not advance: {modify_version_id} -> {inspect_version} -> {normalize_version}"
        ),
    )?;
    for (kind, source_version) in [
        ("artifact.remux_succeeded", modify_version_id),
        ("artifact.transcode_succeeded", inspect_version),
    ] {
        let events = run.ok(
            &["event", "list", "--kind", kind],
            "event",
            "F1 success events",
        )?;
        let events = array(&events["data"]["events"], "F1 success events")?;
        require(
            events.len() == 1
                && events[0]["payload"]["source_file_version_id"] == source_version
                && events[0]["payload"]["artifact_handle_id"]
                    .as_u64()
                    .is_some()
                && events[0]["payload"]["artifact_location_id"]
                    .as_u64()
                    .is_some(),
            format!("F1 {kind}: {events:?}"),
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
    for field in [
        "summary",
        "phases",
        "file_phases",
        "artifact_verifications",
        "audio_extract_outputs",
        "audio_synthesis_companions",
    ] {
        require(
            execute["data"][field] == stored["data"][field],
            format!("stored report differs at {field}"),
        )?;
    }
    Ok(())
}

fn assert_core_inspections(run: &ScenarioRun, execute: &Value, job_id: u64) -> io::Result<()> {
    assert_successful_job(run, job_id, 3, "C1")?;
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

fn assert_successful_job(
    run: &ScenarioRun,
    job_id: u64,
    expected_tickets: usize,
    scenario: &str,
) -> io::Result<()> {
    let job = run.ok(
        &["job", "show", "--job-id", &job_id.to_string()],
        "job",
        "job show",
    )?;
    require(
        job["data"]["job"]["state"] == "succeeded",
        format!("{scenario} job: {job}"),
    )?;
    let tickets = run.ok(&["ticket", "list"], "ticket", "ticket list")?;
    let tickets = array(&tickets["data"]["tickets"], "successful job tickets")?;
    let job_tickets = tickets
        .iter()
        .filter(|ticket| ticket["job_id"] == job_id)
        .collect::<Vec<_>>();
    require(
        job_tickets.len() == expected_tickets
            && job_tickets
                .iter()
                .all(|ticket| ticket["state"] == "succeeded"),
        format!("{scenario} durable tickets: {job_tickets:?}"),
    )
}

fn assert_tracks_execute(execute: &Value) -> io::Result<()> {
    require(
        execute["data"]["summary"]["failure_count"] == 0
            && execute["data"]["summary"]["progress"]["completed"] == 3,
        format!("T1 summary: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], "T1 phases")?;
    let phase_outcomes = phases
        .iter()
        .map(|phase| {
            (
                phase["phase_name"].as_str().unwrap_or_default(),
                phase["outcome"].as_str().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    require(
        phase_outcomes
            == [
                ("select", "completed"),
                ("group_order", "completed"),
                ("default_head", "completed"),
                ("forced_head", "completed"),
                ("defaults", "completed"),
                ("alternate_defaults", "partially-committed"),
                ("verify", "completed"),
            ],
        format!("T1 phase outcomes: {phase_outcomes:?}"),
    )?;
    let file_phases = array(&execute["data"]["file_phases"], "T1 file phases")?;
    require(
        file_phases.len() == 21,
        format!("T1 file phase count: {}", file_phases.len()),
    )?;
    for row in file_phases {
        let phase = number(&row["phase_ordinal"], "T1 phase ordinal")?;
        let branch = row["branch_id"].as_str().unwrap_or_default();
        let expected = match phase {
            5 if branch == "tracks-1920" => "skipped",
            0..=5 => "committed",
            6 => "verified",
            _ => "",
        };
        require(
            row["outcome"] == expected,
            format!("T1 file phase {phase}/{branch}: {row}"),
        )?;
    }
    let evidence = array(
        &execute["data"]["artifact_verifications"],
        "T1 verification evidence",
    )?;
    require(
        evidence.len() == 3
            && evidence.iter().all(|item| {
                item["status"] == "succeeded"
                    && item["expected_checksum"] == item["observed_checksum"]
                    && item["expected_size_bytes"] == item["observed_size_bytes"]
            }),
        format!("T1 verification evidence: {evidence:?}"),
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

fn assert_audio_execute(execute: &Value) -> io::Result<()> {
    require(
        execute["data"]["summary"]["failure_count"] == 0
            && execute["data"]["summary"]["progress"]["completed"] == 1,
        format!("A1 summary: {execute}"),
    )?;
    let phases = array(&execute["data"]["phases"], "A1 phases")?;
    require(
        phases.len() == 9
            && phases[..6]
                .iter()
                .all(|phase| phase["outcome"] == "completed")
            && phases[6..8]
                .iter()
                .all(|phase| phase["outcome"] == "skipped")
            && phases[8]["outcome"] == "completed",
        format!("A1 phase outcomes: {phases:?}"),
    )?;
    let file_phases = array(&execute["data"]["file_phases"], "A1 file phases")?;
    require(
        file_phases.len() == 9
            && file_phases[..6]
                .iter()
                .all(|row| row["outcome"] == "committed")
            && file_phases[6..8]
                .iter()
                .all(|row| row["outcome"] == "skipped")
            && file_phases[8]["outcome"] == "verified",
        format!("A1 file phases: {file_phases:?}"),
    )?;
    assert_audio_synthesis(execute)?;
    assert_audio_extract_lineage(execute)?;
    let evidence = array(
        &execute["data"]["artifact_verifications"],
        "A1 verification evidence",
    )?;
    require(
        evidence.len() == 1
            && evidence[0]["status"] == "succeeded"
            && evidence[0]["expected_checksum"] == evidence[0]["observed_checksum"]
            && evidence[0]["expected_size_bytes"] == evidence[0]["observed_size_bytes"],
        format!("A1 verification evidence: {evidence:?}"),
    )
}

fn assert_audio_synthesis(execute: &Value) -> io::Result<()> {
    let rows = array(
        &execute["data"]["audio_synthesis_companions"],
        "A1 synthesis companions",
    )?;
    require(rows.len() == 3, format!("A1 synthesis count: {rows:?}"))?;
    let codecs = rows
        .iter()
        .filter_map(|row| row["codec"].as_str())
        .collect::<Vec<_>>();
    require(
        codecs == ["aac", "opus", "eac3"]
            && rows
                .iter()
                .all(|row| row["source_snapshot_stream_id"] == "stream-1")
            && rows
                .iter()
                .all(|row| row["source_provider_stream_index"] == 1)
            && rows.iter().all(|row| row["channels"] == 2),
        format!("A1 synthesis facts: {rows:?}"),
    )?;
    for pair in rows.windows(2) {
        require(
            pair[0]["result_file_version_id"] == pair[1]["source_file_version_id"],
            format!("A1 synthesis version chain: {rows:?}"),
        )?;
    }
    let identities = rows
        .iter()
        .map(|row| {
            (
                row["companion_id"].as_str(),
                row["result_file_version_id"].as_u64(),
                row["result_file_location_id"].as_u64(),
                row["result_media_snapshot_id"].as_u64(),
                row["artifact_handle_id"].as_u64(),
            )
        })
        .collect::<BTreeSet<_>>();
    require(
        identities.len() == rows.len(),
        format!("A1 synthesis identities: {rows:?}"),
    )
}

fn assert_audio_extract_lineage(execute: &Value) -> io::Result<()> {
    let rows = array(
        &execute["data"]["audio_extract_outputs"],
        "A1 extraction outputs",
    )?;
    require(rows.len() == 8, format!("A1 extraction count: {rows:?}"))?;
    let expected = [
        ("stream-4", 4_u64, "commentary_audio"),
        ("stream-1", 1, "external_audio"),
        ("stream-2", 2, "external_audio"),
        ("stream-3", 3, "external_audio"),
        ("stream-4", 4, "commentary_audio"),
    ];
    for (row, (stream_id, provider_index, role)) in rows.iter().take(5).zip(expected) {
        require(
            row["source_snapshot_stream_id"] == stream_id
                && row["source_provider_stream_index"] == provider_index
                && row["role"] == role,
            format!("A1 extraction source lineage: {rows:?}"),
        )?;
    }
    for (row, provider_index) in rows.iter().skip(5).zip([5_u64, 6, 7]) {
        require(
            row["source_snapshot_stream_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("synth_companion_"))
                && row["source_provider_stream_index"] == provider_index
                && row["role"] == "external_audio",
            format!("A1 synthesized extraction lineage: {rows:?}"),
        )?;
    }
    for field in [
        "operation_output_id",
        "output_id",
        "staged_artifact_handle_id",
        "staged_artifact_location_id",
        "verification_id",
        "commit_record_id",
        "result_file_version_id",
        "result_file_location_id",
        "result_file_asset_id",
        "result_media_snapshot_id",
        "bundle_member_id",
        "lineage_id",
        "target_path",
    ] {
        let values = rows
            .iter()
            .map(|row| row[field].to_string())
            .collect::<BTreeSet<_>>();
        require(
            values.len() == rows.len(),
            format!("A1 extraction {field} identities: {rows:?}"),
        )?;
    }
    Ok(())
}

fn assert_audio_sidecars(execute: &Value, output_dir: &Path) -> io::Result<()> {
    let rows = array(
        &execute["data"]["audio_extract_outputs"],
        "A1 sidecar outputs",
    )?;
    let published = files_under(output_dir)?;
    let expected = [
        (
            COMMENTARY_TONE_HZ,
            2_u64,
            Some("jpn"),
            Some("Japanese Commentary"),
        ),
        (SURROUND_TONE_HZ, 6, Some("eng"), Some("Surround")),
        (ENGLISH_TONE_HZ, 2, Some("eng"), Some("Main")),
        (UNTAGGED_TONE_HZ, 2, None, Some("Untagged")),
        (
            COMMENTARY_TONE_HZ,
            2,
            Some("jpn"),
            Some("Japanese Commentary"),
        ),
        (SURROUND_TONE_HZ, 2, Some("eng"), Some("Surround")),
        (SURROUND_TONE_HZ, 2, Some("eng"), Some("Surround")),
        (SURROUND_TONE_HZ, 2, Some("eng"), Some("Surround")),
    ];
    for (row, (tone, channels, language, title)) in rows.iter().zip(expected) {
        let target_path = row["target_path"]
            .as_str()
            .ok_or_else(|| io::Error::other(format!("A1 target path: {row}")))?;
        let target_path = Path::new(target_path);
        let file_name = target_path.file_name().ok_or_else(|| {
            io::Error::other(format!("A1 target file name: {}", target_path.display()))
        })?;
        let operation_dir = target_path
            .parent()
            .and_then(Path::file_name)
            .ok_or_else(|| {
                io::Error::other(format!("A1 target operation: {}", target_path.display()))
            })?;
        let matches = published
            .iter()
            .filter(|path| {
                path.file_name() == Some(file_name)
                    && path.parent().and_then(Path::file_name) == Some(operation_dir)
            })
            .collect::<Vec<_>>();
        require(
            matches.len() == 1,
            format!(
                "A1 published sidecar {}: {published:?}",
                file_name.to_string_lossy()
            ),
        )?;
        let path = matches[0];
        let probe = ffprobe(path)?;
        let streams = array(&probe["streams"], "A1 sidecar streams")?;
        require(
            probe["format"]["format_name"]
                .as_str()
                .is_some_and(|format| format.contains("ogg"))
                && streams.len() == 1
                && streams[0]["codec_name"] == "opus"
                && streams[0]["channels"] == channels
                && streams[0]["tags"]["language"].as_str() == language
                && streams[0]["tags"]["title"].as_str() == title,
            format!("A1 sidecar facts for {}: {probe}", path.display()),
        )?;
        assert_stream_tone(path, 0, tone, &ALL_TONES_HZ)?;
    }
    Ok(())
}

fn assert_audio_inspections(run: &ScenarioRun, execute: &Value, job_id: u64) -> io::Result<()> {
    assert_successful_job(run, job_id, 9, "A1")?;
    let events = run.ok(
        &[
            "event",
            "list",
            "--kind",
            "artifact.audio_extract_succeeded",
        ],
        "event",
        "A1 extraction events",
    )?;
    let events = array(&events["data"]["events"], "A1 extraction events")?;
    let counts = events
        .iter()
        .map(|event| event["payload"]["outputs"].as_array().map_or(0, Vec::len))
        .collect::<BTreeSet<_>>();
    let event_output_ids = events
        .iter()
        .flat_map(|event| event["payload"]["outputs"].as_array().into_iter().flatten())
        .map(|output| output["output_id"].to_string())
        .collect::<BTreeSet<_>>();
    let report_output_ids = array(
        &execute["data"]["audio_extract_outputs"],
        "A1 report extraction outputs",
    )?
    .iter()
    .map(|output| output["output_id"].to_string())
    .collect::<BTreeSet<_>>();
    require(
        events.len() == 2
            && counts == BTreeSet::from([1, 7])
            && event_output_ids == report_output_ids,
        format!("A1 durable extraction events: {events:?}"),
    )?;
    assert_verification_inspection(run, execute, "A1")
}

fn assert_verification_inspection(
    run: &ScenarioRun,
    execute: &Value,
    scenario: &str,
) -> io::Result<()> {
    let evidence = &execute["data"]["artifact_verifications"][0];
    let handle_id = number(
        &evidence["artifact_handle_id"],
        "verification artifact handle",
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
        format!("{scenario} verification artifact: {artifact}"),
    )
}

fn assert_tracks_output(output_dir: &Path) -> io::Result<()> {
    let files = files_under(output_dir)?;
    require(files.len() == 3, format!("T1 output files: {files:?}"))?;
    for path in files {
        let identified = mkvmerge_identify(&path)?;
        let tracks = array(&identified["tracks"], "T1 output tracks")?;
        let video = tracks
            .iter()
            .find(|track| track["type"] == "video")
            .ok_or_else(|| {
                io::Error::other(format!("T1 output missing video: {}", path.display()))
            })?;
        let dimensions = video["properties"]["pixel_dimensions"]
            .as_str()
            .unwrap_or_default();
        match dimensions {
            "1920x1080" => assert_track_signatures(
                tracks,
                &[
                    ("subtitles", "eng", "Forced", true, true),
                    ("audio", "eng", "Surround", true, false),
                    ("video", "und", "", false, false),
                    ("audio", "eng", "Main", false, false),
                    ("subtitles", "und", "Untagged", false, false),
                ],
                "T1a",
            )?,
            "1024x576" => assert_track_signatures(
                tracks,
                &[
                    ("subtitles", "eng", "Forced", true, true),
                    ("audio", "eng", "Surround", false, false),
                    ("video", "und", "", false, false),
                    ("audio", "eng", "Main", false, false),
                    ("subtitles", "und", "Untagged", false, false),
                ],
                "T1b",
            )?,
            "512x288" => assert_track_signatures(
                tracks,
                &[
                    ("audio", "eng", "Surround", false, false),
                    ("video", "und", "", false, false),
                    ("audio", "eng", "Main", false, false),
                ],
                "T1c",
            )?,
            _ => {
                return Err(io::Error::other(format!(
                    "unexpected T1 dimensions {dimensions}"
                )));
            }
        }
        let attachments = array(&identified["attachments"], "T1 attachments")?;
        require(
            attachments.len() == 1 && attachments[0]["content_type"] == "font/ttf",
            format!("T1 attachment oracle: {attachments:?}"),
        )?;
    }
    Ok(())
}

fn assert_track_signatures(
    tracks: &[Value],
    expected: &[(&str, &str, &str, bool, bool)],
    scenario: &str,
) -> io::Result<()> {
    let actual = tracks
        .iter()
        .map(|track| {
            (
                track["type"].as_str().unwrap_or_default(),
                track["properties"]["language"].as_str().unwrap_or_default(),
                track["properties"]["track_name"]
                    .as_str()
                    .unwrap_or_default(),
                track["properties"]["default_track"]
                    .as_bool()
                    .unwrap_or(false),
                track["properties"]["forced_track"]
                    .as_bool()
                    .unwrap_or(false),
            )
        })
        .collect::<Vec<_>>();
    require(
        actual == expected,
        format!("{scenario} track signatures: {actual:?}"),
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
            .env(
                "VOOM_LOCAL_NODE_ID",
                voom_store::test_support::TEST_STORAGE_ROOT_ID.0.to_string(),
            )
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

fn assert_error_envelope(output: &BoundedOutput, command: &str, what: &str) -> io::Result<Value> {
    if output.timed_out || output.status.code() != Some(2) {
        return Err(io::Error::other(output.diagnostics(what)));
    }
    let json: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        io::Error::other(format!(
            "{what} stdout is not one JSON envelope: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        ))
    })?;
    require(
        json["command"] == command && json["status"] == "error",
        format!("{what} envelope: {json}"),
    )?;
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

fn scanned_version_id(scan: &Value, path: &Path) -> io::Result<u64> {
    let path = path.display().to_string();
    let file = array(&scan["data"]["files"], "scan files")?
        .iter()
        .find(|file| file["path"] == path)
        .ok_or_else(|| io::Error::other(format!("scan did not report {path}: {scan}")))?;
    number(&file["file_version_id"], "scanned file version id")
}

fn require(condition: bool, message: impl std::fmt::Display) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.to_string()))
    }
}
