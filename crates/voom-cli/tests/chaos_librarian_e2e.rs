#![expect(
    clippy::unwrap_used,
    reason = "E2E tests fail loudly and preserve paths for diagnosis"
)]

mod support;

use support::chaos_librarian::ChaosLibrarian;
use support::observed_state::{
    export_observed_state, library_relative_path, sha256_to_observed_hash,
};
use support::policy_seed::seed_transcode_policy_from_scan;
use support::voom_cli::{VoomTestDb, run_voom};

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

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn static_library_baseline_scans_exports_and_compares() {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.upstream_scenario("static-library.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();

    let scan = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
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
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();
    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-required.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
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
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();
    support::voom_cli::build_worker_binary("voom-verify-artifact-worker").unwrap();
    support::voom_cli::build_worker_binary("voom-ffmpeg-worker").unwrap();

    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-required.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
    let library_arg = library_path.to_str().unwrap().to_owned();
    let scan = run_voom(&db.url, ["scan", "--path", library_arg.as_str()]).unwrap();
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
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    support::voom_cli::build_worker_binary("voom-ffprobe-worker").unwrap();

    let run = chaos
        .materialize(&chaos.voom_scenario("video-transcode-noop.yaml"))
        .unwrap();
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run.run_dir.join("library");
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
