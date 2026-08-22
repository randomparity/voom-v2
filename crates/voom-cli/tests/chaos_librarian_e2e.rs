#![expect(
    clippy::unwrap_used,
    reason = "E2E tests fail loudly and preserve paths for diagnosis"
)]

mod support;

use std::path::Path;

use support::chaos_librarian::{ChaosLibrarian, ChaosRun};
use support::observed_state::{
    export_observed_state, library_relative_path, sha256_to_observed_hash,
};
use support::policy_seed::seed_transcode_policy;
use support::voom_cli::{VoomTestDb, run_voom};
use voom_test_support::scan_seed::{SeedFile, SeededSource, seed_scanned_files};

struct SeededChaosRun {
    run: ChaosRun,
    db: VoomTestDb,
    seeded: Vec<SeededSource>,
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn chaos_librarian_submodule_is_pinned_and_ready() {
    let chaos = ChaosLibrarian::discover().unwrap();
    let readiness = chaos.validate_ready().unwrap();

    assert_eq!(
        readiness.revision,
        "9f4c3bf7b7908484ad179d288dd59f3f85185053"
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_static"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_filesystem_mutations"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_media_mutations"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_hevc_video"]
            .as_bool()
            .unwrap_or(false)
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn voom_e2e_support_runs_version_envelope() {
    let db = VoomTestDb::init().await.unwrap();
    let version = run_voom(&db.url, ["version"]).unwrap();

    assert_eq!(version.status_code, Some(0));
    assert_eq!(version.json["command"], "version");
    assert_eq!(version.json["status"], "ok");
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn observed_state_rejects_paths_outside_library() {
    let tmp = tempfile::tempdir().unwrap();
    let library = tmp.path().join("chaos-run/library");
    let outside_dir = tmp.path().join("other");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();
    let outside = outside_dir.join("Movie.mkv");
    std::fs::write(&outside, b"not real media").unwrap();

    let err = library_relative_path(&library.canonicalize().unwrap(), &outside).unwrap_err();

    assert!(err.to_string().contains("outside library root"));
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn observed_state_hash_uses_chaos_librarian_prefix() {
    let hash = sha256_to_observed_hash(
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap();

    assert_eq!(
        hash,
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}

#[test]
fn observed_state_uses_stream_language_from_snapshot() {
    let stream = serde_json::json!({
        "kind": "audio",
        "codec_name": "aac",
        "language": "und"
    });

    let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();

    assert_eq!(observed["language"], "und");
}

#[test]
fn observed_state_does_not_infer_mp4_language_when_snapshot_omits_it() {
    let stream = serde_json::json!({
        "kind": "audio",
        "codec_name": "aac"
    });

    let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();

    assert!(observed.get("language").is_none());
}

#[test]
fn chaos_run_scan_root_uses_fixture_library_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let run = support::chaos_librarian::ChaosRun {
        _tmp: tmp,
        run_dir: std::path::PathBuf::from("/tmp/voom-chaos/run"),
        report: serde_json::json!({"materialized": []}),
    };

    assert_eq!(
        run.scan_root(),
        std::path::Path::new("/tmp/voom-chaos/run/library")
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn static_library_baseline_seeds_exports_and_compares() {
    let chaos = ready_chaos();
    let SeededChaosRun { run, db, seeded } =
        seed_materialized_scenario(&chaos, &chaos.upstream_scenario("static-library.yaml")).await;
    assert!(
        !seeded.is_empty(),
        "the static-library scenario must materialize media files"
    );

    let observed_path = run.run_dir.join("observed-state.json");
    export_observed_state(
        &db.url,
        &run.run_dir,
        &observed_path,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .unwrap();
    let compare = chaos
        .compare_final_state(&run.run_dir, &observed_path)
        .unwrap();

    assert_eq!(compare["ok"], true);
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn policy_seed_creates_durable_ids_from_seeded_source() {
    let chaos = ready_chaos();
    let SeededChaosRun { db, seeded, .. } = seed_materialized_scenario(
        &chaos,
        &chaos.upstream_scenario("voom-ci/h264-transcode-candidate.yaml"),
    )
    .await;

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy(
        &cp,
        "seed-test",
        "mp4",
        "h264",
        seeded[0].file_version_id,
        Some(seeded[0].media_snapshot_id),
    )
    .await
    .unwrap();

    assert!(ids.policy_version_id > 0);
    assert!(ids.input_set_id > 0);
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_required_executes_real_worker_and_commits_hevc_mkv() {
    let chaos = ready_chaos();
    let SeededChaosRun { run, db, seeded } = seed_materialized_scenario(
        &chaos,
        &chaos.upstream_scenario("voom-ci/h264-transcode-candidate.yaml"),
    )
    .await;

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy(
        &cp,
        "chaos-h264",
        "mp4",
        "h264",
        seeded[0].file_version_id,
        Some(seeded[0].media_snapshot_id),
    )
    .await
    .unwrap();
    let plan = run_voom(
        &db.url,
        [
            "plan",
            "show",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
        ],
    )
    .unwrap();
    assert_eq!(plan.status_code, Some(0), "stderr: {}", plan.stderr);
    assert!(
        plan.json["data"]["plan"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["operation_kind"] == "transcode_video")
    );

    let mut worker = support::voom_cli::TranscodeWorkerLaunch::start(&cp)
        .await
        .unwrap();
    let stage = run.run_dir.join("voom-stage");
    let out = run.run_dir.join("voom-output");
    let execute = run_voom(
        &db.url,
        [
            "compliance",
            "execute",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
            "--staging-root",
            stage.to_str().unwrap(),
            "--output-dir",
            out.to_str().unwrap(),
        ],
    )
    .unwrap();
    worker.shutdown().unwrap();

    assert_eq!(
        execute.status_code,
        Some(0),
        "envelope: {} stderr: {}",
        execute.json,
        execute.stderr
    );
    let transcode_summary = &execute.json["data"]["summary"]["per_operation"]["transcode_video"];
    assert_eq!(transcode_summary["success_count"], 1);
    assert_eq!(transcode_summary["failure_count"], 0);
    let file_phase = &execute.json["data"]["file_phases"][0];
    assert_eq!(file_phase["outcome"], "committed");
    assert!(!file_phase["ticket_ids"].as_array().unwrap().is_empty());
    assert!(file_phase["produced_file_version_id"].as_u64().unwrap() > 0);
    assert!(file_phase["reprobe_snapshot_id"].as_u64().unwrap() > 0);
    let produced = first_file_with_extension(&out, "mkv").unwrap();
    assert!(produced.is_file());
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_noop_does_not_schedule_worker_mutation() {
    let chaos = ready_chaos();

    let SeededChaosRun { db, seeded, .. } =
        seed_materialized_scenario(&chaos, &chaos.upstream_scenario("voom-ci/hevc-noop.yaml"))
            .await;

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy(
        &cp,
        "chaos-hevc",
        "mkv",
        "hevc",
        seeded[0].file_version_id,
        Some(seeded[0].media_snapshot_id),
    )
    .await
    .unwrap();
    let report = run_voom(
        &db.url,
        [
            "compliance",
            "report",
            "--policy-version-id",
            &ids.policy_version_id.to_string(),
            "--input-set-id",
            &ids.input_set_id.to_string(),
        ],
    )
    .unwrap();

    assert_eq!(report.status_code, Some(0), "stderr: {}", report.stderr);
    assert_eq!(report.json["data"]["plan"]["nodes"][0]["status"], "no_op");
    assert_eq!(
        report.json["data"]["report"]["summary"]["noncompliant_check_count"],
        0
    );
    assert_eq!(
        report.json["data"]["report"]["summary"]["executable_check_count"],
        0
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn step_mutation_rescan_rejects_changed_bytes_at_live_rooted_address() {
    let chaos = ready_chaos();

    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    let child = chaos
        .run_for_duration(
            &chaos.upstream_scenario("reencode-video.yaml"),
            &run_dir,
            "3s",
            "1x",
        )
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run_dir.join("library");
    wait_for_file_with_extension(&library_path, "mkv");
    let root_id = db.configure_local_root(&library_path).await.unwrap();

    // The first seeding publishes the original bytes' identity at the live
    // rooted address.
    let cp = db.control_plane().await.unwrap();
    let (mut files, mut locators) = (Vec::new(), Vec::new());
    let seeds = media_seeds_for_library(&library_path, &mut files, &mut locators);
    seed_scanned_files(&cp, &db.url, root_id, &seeds)
        .await
        .unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "chaos-librarian run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // A rescan that observes different bytes at the same address must refuse
    // to overwrite the recorded identity during completion.
    let conflict = seed_scanned_files(&cp, &db.url, root_id, &seeds).await;
    let error = conflict
        .err()
        .unwrap_or_else(|| unreachable!("rescanning changed bytes must conflict"));
    assert!(
        error
            .to_string()
            .contains("already records different bytes"),
        "conflict must name the byte mismatch: {error}"
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn malformed_media_scan_request_stays_accepted_without_worker_side_effects() {
    let chaos = ready_chaos();
    let run = chaos
        .materialize(&chaos.upstream_scenario("malformed-container-header.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let root_id = db
        .configure_local_root(&run.scan_root())
        .await
        .unwrap()
        .0
        .to_string();

    // The CLI only requests the durable session; probing (and any per-file
    // probe failure on unprobeable media, #213) belongs to owner-node workers.
    let request = run_voom(&db.url, ["scan", "--root", root_id.as_str(), "--no-wait"]).unwrap();
    assert_eq!(request.status_code, Some(0), "stderr: {}", request.stderr);
    assert!(request.json["data"]["scan_session_id"].as_u64().unwrap() > 0);
    assert!(request.json["data"]["ticket_id"].as_u64().unwrap() > 0);

    let pool = voom_store::connect(&db.url).await.unwrap();
    let ticket_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        ticket_count, 1,
        "only the scan-request ticket exists; no execution ticket"
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn hardlinked_paths_resolve_to_one_physical_file() {
    let chaos = ready_chaos();
    let SeededChaosRun { seeded, .. } = seed_materialized_scenario(
        &chaos,
        &chaos.upstream_recipe("scanner/hardlink-duplicates.yaml"),
    )
    .await;

    // #249: the seeder records (dev, ino) inode facts from disk, so two
    // hardlinked paths resolve through publication to one physical file — one
    // asset/version with the second path as an added location. A byte-identical
    // copy (distinct inode) would remain a separate asset.
    assert_eq!(seeded.len(), 2);
    assert_eq!(seeded[0].file_version_id, seeded[1].file_version_id);
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn symlinked_media_scan_request_is_accepted() {
    let chaos = ready_chaos();
    let run = chaos
        .materialize(&chaos.upstream_recipe("scanner/symlink-external.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let root_id = db
        .configure_local_root(&run.scan_root())
        .await
        .unwrap()
        .0
        .to_string();

    // The scanner's skip-symlinks policy is worker-side now; the CLI contract
    // is that a request for the root is accepted durably.
    let request = run_voom(&db.url, ["scan", "--root", root_id.as_str(), "--no-wait"]).unwrap();
    assert_eq!(request.status_code, Some(0), "stderr: {}", request.stderr);
    assert_eq!(request.json["status"], "ok");
    assert!(request.json["data"]["scan_session_id"].as_u64().unwrap() > 0);
}

fn first_file_with_extension(dir: &std::path::Path, extension: &str) -> Option<std::path::PathBuf> {
    let mut entries = std::fs::read_dir(dir)
        .ok()?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if let Some(found) = first_file_with_extension(&path, extension) {
                return Some(found);
            }
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            return Some(path);
        }
    }
    None
}

fn wait_for_file_with_extension(dir: &std::path::Path, extension: &str) {
    let started = std::time::Instant::now();
    loop {
        if first_file_with_extension(dir, extension).is_some() {
            return;
        }
        assert!(
            started.elapsed() <= std::time::Duration::from_secs(10),
            "timed out waiting for .{extension} under {}",
            dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn ready_chaos() -> ChaosLibrarian {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    chaos
}

/// Media extensions the chaos scenarios materialize.
const MEDIA_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "mov", "avi", "webm", "m4v"];

fn is_media_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| MEDIA_EXTENSIONS.contains(&extension))
}

fn collect_media_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect_media_files(&entry, out);
        } else if is_media_file(&entry) {
            out.push(entry);
        }
    }
}

/// Canned normalized probe snapshot for a media file; the codec follows the
/// container family the scenario materialized (mp4-family h264, mkv-family
/// hevc), matching what each downstream policy assertion assumes.
fn probe_for_extension(path: &Path) -> serde_json::Value {
    let extension = path.extension().and_then(|value| value.to_str());
    let codec = match extension {
        Some("mkv" | "webm") => "hevc",
        _ => "h264",
    };
    serde_json::json!({
        "format": "sprint10-v1",
        "container": { "format_name": "application/octet-stream" },
        "streams": [{
            "index": 0,
            "kind": "video",
            "codec_name": codec,
            "width": 320,
            "height": 180,
        }],
    })
}

/// Build `SeedFile`s for every media file under the materialized library.
fn media_seeds_for_library<'a>(
    library_path: &Path,
    files: &'a mut Vec<std::path::PathBuf>,
    locators: &'a mut Vec<String>,
) -> Vec<SeedFile<'a>> {
    files.clear();
    collect_media_files(library_path, files);
    assert!(
        !files.is_empty(),
        "the scenario must materialize media files under {}",
        library_path.display()
    );
    *locators = files
        .iter()
        .map(|path| {
            path.strip_prefix(library_path)
                .unwrap()
                .to_str()
                .unwrap()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .collect();
    files
        .iter()
        .zip(locators.iter())
        .map(|(path, locator)| SeedFile {
            locator,
            path,
            probe_snapshot: probe_for_extension(path),
        })
        .collect()
}

/// Materialize a chaos scenario and publish every media file's identity rows
/// through the real request/start/batch/complete scan-session chain. The CLI
/// no longer scans in-process (ADR 0077), so flows seed through the seeder
/// instead of parsing a scanned-file envelope.
async fn seed_materialized_scenario(chaos: &ChaosLibrarian, scenario: &Path) -> SeededChaosRun {
    let run = chaos.materialize(scenario).unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.scan_root();
    let root_id = db.configure_local_root(&library_path).await.unwrap();
    let mut files = Vec::new();
    let mut locators = Vec::new();
    let seeds = media_seeds_for_library(&library_path, &mut files, &mut locators);
    let cp = db.control_plane().await.unwrap();
    let seeded = seed_scanned_files(&cp, &db.url, root_id, &seeds)
        .await
        .unwrap();
    SeededChaosRun { run, db, seeded }
}
