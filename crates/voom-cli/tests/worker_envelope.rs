#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_control_plane::ControlPlane;
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::sqlite_url_for;

mod worker_envelope {
    use super::*;

    #[tokio::test]
    async fn worker_register_requires_node_token_and_capability() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "node-a");

        let no_token = worker_command(&seeded.url)
            .args([
                "register",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--name",
                "worker-a",
                "--kind",
                "local",
                "--capability",
                "hash_file",
            ])
            .output()
            .unwrap();
        assert_eq!(no_token.status.code(), Some(1));
        let mut no_token_json = envelope(no_token.stdout);
        assert_eq!(no_token_json["command"], "worker");
        assert_eq!(no_token_json["status"], "error");
        assert_eq!(no_token_json["error"]["code"], "BAD_ARGS");
        assert_eq!(
            no_token_json["error"]["hint"],
            "Pass exactly one token source"
        );
        redact_local(&mut no_token_json);
        insta::assert_json_snapshot!("worker_register_requires_node_token", no_token_json);

        let no_capability = worker_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", &registered.token)
            .args([
                "register",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--name",
                "worker-a",
                "--kind",
                "local",
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();
        assert_eq!(no_capability.status.code(), Some(1));
        let mut no_capability_json = envelope(no_capability.stdout);
        assert_eq!(no_capability_json["command"], "worker");
        assert_eq!(no_capability_json["status"], "error");
        assert_eq!(no_capability_json["error"]["code"], "BAD_ARGS");
        redact_local(&mut no_capability_json);
        insta::assert_json_snapshot!("worker_register_requires_capability", no_capability_json);
    }

    #[tokio::test]
    async fn worker_register_with_valid_node_token_outputs_node_context() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "node-a");

        let output = worker_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", &registered.token)
            .args([
                "register",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--name",
                "worker-a",
                "--kind",
                "local",
                "--capability",
                "hash_file",
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_linked_node_context(&json["data"]["worker"], registered.node_id, "node-a");
        redact_worker_timestamps(&mut json["data"]["worker"]);
        redact_local(&mut json);
        insta::assert_json_snapshot!(
            "worker_register_with_valid_node_token_outputs_node_context",
            json
        );
    }

    #[tokio::test]
    async fn worker_register_bad_token_returns_conflict_and_no_worker() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "node-a");

        let output = worker_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", "voom-node-v1.invalid")
            .args([
                "register",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--name",
                "worker-bad-token",
                "--kind",
                "local",
                "--capability",
                "hash_file",
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "worker");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "CONFLICT");
        redact_local(&mut json);
        insta::assert_json_snapshot!("worker_register_bad_token_returns_conflict", json);

        let pool = voom_store::connect(&seeded.url).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workers WHERE name = ?")
            .bind("worker-bad-token")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn worker_list_shows_linked_node_and_legacy_null_node() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "node-a");
        register_worker(&seeded.url, &registered, "linked");
        seed_legacy_worker(&seeded.url).await;

        let output = worker_command(&seeded.url).args(["list"]).output().unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        let workers = json["data"]["workers"].as_array().unwrap();
        let linked = workers
            .iter()
            .find(|worker| worker["name"] == "linked")
            .unwrap();
        assert_linked_node_context(linked, registered.node_id, "node-a");
        let legacy = workers
            .iter()
            .find(|worker| worker["name"] == "legacy")
            .unwrap();
        assert_eq!(legacy["node"], Value::Null);
        for worker in json["data"]["workers"].as_array_mut().unwrap() {
            redact_worker_timestamps(worker);
        }
        redact_local(&mut json);
        insta::assert_json_snapshot!("worker_list_shows_linked_node_and_legacy_null_node", json);
    }

    #[tokio::test]
    async fn worker_show_shows_linked_node_context() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "node-a");
        let worker_id = register_worker(&seeded.url, &registered, "worker-a");

        let output = worker_command(&seeded.url)
            .args(["show", "--worker-id", worker_id.to_string().as_str()])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_linked_node_context(&json["data"]["worker"], registered.node_id, "node-a");
        redact_worker_timestamps(&mut json["data"]["worker"]);
        redact_local(&mut json);
        insta::assert_json_snapshot!("worker_show_shows_linked_node_context", json);
    }

    struct Seeded {
        _tmp: NamedTempFile,
        url: String,
    }

    struct Registered {
        node_id: u64,
        token: String,
    }

    async fn seed() -> Seeded {
        let tmp = NamedTempFile::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        Seeded { _tmp: tmp, url }
    }

    async fn seed_legacy_worker(url: &str) {
        let cp = ControlPlane::open(url).await.unwrap();
        cp.register_worker(NewWorker {
            name: "legacy".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: cp.clock().now(),
            node_id: None,
        })
        .await
        .unwrap();
    }

    fn register_node(url: &str, name: &str) -> Registered {
        let output = node_command(url)
            .args(["register", "--name", name, "--kind", "local"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let json = envelope(output.stdout);
        Registered {
            node_id: json["data"]["node"]["id"].as_u64().unwrap(),
            token: json["data"]["token"].as_str().unwrap().to_owned(),
        }
    }

    fn register_worker(url: &str, registered: &Registered, name: &str) -> u64 {
        let output = worker_command(url)
            .env("VOOM_TEST_NODE_TOKEN", &registered.token)
            .args([
                "register",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--name",
                name,
                "--kind",
                "local",
                "--capability",
                "hash_file",
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        let json = envelope(output.stdout);
        json["data"]["worker"]["id"].as_u64().unwrap()
    }

    fn node_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "node"]);
        command
    }

    fn worker_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "worker"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn assert_linked_node_context(worker: &Value, node_id: u64, node_name: &str) {
        assert_eq!(worker["node"]["id"], node_id);
        assert_eq!(worker["node"]["name"], node_name);
        assert_eq!(worker["node"]["kind"], "local");
        assert_eq!(worker["node"]["status"], "registered");
        assert!(worker["node"].get("last_seen_at").is_some());
    }

    fn redact_worker_timestamps(worker: &mut Value) {
        worker["registered_at"] = Value::String("[registered-at]".to_owned());
        worker["last_seen_at"] = Value::String("[last-seen-at]".to_owned());
        if worker["retired_at"].is_string() {
            worker["retired_at"] = Value::String("[retired-at]".to_owned());
        }
        if worker["node"].is_object() {
            worker["node"]["last_seen_at"] = Value::String("[node-last-seen-at]".to_owned());
        }
    }

    fn redact_local(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    }
}
