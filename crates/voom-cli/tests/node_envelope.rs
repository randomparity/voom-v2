#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_store::test_support::sqlite_url_for;

mod node_envelope {
    use super::*;

    #[tokio::test]
    async fn node_register_outputs_token_once() {
        let seeded = seed().await;

        let output = node_command(&seeded.url)
            .args([
                "register",
                "--name",
                "local-a",
                "--kind",
                "local",
                "--heartbeat-ttl-seconds",
                "60",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "node");
        assert_eq!(json["status"], "ok");
        assert!(
            json["data"].get("token").is_some(),
            "register must expose the one-time token"
        );
        assert!(
            json["data"]["node"].get("token").is_none(),
            "node payload must not duplicate the token"
        );
        assert!(
            json["data"]["node"].get("auth_token_hash").is_none(),
            "node payload must not expose auth_token_hash"
        );
        redact_local(&mut json);
        redact_token(&mut json);
        insta::assert_json_snapshot!("node_register_outputs_token_once", json);
    }

    #[tokio::test]
    async fn node_register_zero_ttl_returns_config_invalid() {
        let seeded = seed().await;

        let output = node_command(&seeded.url)
            .args([
                "register",
                "--name",
                "bad-ttl",
                "--kind",
                "local",
                "--heartbeat-ttl-seconds",
                "0",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "node");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "CONFIG_INVALID");
        redact_local(&mut json);
        insta::assert_json_snapshot!("node_register_zero_ttl_returns_config_invalid", json);
    }

    #[tokio::test]
    async fn node_show_and_list_do_not_expose_token_hash_or_plaintext() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "local-a");

        let show_output = node_command(&seeded.url)
            .args(["show", "--node-id", registered.node_id.to_string().as_str()])
            .output()
            .unwrap();
        assert_eq!(show_output.status.code(), Some(0));
        let mut show_json = envelope(show_output.stdout);
        assert_no_secret_fields(&show_json["data"]);
        redact_local(&mut show_json);
        insta::assert_json_snapshot!(
            "node_show_does_not_expose_token_hash_or_plaintext",
            show_json
        );

        let list_output = node_command(&seeded.url).args(["list"]).output().unwrap();
        assert_eq!(list_output.status.code(), Some(0));
        let mut list_json = envelope(list_output.stdout);
        assert_no_secret_fields(&list_json["data"]);
        redact_local(&mut list_json);
        insta::assert_json_snapshot!(
            "node_list_does_not_expose_token_hash_or_plaintext",
            list_json
        );
    }

    #[tokio::test]
    async fn node_heartbeat_with_env_token_activates_node() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "local-a");

        let output = node_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", &registered.token)
            .args([
                "heartbeat",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["node"]["status"], "active");
        assert_no_secret_fields(&json["data"]);
        redact_local(&mut json);
        insta::assert_json_snapshot!("node_heartbeat_with_env_token_activates_node", json);
    }

    #[tokio::test]
    async fn node_heartbeat_with_bad_token_returns_conflict_envelope() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "local-a");

        let output = node_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", "voom-node-v1.invalid")
            .args([
                "heartbeat",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "node");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "CONFLICT");
        redact_local(&mut json);
        insta::assert_json_snapshot!(
            "node_heartbeat_with_bad_token_returns_conflict_envelope",
            json
        );
    }

    #[tokio::test]
    async fn node_retire_outputs_retired_status() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "local-a");

        let output = node_command(&seeded.url)
            .args([
                "retire",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--expected-epoch",
                "0",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["node"]["status"], "retired");
        assert_no_secret_fields(&json["data"]);
        redact_local(&mut json);
        insta::assert_json_snapshot!("node_retire_outputs_retired_status", json);
    }

    #[tokio::test]
    async fn node_token_sources_are_mutually_exclusive_bad_args() {
        let seeded = seed().await;
        let registered = register_node(&seeded.url, "local-a");

        let output = node_command(&seeded.url)
            .env("VOOM_TEST_NODE_TOKEN", &registered.token)
            .args([
                "heartbeat",
                "--node-id",
                registered.node_id.to_string().as_str(),
                "--token-env",
                "VOOM_TEST_NODE_TOKEN",
                "--token-stdin",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "node");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "BAD_ARGS");
        redact_local(&mut json);
        insta::assert_json_snapshot!("node_token_sources_are_mutually_exclusive_bad_args", json);
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

    fn node_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "node"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact_local(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
    }

    fn redact_token(json: &mut Value) {
        json["data"]["token"] = Value::String("[token]".to_owned());
        json["data"]["token_hint"] = Value::String("[token-hint]".to_owned());
    }

    fn assert_no_secret_fields(value: &Value) {
        match value {
            Value::Object(map) => {
                assert!(!map.contains_key("token"), "payload must not expose token");
                assert!(
                    !map.contains_key("auth_token_hash"),
                    "payload must not expose auth_token_hash"
                );
                for child in map.values() {
                    assert_no_secret_fields(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    assert_no_secret_fields(child);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}
