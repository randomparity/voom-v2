#![expect(
    clippy::unwrap_used,
    reason = "E2E tests fail loudly and preserve paths for diagnosis"
)]

mod support;

#[path = "support/owner_node.rs"]
mod owner_node;

use std::path::Path;

use support::chaos_librarian::{ChaosLibrarian, ChaosRun};
use support::observed_state::{
    export_observed_state, library_relative_path, sha256_to_observed_hash,
};
use support::policy_seed::seed_transcode_policy;
use support::voom_cli::{VoomTestDb, run_voom};
use voom_scan_worker::walk::{WalkOutcome, scan_root};
use voom_test_support::scan_seed::{SeedFile, SeedSidecar, SeededSource, seed_scanned_files};

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
        "3b26eb9625babca61feaf1f56cb7ed8d36f1bd1a"
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
fn observed_state_v6_preserves_recorded_audio_and_subtitle_dispositions() {
    for kind in ["audio", "subtitle"] {
        for (default, forced) in [(true, false), (false, true), (false, false)] {
            let stream = serde_json::json!({
                "kind": kind, "codec_name": "aac",
                "disposition": {"default": default, "forced": forced}
            });
            let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();
            assert_eq!(observed["default"], default, "{kind} default evidence");
            assert_eq!(observed["forced"], forced, "{kind} forced evidence");
        }
    }
}

#[test]
fn observed_state_v6_does_not_invent_disposition_evidence() {
    for kind in ["audio", "subtitle", "video"] {
        let mut stream = serde_json::json!({"kind": kind, "codec_name": "aac"});
        if kind == "video" {
            stream["disposition"] = serde_json::json!({"default": true, "forced": false});
        }
        let observed = support::observed_state::probed_stream_for_test(&stream).unwrap();
        assert!(observed.get("default").is_none(), "{kind}");
        assert!(observed.get("forced").is_none(), "{kind}");
    }
}

#[test]
fn observed_state_projects_only_supported_persisted_format_tags() {
    let project = support::observed_state::format_tags_for_test;
    assert_eq!(
        project(&serde_json::json!({})).unwrap(),
        serde_json::Value::Null
    );
    assert_eq!(
        project(&serde_json::json!({"raw": {"ffprobe_json": {"format": {}}}})).unwrap(),
        serde_json::json!({})
    );
    let snapshot = serde_json::json!({"raw": {"ffprobe_json": {"format": {"tags": {
        "TITLE": "Évidence", "YeAr": "", "encoder": "excluded"
    }}}}});
    assert_eq!(
        project(&snapshot).unwrap(),
        serde_json::json!({"title": "Évidence", "year": ""})
    );
    for tags in [
        serde_json::json!({"title": "one", "TITLE": "two"}),
        serde_json::json!({"year": 2026}),
        serde_json::json!({"title": "é".repeat(2049)}),
        serde_json::json!([]),
        serde_json::Value::Null,
    ] {
        let snapshot = serde_json::json!({"raw": {"ffprobe_json": {"format": {"tags": tags}}}});
        assert!(
            project(&snapshot).is_err(),
            "invalid persisted tag evidence: {tags}"
        );
    }
    let boundary = serde_json::json!({"raw": {"ffprobe_json": {"format": {"tags": {"title": "é".repeat(2048)}}}}});
    assert!(project(&boundary).is_ok());
}

