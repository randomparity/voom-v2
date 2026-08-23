#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use secrecy::ExposeSecret as _;
use serde_json::Value;
use tempfile::TempDir;
use voom_control_plane::ControlPlane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{NodeKind, ProviderLocator, StorageProviderKind, StorageRootId};
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;
use voom_test_support::commit_node::SimulatedOwnerNode;
use voom_test_support::scan_seed::{SeedFile, SeededSource, seed_scanned_files};
use voom_test_support::worker::cargo_bin_or_build;

const BASIC_FFPROBE_JSON: &str =
    include_str!("../../voom-ffprobe-worker/fixtures/ffprobe/basic-mp4.json");

#[tokio::test]
async fn artifact_full_flow_outputs_committed_envelopes() {
    let seeded = seed().await;
    let dir = artifact_tempdir(&seeded);
    let media = seeded.media.clone();
    let staging = dir.path().join("staged.mp4");
    let target = dir.path().join("committed.mp4");
    // The control-plane staging byte copy is gone (ADR 0075): seed the
    // staged rows directly, then drive verify/commit/show through the CLI.
    std::fs::copy(&media, &staging).unwrap();
    let pool = voom_store::connect(&seeded.url).await.unwrap();
    let staged_artifact = voom_test_support::staging_seed::seed_staged_artifact(
        &pool,
        seeded.source.file_version_id,
        &staging,
    )
    .await
    .unwrap();
    let artifact_handle_id = staged_artifact.artifact_handle_id.0;
    let verify = run(
        artifact_command(&seeded)
            .args([
                "artifact",
                "verify",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--staging-root",
            ])
            .arg(dir.path()),
        Some(0),
    );
    let commit = run(
        artifact_command(&seeded)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target),
        Some(0),
    );
    let show = run(
        artifact_command(&seeded).args([
            "artifact",
            "show",
            "--artifact-handle-id",
            &artifact_handle_id.to_string(),
        ]),
        Some(0),
    );

    assert_eq!(show["data"]["artifact"]["state"], "committed");
    let mut json = Value::Array(vec![verify, commit, show]);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &[
            (media.as_path(), "[media]/tiny.source"),
            (dir.path(), "[artifact-dir]"),
            (target.as_path(), "[artifact-dir]/committed.mp4"),
        ],
    );
    redact_path_set(&mut json, &[(seeded.root.path(), "[media]")]);
    insta::assert_json_snapshot!("artifact_full_flow_outputs_committed_envelopes", json);
}

#[tokio::test]
async fn artifact_list_and_show_cover_all_inspection_states() {
    let seeded = seed().await;
    let dir = artifact_tempdir(&seeded);
    let staged = create_staged_artifact(&seeded, dir.path(), "staged").await;
    let verified = create_verified_artifact(&seeded, dir.path(), "verified").await;
    let committed = create_committed_artifact(&seeded, dir.path(), "committed").await;
    let failed = create_failed_artifact(&seeded, dir.path(), "failed").await;
    let recovery = create_verified_artifact(&seeded, dir.path(), "recovery").await;
    inject_recovery_required(
        &seeded.url,
        recovery.artifact_handle_id,
        recovery.verification_id.unwrap(),
        dir.path(),
    )
    .await;

    let mut envelopes = Vec::new();
    for state in [
        "staged",
        "verified",
        "committed",
        "failed",
        "recovery_required",
    ] {
        let list = run(
            artifact_command(&seeded).args(["artifact", "list", "--state", state]),
            Some(0),
        );
        assert_eq!(list["data"]["artifacts"].as_array().unwrap().len(), 1);
        assert_eq!(list["data"]["artifacts"][0]["state"], state);
        envelopes.push(list);
    }
    for artifact in [&staged, &verified, &committed, &failed, &recovery] {
        envelopes.push(run(
            artifact_command(&seeded).args([
                "artifact",
                "show",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
            ]),
            Some(0),
        ));
    }

    assert!(envelopes.iter().any(|json| {
        json["data"]["artifact"]["latest_verification"]["status"] == "failed"
            && json["data"]["artifact"]["latest_verification"]["id"].is_number()
    }));
    let recovery_show = envelopes.last().unwrap();
    let recovery_commit = &recovery_show["data"]["artifact"]["latest_commit"];
    assert_eq!(recovery_commit["state"], "recovery_required");
    assert!(recovery_commit["id"].is_number());
    assert!(recovery_commit["target_path"].is_string());
    assert!(recovery_commit["temp_path"].is_string());
    assert!(recovery_commit["recovery"]["target"]["exists"].is_boolean());
    assert!(recovery_commit["recovery"]["temp"]["exists"].is_boolean());
    assert!(recovery_commit["recovery"]["staging"]["exists"].is_boolean());

    let mut json = Value::Array(envelopes);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &path_redaction_inputs(
            dir.path(),
            &[
                (&staged, "staged"),
                (&verified, "verified"),
                (&committed, "committed"),
                (&failed, "failed"),
                (&recovery, "recovery"),
            ],
        ),
    );
    redact_path_set(&mut json, &[(seeded.root.path(), "[media]")]);
    insta::assert_json_snapshot!("artifact_list_and_show_cover_all_inspection_states", json);
}

