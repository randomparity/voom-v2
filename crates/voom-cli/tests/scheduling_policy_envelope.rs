#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;

mod scheduling_policy_envelope {
    use super::*;

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        Fixture { _tmp: tmp, url }
    }

    fn cli(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "scheduling-policy"]);
        command
    }

    fn seed_home(url: &str) {
        let status = cli(url)
            .args([
                "create",
                "--slug",
                "home",
                "--display-name",
                "Home library default",
                "--priority",
                "newest_first",
                "--copy-window",
                "00:00-08:00",
                "--large-jobs-night-only",
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    /// Replace clock-driven fields with placeholders so the snapshot is stable.
    fn redact(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
        redact_timestamps(&mut json["data"]);
    }

    fn redact_timestamps(node: &mut Value) {
        if let Some(policies) = node.get_mut("policies").and_then(Value::as_array_mut) {
            for policy in policies {
                stamp(policy);
            }
        } else {
            stamp(node);
        }
    }

    fn stamp(policy: &mut Value) {
        if policy.get("created_at").is_some() {
            policy["created_at"] = Value::String("[ts]".to_owned());
            policy["updated_at"] = Value::String("[ts]".to_owned());
        }
    }

    #[tokio::test]
    async fn create_outputs_the_record() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args([
                "create",
                "--slug",
                "home",
                "--display-name",
                "Home library default",
                "--priority",
                "newest_first",
                "--copy-window",
                "00:00-08:00",
                "--large-jobs-night-only",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "scheduling-policy");
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("create_outputs_the_record", json);
    }

    #[tokio::test]
    async fn list_outputs_records() {
        let fixture = fixture().await;
        seed_home(&fixture.url);
        let output = cli(&fixture.url).arg("list").output().unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("list_outputs_records", json);
    }

    #[tokio::test]
    async fn update_replaces_fields() {
        let fixture = fixture().await;
        seed_home(&fixture.url);
        let output = cli(&fixture.url)
            .args([
                "update",
                "--slug",
                "home",
                "--display-name",
                "Renamed",
                "--priority",
                "largest_first",
                "--pause-on-degraded-node",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("update_replaces_fields", json);
    }

    #[tokio::test]
    async fn delete_reports_success() {
        let fixture = fixture().await;
        seed_home(&fixture.url);
        let output = cli(&fixture.url)
            .args(["delete", "--slug", "home"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("delete_reports_success", json);
    }

    #[tokio::test]
    async fn show_unknown_slug_is_not_found() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args(["show", "--slug", "missing"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("show_unknown_slug_is_not_found", json);
    }

    #[tokio::test]
    async fn create_rejects_bad_copy_window() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args([
                "create",
                "--slug",
                "bad",
                "--display-name",
                "Bad",
                "--priority",
                "newest_first",
                "--copy-window",
                "8am-4pm",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "CONFIG_INVALID");
        redact(&mut json);
        insta::assert_json_snapshot!("create_rejects_bad_copy_window", json);
    }
}