#[test]
fn observed_state_coverage_rejects_unreported_tags_and_each_stream_flag() {
    let complete = serde_json::json!({
        "format_tags": {"title": "", "year": "2026"},
        "streams": [
            {"kind": "audio", "default": false, "forced": true},
            {"kind": "subtitle", "default": true, "forced": false}
        ]
    });
    let covered = support::observed_state::require_probe_evidence;
    assert!(covered(&complete).is_ok());
    for replacement in [serde_json::Value::Null, serde_json::json!([])] {
        let mut missing = complete.clone();
        missing["format_tags"] = replacement;
        assert!(covered(&missing).is_err());
    }
    let mut missing = complete.clone();
    missing.as_object_mut().unwrap().remove("format_tags");
    assert!(covered(&missing).is_err());
    // An observed empty map proves tag inspection; no absent key becomes an empty value.
    let mut empty = complete.clone();
    empty["format_tags"] = serde_json::json!({});
    assert!(covered(&empty).is_ok());
    for index in [0, 1] {
        for flag in ["default", "forced"] {
            let mut missing = complete.clone();
            missing["streams"][index]
                .as_object_mut()
                .unwrap()
                .remove(flag);
            assert!(covered(&missing).is_err(), "stream {index} missing {flag}");
        }
        let mut missing = complete.clone();
        missing["streams"].as_array_mut().unwrap().remove(index);
        assert!(
            covered(&missing).is_err(),
            "missing required stream kind {index}"
        );
    }
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
    let observed = export_observed_state(
        &db.url,
        &run.run_dir,
        &observed_path,
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .unwrap();
    assert_eq!(observed["schema_version"], 6);
    let compare = chaos
        .compare_final_state(&run.run_dir, &observed_path)
        .unwrap();

    assert_eq!(compare["ok"], true);
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn chaos_contract_validation_rejects_incompatible_artifacts() {
    let chaos = ready_chaos();
    let run = chaos
        .materialize(&chaos.upstream_scenario("static-library.yaml"))
        .unwrap();
    let replay: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run.run_dir.join("replay.json")).unwrap()).unwrap();
    let scenario = std::fs::read_to_string(chaos.upstream_scenario("static-library.yaml")).unwrap();
    chaos
        .validate_contract("scenario", &serde_json::Value::String(scenario.clone()))
        .unwrap();
    let incompatible = scenario.replacen("schema_version: 37", "schema_version: 999", 1);
    assert_ne!(incompatible, scenario);
    assert!(
        chaos
            .validate_contract("scenario", &serde_json::Value::String(incompatible))
            .is_err()
    );
    assert_eq!(run.report["schema_version"], 17);
    assert_eq!(replay["schema_version"], 15);
    let mut contracts = vec![("materialization", run.report.clone()), ("replay", replay)];
    let reports = std::fs::read_dir(run.run_dir.join("reports/assets")).unwrap();
    let mut asset_count = 0;
    for report in reports {
        let report = report.unwrap();
        if report
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            contracts.push((
                "asset",
                serde_json::from_slice(&std::fs::read(report.path()).unwrap()).unwrap(),
            ));
            asset_count += 1;
        }
    }
    assert!(
        asset_count > 0,
        "asset schema validation must not be vacuous"
    );
    contracts.push((
        "observed",
        serde_json::json!({
            "schema_version": 6, "consumer": {"name": "voom"},
            "run_id": run.report["run_id"], "observed_at": "2026-10-25T00:00:00Z", "assets": []
        }),
    ));
    for (contract, value) in contracts {
        chaos.validate_contract(contract, &value).unwrap();
        for version in [0, 4, 999] {
            let mut incompatible = value.clone();
            incompatible["schema_version"] = version.into();
            assert!(
                chaos.validate_contract(contract, &incompatible).is_err(),
                "{contract} accepted version {version}"
            );
        }
    }
}