#[tokio::test]
async fn artifact_failure_envelopes_are_actionable() {
    let seeded = seed().await;
    let dir = artifact_tempdir(&seeded);
    let unverified = create_staged_artifact(&seeded, dir.path(), "unverified").await;
    let drift = create_verified_artifact(&seeded, dir.path(), "drift").await;
    std::fs::write(&drift.staging_path, b"changed bytes").unwrap();
    let existing_target = create_verified_artifact(&seeded, dir.path(), "existing").await;
    let existing_target_path = dir.path().join("already-exists.mp4");
    std::fs::write(&existing_target_path, b"already here").unwrap();
    let failed = create_failed_artifact(&seeded, dir.path(), "verify-failed").await;
    let missing = run(
        artifact_command(&seeded).args(["artifact", "show", "--artifact-handle-id", "999999"]),
        Some(2),
    );
    let failed_verification = run(
        artifact_command(&seeded).args([
            "artifact",
            "show",
            "--artifact-handle-id",
            &failed.artifact_handle_id.to_string(),
        ]),
        Some(0),
    );
    let unverified_commit = run(
        artifact_command(&seeded)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &unverified.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(dir.path().join("unverified-target.mp4")),
        Some(2),
    );
    let drift_commit = run(
        artifact_command(&seeded)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &drift.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(dir.path().join("drift-target.mp4")),
        Some(2),
    );
    let target_exists = run(
        artifact_command(&seeded)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &existing_target.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&existing_target_path),
        Some(2),
    );
    assert!(failed_verification["data"]["artifact"]["latest_verification"]["id"].is_number());
    assert_eq!(
        failed_verification["data"]["artifact"]["latest_verification"]["status"],
        "failed"
    );
    assert_eq!(missing["error"]["code"], "NOT_FOUND");
    assert_eq!(unverified_commit["error"]["code"], "CONFIG_INVALID");
    assert_eq!(drift_commit["error"]["code"], "ARTIFACT_CHECKSUM_MISMATCH");
    assert_eq!(target_exists["error"]["code"], "CONFIG_INVALID");
    let mut json = Value::Array(vec![
        missing,
        failed_verification,
        unverified_commit,
        drift_commit,
        target_exists,
    ]);
    redact_artifact_snapshot(
        &mut json,
        &seeded.url,
        &path_redaction_inputs(
            dir.path(),
            &[
                (&unverified, "unverified"),
                (&drift, "drift"),
                (&existing_target, "existing"),
                (&failed, "verify-failed"),
            ],
        ),
    );
    redact_path_set(&mut json, &[(seeded.root.path(), "[media]")]);
    redact_path_set(
        &mut json,
        &[(
            existing_target_path.as_path(),
            "[artifact-dir]/already-exists.mp4",
        )],
    );
    redact_long_target_names(&mut json);
    insta::assert_json_snapshot!("artifact_failure_envelopes_are_actionable", json);
}

