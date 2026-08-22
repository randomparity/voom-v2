#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly with complete process output"
)]

use std::path::Path;
use std::process::{Command, Output};

use secrecy::ExposeSecret as _;
use serde_json::Value;
use voom_control_plane::ControlPlane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_core::{NodeKind, StorageRootId};
use voom_store::test_support::sqlite_url_for;

const POLICY: &str = "policy \"empty tools\" {\n  \
    metadata { requires_tools: [mkvtoolnix] }\n  \
    phase normalize { container mkv }\n\
}\n";

#[tokio::test]
async fn empty_whole_scan_runs_end_to_end_as_zero_work_without_workers() {
    let dir = tempfile::tempdir().unwrap();
    let database = voom_test_support::TempDatabase::new_in(dir.path()).unwrap();
    let database_url = sqlite_url_for(database.path());
    voom_store::init(&database_url).await.unwrap();
    let cp = ControlPlane::open(&database_url).await.unwrap();
    let owner = cp
        .register_node(RegisterNodeInput {
            name: "empty-root-owner".to_owned(),
            kind: NodeKind::Local,
            heartbeat_ttl_seconds: 60,
            metadata: serde_json::json!({}),
        })
        .await
        .unwrap();
    cp.heartbeat_node(owner.node.id, owner.token.expose_secret())
        .await
        .unwrap();

    let input_set_id = create_empty_inputs(&database_url, dir.path()).await;
    let policy_version_id = create_policy(&database_url, dir.path());
    assert_preview(&database_url, policy_version_id, input_set_id);
    let job_id = execute_empty_input(&database_url, policy_version_id, input_set_id);
    assert_run_report(&database_url, job_id);
    assert_durable_zero_work(&database_url).await;
}

async fn create_empty_inputs(database_url: &str, root: &Path) -> u64 {
    let input = ok(run(
        database_url,
        &[
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "empty-process-input",
            "--all",
        ],
    ));
    assert_eq!(input["command"], "policy");
    assert_eq!(input["data"]["input_set"]["included_count"], 0);
    assert_eq!(input["data"]["input_set"]["skipped_count"], 0);
    let input_set_id = input["data"]["input_set"]["input_set_id"].as_u64().unwrap();

    let empty_root = root.join("empty-root");
    std::fs::create_dir(&empty_root).unwrap();
    ok(run(
        database_url,
        &[
            "library",
            "add",
            "--slug",
            "empty-library",
            "--display-name",
            "Empty library",
        ],
    ));
    ok(run(
        database_url,
        &[
            "library",
            "root",
            "add",
            "--library-id",
            "1",
            "--owner-node-id",
            "1",
            "--provider",
            "local_filesystem",
            "--provider-locator",
            path_str(&empty_root),
        ],
    ));
    ControlPlane::open(database_url)
        .await
        .unwrap()
        .activate_library_root(StorageRootId(1), "empty-root-fixture".to_owned())
        .await
        .unwrap();
    // An empty root still accepts a durable scan request; `--no-wait` asserts
    // the request envelope without pumping the session (no agent is attached).
    let scan_request = ok(run(database_url, &["scan", "--root", "1", "--no-wait"]));
    assert_eq!(scan_request["command"], "scan");
    assert_eq!(scan_request["status"], "ok");
    assert!(scan_request["data"]["scan_session_id"].as_u64().unwrap() > 0);
    assert!(scan_request["data"]["ticket_id"].as_u64().unwrap() > 0);
    let root_input = ok(run(
        database_url,
        &[
            "policy",
            "input",
            "create-from-scan",
            "--slug",
            "empty-root-input",
            "--root",
            "1",
        ],
    ));
    assert_eq!(root_input["data"]["input_set"]["library_root_id"], 1);
    assert_eq!(root_input["data"]["input_set"]["included_count"], 0);
    assert_eq!(root_input["data"]["input_set"]["skipped_count"], 0);
    input_set_id
}

fn create_policy(database_url: &str, root: &Path) -> u64 {
    let policy_path = root.join("empty-tools.voom");
    std::fs::write(&policy_path, POLICY).unwrap();
    let policy = ok(run(
        database_url,
        &[
            "policy",
            "create",
            "--slug",
            "empty-tools",
            "--file",
            path_str(&policy_path),
        ],
    ));
    policy["data"]["version"]["version_id"].as_u64().unwrap()
}

fn assert_preview(database_url: &str, policy_version_id: u64, input_set_id: u64) {
    let preview = ok(run(
        database_url,
        &[
            "compliance",
            "report",
            "--policy-version-id",
            &policy_version_id.to_string(),
            "--input-set-id",
            &input_set_id.to_string(),
        ],
    ));
    assert_eq!(preview["command"], "compliance");
    assert_eq!(preview["data"]["plan"]["summary"]["total_node_count"], 0);
    assert_eq!(preview["data"]["report"]["summary"]["total_check_count"], 0);
}

fn execute_empty_input(database_url: &str, policy_version_id: u64, input_set_id: u64) -> u64 {
    let execute = ok(run(
        database_url,
        &[
            "compliance",
            "execute",
            "--policy-version-id",
            &policy_version_id.to_string(),
            "--input-set-id",
            &input_set_id.to_string(),
        ],
    ));
    assert_eq!(execute["data"]["summary"]["branch_count"], 0);
    assert_eq!(execute["data"]["summary"]["ticket_count"], 0);
    assert_eq!(execute["data"]["summary"]["dispatch_count"], 0);
    assert_eq!(execute["data"]["summary"]["failure_count"], 0);
    assert_eq!(execute["data"]["summary"]["progress"]["total"], 0);
    assert_eq!(execute["data"]["phases"], serde_json::json!([]));
    assert_eq!(execute["data"]["file_phases"], serde_json::json!([]));
    execute["data"]["summary"]["job_id"].as_u64().unwrap()
}

fn assert_run_report(database_url: &str, job_id: u64) {
    let report = ok(run(
        database_url,
        &["compliance", "report", "--job-id", &job_id.to_string()],
    ));
    assert_eq!(report["data"]["summary"]["job_id"], job_id);
    assert_eq!(report["data"]["summary"]["ticket_count"], 0);
    assert_eq!(report["data"]["phases"], serde_json::json!([]));
    assert_eq!(report["data"]["file_phases"], serde_json::json!([]));
}

async fn assert_durable_zero_work(database_url: &str) {
    let pool = voom_store::connect(database_url).await.unwrap();
    let durable: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
         (SELECT COUNT(*) FROM policy_input_sets), \
         (SELECT COUNT(*) FROM policy_input_set_fixture_labels), \
         (SELECT COUNT(*) FROM policy_media_snapshot_inputs), \
         (SELECT COUNT(*) FROM workflow_summaries), \
         (SELECT COUNT(*) FROM workflow_phase_summaries), \
         (SELECT COUNT(*) FROM tickets), \
         (SELECT COUNT(*) FROM leases)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(durable, (2, 2, 0, 1, 0, 1, 0));

    let event_kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM events ORDER BY event_id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        event_kinds,
        [
            "schema.initialized",
            "node.registered",
            "node.heartbeat_recorded",
            "storage_root.created",
            "storage_root.activated",
            "ticket.created",
            "ticket.ready",
            "scan_session.requested",
            "job.opened",
            "job.succeeded",
        ]
    );
}

fn run(database_url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voom"))
        .arg("--database-url")
        .arg(database_url)
        .args(args)
        .output()
        .unwrap()
}

fn ok(output: Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|error| panic!("stdout must be one JSON envelope: {stdout:?}: {error}"));
    assert_eq!(value["status"], "ok");
    value
}

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap()
}