#[test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
fn observed_state_v6_oracle_detects_missing_flags_and_preserves_tag_evidence() {
    let chaos = ready_chaos();
    let streams: Vec<_> = ["audio", "subtitle"]
        .into_iter()
        .map(|kind| {
            support::observed_state::probed_stream_for_test(&serde_json::json!({
                "kind": kind, "codec_name": "aac",
                "disposition": {"default": true, "forced": false}
            }))
            .unwrap()
        })
        .collect();
    let output = std::process::Command::new("uv")
        .current_dir(&chaos.submodule_dir)
        .args([
            "run",
            "python",
            "-c",
            r#"
import json, sys
from chaos_librarian.contract.manifest import ProbedMedia, ProbedStream
from chaos_librarian.adapter.probe import compare_probed_media
base = dict(container="matroska", duration_seconds=1.0, size_bytes=1, streams=[])
for tags in (None, {}, {"title": ""}, {"year": "9999"}):
    value = ProbedMedia(**base, format_tags=tags)
    assert value.model_dump()["format_tags"] == tags
assert ProbedMedia(**base).format_tags is None
assert compare_probed_media(ProbedMedia(**base, format_tags={"title": "expected"}),
                            ProbedMedia(**base)) == [], "null metadata is unreported, not coverage"
assert compare_probed_media(ProbedMedia(**base, format_tags={}),
                            ProbedMedia(**base, format_tags={"title": ""})) == [
                                ("format_tags.title", None, "")]
for stream in json.loads(sys.argv[1]):
    expected = ProbedStream(kind=stream["kind"], codec="aac", default=True, forced=False)
    expected_media = ProbedMedia(**(base | {"streams": [expected]}))
    observed = ProbedMedia(**(base | {"streams": [stream]}))
    assert compare_probed_media(expected_media, observed) == []
    del stream["default"]
    del stream["forced"]
    missing = ProbedMedia(**(base | {"streams": [stream]}))
    assert compare_probed_media(expected_media, missing) == [
        ("streams.0.default", True, None), ("streams.0.forced", False, None)]
"#,
            &serde_json::to_string(&streams).unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Only this chaos case uses the real authenticated owner stack. The other
/// cases retain emulation to isolate their durable orchestration assertions.
#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires Chaos Librarian media tools"]
async fn transcode_required_executes_real_owner_and_commits_hevc_mkv() {
    let chaos = ready_chaos();
    let run = chaos
        .materialize(&chaos.upstream_scenario("voom-ci/h264-transcode-candidate.yaml"))
        .unwrap();
    let evidence = run_real_owner_encode(&chaos, &run).await.unwrap();
    assert!(
        evidence["plan"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["operation_kind"] == "transcode_video")
    );
    let execute = &evidence["execution"];
    let transcode_summary = &execute["summary"]["per_operation"]["transcode_video"];
    assert_eq!(transcode_summary["success_count"], 1);
    assert_eq!(transcode_summary["failure_count"], 0);
    let file_phase = &execute["file_phases"][0];
    assert_eq!(file_phase["outcome"], "committed");
    assert!(!file_phase["ticket_ids"].as_array().unwrap().is_empty());
    assert!(file_phase["produced_file_version_id"].as_u64().unwrap() > 0);
    assert!(file_phase["reprobe_snapshot_id"].as_u64().unwrap() > 0);
    assert!(is_hevc_matroska_probe(&evidence["probe"]));
}

#[tokio::test]
#[ignore = "run with just chaos-e2e-ci; requires real owner workers and media tools"]
async fn production_scan_persists_probe_evidence_and_isolates_invalid_file() {
    let chaos = ready_chaos();
    let work = tempfile::tempdir().unwrap();
    let run_dir = work.path().join("run");
    let library = run_dir.join("library");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(
        run_dir.join("replay.json"),
        br#"{"run_id":"74200000-0000-4000-8000-000000000001"}"#,
    )
    .unwrap();
    let evidence = run_real_owner_process(&chaos, &library, work.path(), "real_owner_scan.py")
        .await
        .unwrap();
    assert_eq!(evidence["scan"]["status"], "succeeded");
    assert_eq!(evidence["scan"]["observation_count"], 2);
    assert_eq!(evidence["completion"]["failed_content_count"], 1);
    assert_eq!(evidence["completion"]["degraded_evidence_count"], 0);
    // Observe the real worker's tool invocation, independently of the absence
    // of a snapshot. An evidence-less observation alone has several causes.
    let invocations = evidence["probe_invocations"].as_array().unwrap();
    for (name, success) in [("healthy.mkv", true), ("invalid.mp4", false)] {
        let calls: Vec<_> = invocations
            .iter()
            .filter(|call| {
                call["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg.as_str().is_some_and(|arg| arg.ends_with(name)))
            })
            .collect();
        assert_eq!(calls.len(), 1, "one actual worker probe for {name}");
        let exit_code = calls[0]["exit_code"].as_i64().unwrap();
        assert_eq!(exit_code == 0, success, "{name}");
    }
    let url = format!("sqlite://{}", work.path().join("stack/voom.db").display());
    let pool = voom_store::connect(&url).await.unwrap();
    let observations: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT provider_relative_locator, evidence_json FROM scan_observations ORDER BY provider_relative_locator"
    ).fetch_all(&pool).await.unwrap();
    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].0, "healthy.mkv");
    let healthy: serde_json::Value =
        serde_json::from_str(observations[0].1.as_ref().unwrap()).unwrap();
    assert!(
        healthy["content_hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    assert_eq!(
        healthy["probe_snapshot"]["raw"]["ffprobe_json"]["format"]["tags"]["title"],
        "Persisted Évidence"
    );
    assert_eq!(observations[1], ("invalid.mp4".to_owned(), None));
    let snapshot_count: i64 = sqlx::query_scalar("SELECT count(*) FROM media_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        snapshot_count, 1,
        "failed probe must not invent a published snapshot"
    );
    pool.close().await;

    let observed = export_observed_state(
        &url,
        &run_dir,
        &run_dir.join("observed-state.json"),
        env!("CARGO_PKG_VERSION"),
    )
    .await
    .unwrap();
    chaos.validate_contract("observed", &observed).unwrap();
    let assets = observed["assets"].as_array().unwrap();
    assert_eq!(
        assets.len(),
        1,
        "only successful identity evidence publishes an asset"
    );
    assert_eq!(assets[0]["current_path"], "healthy.mkv");
    let probed = &assets[0]["probed"];
    support::observed_state::require_probe_evidence(probed).unwrap();
    assert_eq!(
        probed["format_tags"],
        serde_json::json!({"title": "Persisted Évidence", "year": "2026"})
    );
    for (kind, default, forced) in [("audio", true, false), ("subtitle", false, true)] {
        let stream = probed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| stream["kind"] == kind)
            .unwrap();
        assert_eq!(stream["default"], default);
        assert_eq!(stream["forced"], forced);
    }
    let mut missing = probed.clone();
    missing["format_tags"] = serde_json::Value::Null;
    assert!(
        support::observed_state::require_probe_evidence(&missing).is_err(),
        "upstream null permissiveness must not create a coverage claim"
    );
}

async fn run_real_owner_encode(
    chaos: &ChaosLibrarian,
    run: &ChaosRun,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let work = tempfile::tempdir().unwrap();
    run_real_owner_process(chaos, &run.scan_root(), work.path(), "real_owner_encode.py").await
}

async fn run_real_owner_process(
    chaos: &ChaosLibrarian,
    input_root: &Path,
    work: &Path,
    script: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    use std::time::Duration;

    let stdout = work.join("stdout.json");
    let stderr = work.join("stderr.log");
    let binary = voom_test_support::worker::target_debug_binary("voom");
    // Files avoid undrained pipe deadlocks while the bounded wait supervises
    // Python; Bootstrap owns authenticated activation and descendant shutdown.
    let mut child = tokio::process::Command::new("python3")
        .arg(
            chaos
                .workspace_root
                .join("crates/voom-cli/tests/support")
                .join(script),
        )
        .args(["serve", "--workdir"])
        .arg(work.join("stack"))
        .arg("--input")
        .arg(input_root)
        .arg("--bin-dir")
        .arg(binary.parent().unwrap())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let status =
        if let Ok(status) = tokio::time::timeout(Duration::from_mins(15), child.wait()).await {
            status.unwrap()
        } else {
            // Bootstrap observes SIGTERM between bounded CLI calls. Allow an
            // in-flight 60s CLI plus its two bounded child-shutdown waits.
            let terminated = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(child.id().unwrap().to_string())
                .status()
                .unwrap();
            assert!(terminated.success());
            if tokio::time::timeout(Duration::from_mins(3), child.wait())
                .await
                .is_err()
            {
                child.kill().await.unwrap();
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "real owner process exceeded its deadline: {}",
                    std::fs::read_to_string(stderr).unwrap()
                ),
            )
            .into());
        };
    assert!(
        status.success(),
        "real owner process failed: {}\nCLI errors: {}",
        std::fs::read_to_string(stderr).unwrap(),
        real_owner_cli_errors(work)
    );
    Ok(serde_json::from_slice(&std::fs::read(stdout).unwrap())?)
}