struct Seeded {
    _tmp: TempDatabase,
    root: TempDir,
    url: String,
    media: PathBuf,
    node_id: u64,
    source: SeededSource,
}

#[derive(Debug)]
struct ArtifactFixture {
    artifact_handle_id: u64,
    staging_path: PathBuf,
    target_path: Option<PathBuf>,
    verification_id: Option<u64>,
}

async fn seed() -> Seeded {
    let tmp = TempDatabase::new().unwrap();
    let url = sqlite_url_for(tmp.path());
    voom_store::init(&url).await.unwrap();
    let cp = ControlPlane::open(&url).await.unwrap();
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "artifact-envelope-local".to_owned(),
            kind: NodeKind::Local,
            heartbeat_ttl_seconds: 60,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    cp.heartbeat_node(registered.node.id, registered.token.expose_secret())
        .await
        .unwrap();
    let library = cp
        .create_library(NewLibrary {
            slug: "artifact-envelope".to_owned(),
            display_name: "Artifact envelope".to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root_dir = tempfile::tempdir().unwrap();
    let media = root_dir.path().join("tiny.source");
    std::fs::copy(tiny_media_fixture(), &media).unwrap();
    let root_path = root_dir.path().to_str().unwrap().to_owned();
    let storage_root = cp
        .create_library_root(NewLibraryRoot {
            library_id: library.id,
            owner_node_id: registered.node.id,
            provider_kind: StorageProviderKind::LocalFilesystem,
            provider_locator: ProviderLocator::new(root_path.clone()).unwrap(),
            display_locator: root_path,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: vec!["source".to_owned()],
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    cp.activate_library_root(storage_root.id, "artifact-envelope-fixture".to_owned())
        .await
        .unwrap();
    let source = seed_scanned_files(
        &cp,
        &url,
        StorageRootId(storage_root.id.0),
        &[SeedFile {
            locator: "tiny.source",
            path: &media,
            probe_snapshot: basic_mp4_probe_snapshot(),
        }],
    )
    .await
    .unwrap()[0];
    let seeded = Seeded {
        _tmp: tmp,
        root: root_dir,
        url: url.clone(),
        media,
        node_id: registered.node.id.0,
        source,
    };
    spawn_commit_driver(&seeded);
    seeded
}

/// Background stand-in for the storage-owner agent (ADR 0074): flips the
/// seeded owner node into the simulated remote principal and drives every
/// pending commit intent to convergence so CLI subprocess commits complete.
fn spawn_commit_driver(seeded: &Seeded) {
    let url = seeded.url.clone();
    let owner_node_id = seeded.node_id;
    // The tests drive the CLI as a blocking subprocess, so the driver needs
    // its own thread and runtime to make progress.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let pool = voom_store::connect(&url).await.unwrap();
            let mut node = SimulatedOwnerNode::new().unwrap();
            node.install_for(&pool, voom_core::NodeId(owner_node_id))
                .await
                .unwrap();
            // Authenticate as the installed seeded owner, not the default
            // simulated principal id.
            node.node_id = voom_core::NodeId(owner_node_id);
            let cp = ControlPlane::open(&url).await.unwrap();
            loop {
                let pending: Option<(i64, i64)> = sqlx::query_as(
                    "SELECT id, artifact_handle_id FROM artifact_commit_intents \
                     WHERE state = 'pending' ORDER BY id ASC LIMIT 1",
                )
                .fetch_optional(&pool)
                .await
                .unwrap();
                if let Some((_, artifact_handle_id)) = pending {
                    let _ = node
                        .drive_pending_commit(
                            &cp,
                            &pool,
                            voom_core::ArtifactHandleId(u64::try_from(artifact_handle_id).unwrap()),
                        )
                        .await
                        .ok();
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    });
}

/// Canned normalized probe snapshot matching what the real ffprobe worker
/// reports for the tiny fixture (`basic-mp4.json` once normalized), so the
/// seeded source snapshot agrees with every later staged-artifact probe.
fn basic_mp4_probe_snapshot() -> Value {
    serde_json::json!({
        "format": "sprint10-v1",
        "container": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
        },
        "streams": [
            {
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "width": 320,
                "height": 180,
            },
            {
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "channels": 2,
            },
        ],
    })
}

/// The control-plane staging byte copy is retired (ADR 0075): seed the
/// staging bytes and their durable rows directly instead of shelling out.
async fn create_staged_artifact(seeded: &Seeded, dir: &Path, name: &str) -> ArtifactFixture {
    let staging_path = dir.join(format!("{name}-staged.mp4"));
    std::fs::copy(&seeded.media, &staging_path).unwrap();
    let pool = voom_store::connect(&seeded.url).await.unwrap();
    let staged = voom_test_support::staging_seed::seed_staged_artifact(
        &pool,
        seeded.source.file_version_id,
        &staging_path,
    )
    .await
    .unwrap();

    ArtifactFixture {
        artifact_handle_id: staged.artifact_handle_id.0,
        staging_path,
        target_path: None,
        verification_id: None,
    }
}

async fn create_verified_artifact(
    seeded: &Seeded,
    dir: &Path,
    name: &str,
) -> ArtifactFixture {
    let mut artifact = create_staged_artifact(seeded, dir, name).await;
    let verify = run(
        artifact_command(seeded)
            .args([
                "artifact",
                "verify",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
                "--staging-root",
            ])
            .arg(dir),
        Some(0),
    );
    assert_eq!(verify["data"]["artifact"]["status"], "succeeded");
    artifact.verification_id = Some(id(&verify["data"]["artifact"]["verification_id"]));
    artifact
}

async fn create_committed_artifact(
    seeded: &Seeded,
    dir: &Path,
    name: &str,
) -> ArtifactFixture {
    let mut artifact = create_verified_artifact(seeded, dir, name).await;
    let target_path = dir.join(format!("{name}-committed.mp4"));
    let commit = run(
        artifact_command(seeded)
            .args([
                "artifact",
                "commit",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
                "--target-path",
            ])
            .arg(&target_path),
        Some(0),
    );
    assert_eq!(commit["data"]["artifact"]["state"], "committed");
    artifact.target_path = Some(target_path);
    artifact
}

async fn create_failed_artifact(seeded: &Seeded, dir: &Path, name: &str) -> ArtifactFixture {
    let mut artifact = create_staged_artifact(seeded, dir, name).await;
    std::fs::write(&artifact.staging_path, b"changed bytes").unwrap();
    let verify = run(
        artifact_command(seeded)
            .args([
                "artifact",
                "verify",
                "--artifact-handle-id",
                &artifact.artifact_handle_id.to_string(),
                "--staging-root",
            ])
            .arg(dir),
        Some(0),
    );
    assert_eq!(verify["data"]["artifact"]["status"], "failed");
    artifact.verification_id = Some(id(&verify["data"]["artifact"]["verification_id"]));
    artifact
}

async fn inject_recovery_required(
    url: &str,
    artifact_handle_id: u64,
    verification_id: u64,
    dir: &Path,
) {
    let pool = voom_store::connect(url).await.unwrap();
    let source_file_version_id: i64 =
        sqlx::query_scalar("SELECT file_version_id FROM artifact_handles WHERE id = ?")
            .bind(i64::try_from(artifact_handle_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    let target_path = dir.join("recovery-target.mp4");
    let temp_path = dir.join("recovery-target.mp4.voom.tmp");
    std::fs::write(&target_path, b"promoted bytes").unwrap();
    std::fs::write(&temp_path, b"temp bytes").unwrap();
    sqlx::query(
        "INSERT INTO artifact_commit_records \
         (artifact_handle_id, source_file_version_id, verification_id, target_path, \
          result_file_version_id, result_file_location_id, state, failure_class, error_code, \
          message, recovery_reason, temp_path, report, started_at, promotion_started_at, finished_at) \
         VALUES (?, ?, ?, ?, NULL, NULL, 'recovery_required', 'commit_failure', \
          'DB_UNREACHABLE', 'injected recovery for CLI inspection', 'promotion_started', ?, \
          '{\"test\":true}', '2026-05-25T00:00:00Z', '2026-05-25T00:00:01Z', '2026-05-25T00:00:02Z')",
    )
    .bind(i64::try_from(artifact_handle_id).unwrap())
    .bind(source_file_version_id)
    .bind(i64::try_from(verification_id).unwrap())
    .bind(target_path.display().to_string())
    .bind(temp_path.display().to_string())
    .execute(&pool)
    .await
    .unwrap();
}

fn artifact_command(seeded: &Seeded) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command
        .arg("--database-url")
        .arg(&seeded.url)
        .env("VOOM_LOCAL_NODE_ID", seeded.node_id.to_string())
        .env(
            "VOOM_VERIFY_ARTIFACT_WORKER_BIN",
            built_verify_worker_binary(),
        )
        .env("VOOM_FFPROBE_WORKER_BIN", built_ffprobe_worker_binary())
        .env("VOOM_FFPROBE_BIN", success_ffprobe_binary());
    command
}

fn built_ffprobe_worker_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| cargo_bin_or_build("voom-ffprobe-worker", "voom-ffprobe-worker").unwrap())
}

fn built_verify_worker_binary() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        cargo_bin_or_build("voom-verify-artifact-worker", "voom-verify-artifact-worker").unwrap()
    })
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn tiny_media_fixture() -> PathBuf {
    workspace_root()
        .join("crates/voom-ffprobe-worker/fixtures/media/tiny.mp4")
        .canonicalize()
        .unwrap()
}

