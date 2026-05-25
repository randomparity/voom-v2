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
use support::policy_seed::seed_transcode_policy_from_scan;
use support::voom_cli::{VoomOutput, VoomTestDb, run_voom};

struct ScannedChaosRun {
    run: ChaosRun,
    db: VoomTestDb,
    scan: VoomOutput,
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn chaos_librarian_submodule_is_pinned_and_ready() {
    let chaos = ChaosLibrarian::discover().unwrap();
    let readiness = chaos.validate_ready().unwrap();

    assert_eq!(
        readiness.revision,
        "057a4033a3a9ae14fef664ab82f2c31e1a223544"
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_static"]
            .as_bool()
            .unwrap_or(false)
    );
    assert!(
        readiness.capabilities["ready_for"]["materialize_media_mutations"]
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
fn chaos_run_scan_root_follows_materialized_location_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let run = support::chaos_librarian::ChaosRun {
        _tmp: tmp,
        run_dir: std::path::PathBuf::from("/tmp/voom-chaos/run"),
        report: serde_json::json!({
            "materialized": [
                {"location_path": "movies-hd/asset_main.mkv"}
            ]
        }),
    };

    assert_eq!(
        run.scan_root().unwrap(),
        std::path::Path::new("/tmp/voom-chaos/run/movies-hd")
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn static_library_baseline_scans_exports_and_compares() {
    let chaos = ready_chaos();
    let ScannedChaosRun { run, db, scan } =
        scan_materialized_scenario(&chaos, &chaos.upstream_scenario("static-library.yaml")).await;
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);
    assert_eq!(scan.json["status"], "ok");
    assert!(scan.json["data"]["summary"]["ingested"].as_u64().unwrap() > 0);
    assert_eq!(scan.json["data"]["summary"]["failed"], 0);

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
async fn policy_seed_creates_durable_ids_from_scan_envelope() {
    let chaos = ready_chaos();
    let ScannedChaosRun { db, scan, .. } = scan_materialized_scenario(
        &chaos,
        &chaos.voom_scenario("video-transcode-required.yaml"),
    )
    .await;
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "seed-test", "mp4", "h264")
        .await
        .unwrap();

    assert!(ids.policy_version_id > 0);
    assert!(ids.input_set_id > 0);
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_required_executes_real_worker_and_commits_hevc_mkv() {
    let chaos = ready_chaos();
    let ScannedChaosRun { run, db, scan } = scan_materialized_scenario(
        &chaos,
        &chaos.voom_scenario("video-transcode-required.yaml"),
    )
    .await;
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "chaos-h264", "mp4", "h264")
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

    assert_eq!(execute.status_code, Some(0), "stderr: {}", execute.stderr);
    let ticket = execute.json["data"]["tickets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ticket| ticket["operation"] == "transcode_video")
        .unwrap();
    assert_eq!(ticket["state"], "succeeded");
    assert!(
        ticket["result"]["staged_artifact_handle_id"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(ticket["result"]["verification_id"].as_u64().unwrap() > 0);
    assert!(ticket["result"]["commit_record_id"].as_u64().unwrap() > 0);
    let target_path = ticket["result"]["target_path"].as_str().unwrap();
    assert!(std::path::Path::new(target_path).is_file());
    assert!(std::path::Path::new(target_path).starts_with(&out));
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_noop_does_not_schedule_worker_mutation() {
    let chaos = ready_chaos();

    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-noop.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.scan_root().unwrap();
    rewrite_first_mkv_to_hevc(&library_path);
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
    assert_eq!(scan.status_code, Some(0), "stderr: {}", scan.stderr);

    let cp = db.control_plane().await.unwrap();
    let ids = seed_transcode_policy_from_scan(&cp, &scan.json, "chaos-hevc", "mkv", "hevc")
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
async fn step_mutation_rescan_observes_changed_media_facts() {
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
    let library_path = run_dir.clone();
    wait_for_file_with_extension(&library_path, "mkv");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let first = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
    assert_eq!(first.status_code, Some(0), "stderr: {}", first.stderr);

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "chaos-librarian run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let second = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
    assert_eq!(second.status_code, Some(0), "stderr: {}", second.stderr);

    assert!(
        second.json["data"]["summary"]["snapshots_recorded"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_ne!(
        first.json["data"]["files"][0]["content_hash"],
        second.json["data"]["files"][0]["content_hash"]
    );
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn malformed_media_fails_loudly_without_execution_ticket() {
    let chaos = ready_chaos();
    let ScannedChaosRun { db, scan, .. } = scan_materialized_scenario(
        &chaos,
        &chaos.upstream_scenario("malformed-container-header.yaml"),
    )
    .await;

    assert_eq!(scan.status_code, Some(2), "stderr: {}", scan.stderr);
    assert_eq!(scan.json["status"], "error");
    assert_ne!(scan.json["error"]["code"], "INTERNAL");

    let pool = voom_store::connect(&db.url).await.unwrap();
    let ticket_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tickets")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ticket_count, 0);
}

fn rewrite_first_mkv_to_hevc(library_path: &std::path::Path) {
    let path = first_file_with_extension(library_path, "mkv").unwrap();
    let temp = path.with_extension("hevc.tmp.mkv");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            path.to_str().unwrap(),
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
            "-tag:v",
            "hvc1",
            "-c:a",
            "copy",
            temp.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "ffmpeg HEVC rewrite failed: {status}");
    std::fs::rename(temp, path).unwrap();
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

async fn scan_materialized_scenario(chaos: &ChaosLibrarian, scenario: &Path) -> ScannedChaosRun {
    let run = chaos.materialize(scenario).unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.scan_root().unwrap();
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
    ScannedChaosRun { run, db, scan }
}