/// Bootstrap already sanitizes registration receipts. Project only CLI error
/// fields here; never dump the agent config, credential files or whole receipts.
fn real_owner_cli_errors(work: &Path) -> serde_json::Value {
    let mut errors = Vec::new();
    if let Ok(entries) = std::fs::read_dir(work.join("stack/bootstrap-evidence")) {
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        for path in paths {
            if !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("cli-"))
                || path.extension().is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let receipt = std::fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
            if let Some(receipt) = receipt.filter(|value| value["error"].is_object()) {
                errors.push(serde_json::json!({
                    "code": receipt["error"]["code"],
                    "message": receipt["error"]["message"]
                }));
            }
        }
    }
    serde_json::Value::Array(errors)
}

#[test]
fn real_owner_failure_reports_cli_error_without_registration_or_configuration() {
    let work = tempfile::tempdir().unwrap();
    let evidence = work.path().join("stack/bootstrap-evidence");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(
        evidence.join("cli-0001.json"),
        br#"{"status":"error","node_id":null}"#,
    )
    .unwrap();
    std::fs::write(
        evidence.join("cli-0002.json"),
        br#"{
        "error":{"code":"CONFIG_INVALID","message":"policy slug mismatch","token":"hidden"},
        "data":{"token":"hidden"},"local":{"config_path":"hidden"}}
    "#,
    )
    .unwrap();
    std::fs::write(evidence.join("node-agent.toml"), b"hidden").unwrap();
    assert_eq!(
        real_owner_cli_errors(work.path()),
        serde_json::json!([
            {"code":"CONFIG_INVALID","message":"policy slug mismatch"}
        ])
    );
}