fn artifact_tempdir(seeded: &Seeded) -> TempDir {
    TempDir::new_in(seeded.root.path().canonicalize().unwrap()).unwrap()
}

fn success_ffprobe_binary() -> &'static PathBuf {
    static BIN: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
    &BIN.get_or_init(|| {
        let dir = TempDir::new().unwrap();
        let path = write_success_ffprobe(dir.path());
        (dir, path)
    })
    .1
}

fn write_success_ffprobe(dir: &Path) -> PathBuf {
    let script = format!(
        "#!/usr/bin/env sh\n\
         set -eu\n\
         if [ \"${{1:-}}\" = '-version' ]; then printf 'ffprobe version test-helper Copyright\\n'; exit 0; fi\n\
         cat <<'JSON'\n\
         {BASIC_FFPROBE_JSON}\n\
         JSON\n"
    );
    write_executable(dir, "ffprobe", &script)
}

fn write_executable(dir: &Path, name: &str, contents: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).unwrap();
    path
}

fn run(command: &mut Command, expected: Option<i32>) -> Value {
    let output = command.output().unwrap();
    assert_status(&output, expected);
    envelope(output.stdout)
}

fn assert_status(output: &Output, expected: Option<i32>) {
    assert_eq!(
        output.status.code(),
        expected,
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}

fn id(value: &Value) -> u64 {
    value.as_u64().unwrap()
}

fn redact_artifact_snapshot(json: &mut Value, db_url: &str, paths: &[(&Path, &str)]) {
    redact_common(json, db_url);
    redact_path_set(json, paths);
    redact_temp_path_names(json);
    redact_hashes(json);
    redact_worker_ids(json);
    redact_local_file_keys(json);
}

fn redact_common(json: &mut Value, db_url: &str) {
    replace_string(json, db_url, "[db-url]");
    redact_path_set(
        json,
        &[(success_ffprobe_binary().as_path(), "[ffprobe-bin]")],
    );
    replace_key_value(
        json,
        "config_path",
        &Value::String("[config-path]".to_owned()),
    );
}

fn redact_path_set(value: &mut Value, paths: &[(&Path, &str)]) {
    let mut replacements = paths
        .iter()
        .flat_map(|(path, replacement)| path_redactions(path, replacement))
        .collect::<Vec<_>>();
    replacements.sort_by_key(|(needle, _)| std::cmp::Reverse(needle.len()));
    for (needle, replacement) in replacements {
        replace_string(value, &needle, &replacement);
    }
}

fn path_redaction_inputs<'a>(
    dir: &'a Path,
    _artifacts: &[(&'a ArtifactFixture, &'a str)],
) -> Vec<(&'a Path, &'a str)> {
    vec![(dir, "[artifact-dir]")]
}

