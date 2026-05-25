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