fn is_hevc_matroska_probe(probe: &serde_json::Value) -> bool {
    let matroska = probe["format"]["format_name"]
        .as_str()
        .is_some_and(|format| format.split(',').any(|name| name == "matroska"));
    let duration = probe["format"]["duration"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok())
        .is_some_and(|value| value.is_finite() && value > 0.0);
    let hevc = probe["streams"].as_array().is_some_and(|streams| {
        streams.iter().any(|stream| {
            stream["codec_type"] == "video"
                && stream["codec_name"] == "hevc"
                && stream["width"].as_u64().is_some_and(|value| value > 0)
                && stream["height"].as_u64().is_some_and(|value| value > 0)
        })
    });
    matroska && duration && hevc
}

#[test]
fn real_encode_probe_rejects_synthetic_bytes_wrong_codec_and_container() {
    // A filename or a fabricated successful commit cannot establish media facts.
    let synthetic: serde_json::Value =
        serde_json::from_slice(b"synthetic output").unwrap_or_default();
    assert!(!is_hevc_matroska_probe(&synthetic));
    let valid = serde_json::json!({
        "format": {"format_name": "matroska,webm", "duration": "1.0"},
        "streams": [{"codec_type": "video", "codec_name": "hevc", "width": 32, "height": 32}]
    });
    assert!(is_hevc_matroska_probe(&valid));
    for (pointer, invalid) in [
        ("/streams/0/codec_name", serde_json::json!("h264")),
        (
            "/format/format_name",
            serde_json::json!("mov,mp4,m4a,3gp,3g2,mj2"),
        ),
        ("/format/duration", serde_json::json!("0")),
        ("/streams/0/width", serde_json::json!(0)),
    ] {
        let mut wrong = valid.clone();
        *wrong.pointer_mut(pointer).unwrap() = invalid;
        assert!(
            !is_hevc_matroska_probe(&wrong),
            "accepted invalid {pointer}"
        );
    }
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
    let scenario = chaos.upstream_scenario("reencode-video.yaml");
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().canonicalize().unwrap().join("run");
    // File existence does not order the seeder's delayed byte read before a
    // timed mutation. Finish a zero-event baseline before publishing identity.
    let output = chaos
        .run_for_duration(&scenario, &run_dir, "1ms", "1x")
        .unwrap()
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "chaos-librarian run failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let baseline: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(baseline["applied_events"], 0);
    let db = VoomTestDb::init().await.unwrap();
    let library_path = run_dir.join("library");
    let root_id = db.configure_local_root(&library_path).await.unwrap();
    let cp = db.control_plane().await.unwrap();
    let walk = LibraryWalk::discover(&library_path);
    assert_eq!(walk.primary_paths.len(), 1);
    let seeds = walk.seed_files(&library_path);
    let original_bytes = std::fs::read(&walk.primary_paths[0]).unwrap();
    seed_scanned_files(&cp, &db.url, root_id, &seeds)
        .await
        .unwrap();

    // Generate the upstream mutation separately, then replace the same rooted
    // address only after its original identity has committed.
    let mutated = chaos.materialize(&scenario).unwrap();
    let replay: serde_json::Value =
        serde_json::from_slice(&std::fs::read(mutated.run_dir.join("replay.json")).unwrap())
            .unwrap();
    assert_eq!(replay["applied_events"], 1);
    let mutated_bytes = std::fs::read(mutated.scan_root().join(seeds[0].locator)).unwrap();
    assert!(
        original_bytes != mutated_bytes,
        "the reencode must change bytes"
    );
    std::fs::write(&walk.primary_paths[0], mutated_bytes).unwrap();
    let rescanned = LibraryWalk::discover(&library_path);
    let rescan_seeds = rescanned.seed_files(&library_path);

    // A rescan that observes different bytes at the same address must refuse
    // to overwrite the recorded identity during completion.
    let conflict = seed_scanned_files(&cp, &db.url, root_id, &rescan_seeds).await;
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

fn ready_chaos() -> ChaosLibrarian {
    let chaos = ChaosLibrarian::discover().unwrap();
    chaos.validate_ready().unwrap();
    chaos
}

/// Normalized probe snapshot for a materialized media file, taken with the real
/// `ffprobe` this suite already requires.
///
/// Deriving these facts from the file extension instead — as this fixture used
/// to — got both of them wrong. It reported the container as
/// `application/octet-stream`, which the planner cannot classify, so every node
/// planned `blocked` with `insufficient_snapshot_facts: snapshot container is
/// unknown`. It also guessed the video codec from the container family, which
/// mislabels this scenario's h264-in-MKV candidate as HEVC — the very transcode
/// the policy is meant to require.
fn probe_media_file(path: &Path) -> serde_json::Value {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "ffprobe failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let probed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let container = probed["format"]["format_name"].as_str().unwrap();
    let streams: Vec<serde_json::Value> = probed["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|stream| stream["codec_type"] == "video")
        .enumerate()
        .map(|(index, stream)| {
            serde_json::json!({
                "index": index,
                "kind": "video",
                "codec_name": stream["codec_name"],
                "width": stream["width"],
                "height": stream["height"],
            })
        })
        .collect();
    assert!(
        !streams.is_empty(),
        "{} carries no video stream",
        path.display()
    );
    serde_json::json!({
        "format": "sprint10-v1",
        "container": { "format_name": container },
        "streams": streams,
    })
}

/// What the production scan walker found under a materialized library: one
/// entry per primary media file, each carrying the sidecars grouped onto it.
///
/// Discovery runs through `voom_scan_worker::walk` rather than a second
/// extension table maintained here. The private table this replaces listed only
/// containers, so `.srt` subtitle sidecars were never seeded, no bundle
/// membership was ever published, and the observed-state export had no sidecar
/// to attach to its primary (#551).
struct LibraryWalk {
    outcome: WalkOutcome,
    /// Absolute path of each primary, parallel to `outcome.candidates`.
    primary_paths: Vec<std::path::PathBuf>,
}

impl LibraryWalk {
    fn discover(library_path: &Path) -> Self {
        let outcome = scan_root(library_path, &[]).unwrap();
        assert!(
            !outcome.candidates.is_empty(),
            "the scenario must materialize media files under {}",
            library_path.display()
        );
        let primary_paths = outcome
            .candidates
            .iter()
            .map(|candidate| library_path.join(candidate.primary.locator.as_str()))
            .collect();
        Self {
            outcome,
            primary_paths,
        }
    }

    /// Build `SeedFile`s the seeder publishes through the real scan-session
    /// chain, probing each primary and hashing each sidecar off disk.
    fn seed_files(&self, library_path: &Path) -> Vec<SeedFile<'_>> {
        self.outcome
            .candidates
            .iter()
            .zip(&self.primary_paths)
            .map(|(candidate, path)| SeedFile {
                locator: candidate.primary.locator.as_str(),
                path,
                probe_snapshot: probe_media_file(path),
                sidecars: candidate
                    .sidecars
                    .iter()
                    .map(|sidecar| SeedSidecar {
                        locator: sidecar.locator.as_str().to_owned(),
                        // The walker sets a role on every file it files as a
                        // sidecar; `None` is reserved for primaries.
                        role: sidecar.kind.unwrap().to_owned(),
                        path: library_path.join(sidecar.locator.as_str()),
                    })
                    .collect(),
            })
            .collect()
    }
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
    let walk = LibraryWalk::discover(&library_path);
    let seeds = walk.seed_files(&library_path);
    let cp = db.control_plane().await.unwrap();
    let seeded = seed_scanned_files(&cp, &db.url, root_id, &seeds)
        .await
        .unwrap();
    SeededChaosRun { run, db, seeded }
}

#[tokio::test]
async fn staging_path_outside_registered_roots_is_not_a_staging_root() {
    let library = tempfile::tempdir().unwrap();
    let sibling = tempfile::tempdir().unwrap();
    let db = VoomTestDb::init().await.unwrap();
    db.configure_local_root(library.path()).await.unwrap();

    assert!(db.is_registered_staging_root(library.path()).await.unwrap());
    assert!(!db.is_registered_staging_root(sibling.path()).await.unwrap());
}