fn path_redactions(path: &Path, replacement: &str) -> Vec<(String, String)> {
    let replacement = replacement.to_owned();
    let mut redactions = vec![(path.display().to_string(), replacement.clone())];
    if let Ok(canonical) = path.canonicalize() {
        let canonical = canonical.display().to_string();
        if redactions.iter().all(|(needle, _)| needle != &canonical) {
            redactions.push((canonical, replacement));
        }
    }
    redactions
}

fn redact_hashes(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                redact_hashes(item);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                if matches!(
                    key.as_str(),
                    "content_hash" | "checksum" | "expected_checksum" | "observed_checksum"
                ) && item.is_string()
                {
                    *item = Value::String("[hash]".to_owned());
                } else {
                    redact_hashes(item);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn redact_temp_path_names(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Some(start) = text.find(".voom-tmp.") {
                let prefix = &text[..start];
                let suffix = &text[start..];
                let mut parts = suffix.rsplitn(3, '.').collect::<Vec<_>>();
                if parts.len() == 3
                    && parts[0].chars().all(|c| c.is_ascii_digit())
                    && parts[1].chars().all(|c| c.is_ascii_digit())
                {
                    parts.reverse();
                    *text = format!("{prefix}{}.[temp]", parts[0]);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_temp_path_names(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_temp_path_names(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_long_target_names(value: &mut Value) {
    let long_name = "x".repeat(240);
    replace_string(
        value,
        &format!("[artifact-dir]/{long_name}.mp4"),
        "[artifact-dir]/long-target.mp4",
    );
    replace_string(
        value,
        &format!("[artifact-dir]/.voom-tmp.{long_name}.mp4.[temp]"),
        "[artifact-dir]/.voom-tmp.long-target.mp4.[temp]",
    );
}

fn redact_worker_ids(value: &mut Value) {
    replace_key_value(value, "worker_id", &Value::String("[worker-id]".to_owned()));
    replace_key_value(
        value,
        "probe_worker_id",
        &Value::String("[worker-id]".to_owned()),
    );
}

fn redact_local_file_keys(value: &mut Value) {
    replace_key_value(
        value,
        "local_file_key",
        &Value::String("[local-file-key]".to_owned()),
    );
}

fn replace_key_value(value: &mut Value, key: &str, replacement: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                replace_key_value(item, key, replacement);
            }
        }
        Value::Object(map) => {
            for (item_key, item) in map {
                if item_key == key {
                    *item = replacement.clone();
                } else {
                    replace_key_value(item, key, replacement);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn replace_string(value: &mut Value, needle: &str, replacement: &str) {
    match value {
        Value::String(text) => *text = text.replace(needle, replacement),
        Value::Array(items) => {
            for item in items {
                replace_string(item, needle, replacement);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                replace_string(item, needle, replacement);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
