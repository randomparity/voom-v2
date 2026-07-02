#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;

mod external_system_envelope {
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
        command.args(["--database-url", url, "external-system"]);
        command
    }

    /// Register a filesystem system with no path mappings and return its id (1
    /// on a fresh database).
    fn seed_filesystem(url: &str) {
        let status = cli(url)
            .args(["register", "--kind", "filesystem", "--display-name", "Home"])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
        redact_timestamps(&mut json["data"]);
    }

    /// Replace every timestamp-shaped field anywhere under `node` with `[ts]`.
    fn redact_timestamps(node: &mut Value) {
        const STAMPS: &[&str] = &[
            "created_at",
            "updated_at",
            "retired_at",
            "started_at",
            "finished_at",
            "last_started_at",
            "last_finished_at",
        ];
        match node {
            Value::Object(map) => {
                for key in STAMPS {
                    if let Some(v) = map.get_mut(*key)
                        && !v.is_null()
                    {
                        *v = Value::String("[ts]".to_owned());
                    }
                }
                for (_, v) in map.iter_mut() {
                    redact_timestamps(v);
                }
            }
            Value::Array(items) => {
                for item in items {
                    redact_timestamps(item);
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn register_outputs_record() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args([
                "register",
                "--kind",
                "plex",
                "--display-name",
                "Home Plex",
                "--connection-profile",
                r#"{"host":"127.0.0.1"}"#,
                "--auth-ref",
                "keyring://voom/plex",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["command"], "external-system");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["data"]["health_status"], "unknown");
        redact(&mut json);
        insta::assert_json_snapshot!("register_outputs_record", json);
    }

    #[tokio::test]
    async fn list_outputs_records() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        let output = cli(&fixture.url).arg("list").output().unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("list_outputs_records", json);
    }

    #[tokio::test]
    async fn show_unknown_id_is_not_found() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args(["show", "--id", "999"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("show_unknown_id_is_not_found", json);
    }

    #[tokio::test]
    async fn register_rejects_invalid_connection_profile() {
        let fixture = fixture().await;
        let output = cli(&fixture.url)
            .args([
                "register",
                "--kind",
                "custom",
                "--display-name",
                "Bad",
                "--connection-profile",
                "not json",
            ])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1));
        let mut json = envelope(output.stdout);
        assert_eq!(json["error"]["code"], "BAD_ARGS");
        redact(&mut json);
        insta::assert_json_snapshot!("register_rejects_invalid_connection_profile", json);
    }

    #[tokio::test]
    async fn health_check_records_unknown_for_system_without_mappings() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        let output = cli(&fixture.url)
            .args(["health-check", "--id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["health_status"], "unknown");
        redact(&mut json);
        insta::assert_json_snapshot!("health_check_records_unknown", json);
    }

    #[tokio::test]
    async fn sync_outputs_report() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        let output = cli(&fixture.url)
            .args(["sync", "--id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["last_outcome"], "ok");
        assert_eq!(json["data"]["last_links_recorded"], 0);
        redact(&mut json);
        insta::assert_json_snapshot!("sync_outputs_report", json);
    }

    #[tokio::test]
    async fn sync_report_before_first_sync_is_empty() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        let output = cli(&fixture.url)
            .args(["sync-report", "--id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["data"]["last_outcome"], Value::Null);
        redact(&mut json);
        insta::assert_json_snapshot!("sync_report_before_first_sync", json);
    }

    #[tokio::test]
    async fn path_mapping_create_and_list() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        let created = cli(&fixture.url)
            .args([
                "path-mapping",
                "create",
                "--system-id",
                "1",
                "--internal-prefix",
                "/srv/media",
                "--external-prefix",
                "/data",
                "--visibility",
                "read_only",
            ])
            .output()
            .unwrap();
        assert_eq!(created.status.code(), Some(0));
        let mut json = envelope(created.stdout);
        assert_eq!(json["command"], "external-system path-mapping");
        redact(&mut json);
        insta::assert_json_snapshot!("path_mapping_create_outputs_record", json);

        let listed = cli(&fixture.url)
            .args(["path-mapping", "list", "--system-id", "1"])
            .output()
            .unwrap();
        assert_eq!(listed.status.code(), Some(0));
        let mut json = envelope(listed.stdout);
        redact(&mut json);
        insta::assert_json_snapshot!("path_mapping_list_outputs_records", json);
    }

    #[tokio::test]
    async fn path_mapping_delete_reports_success() {
        let fixture = fixture().await;
        seed_filesystem(&fixture.url);
        cli(&fixture.url)
            .args([
                "path-mapping",
                "create",
                "--system-id",
                "1",
                "--internal-prefix",
                "/srv/media",
                "--external-prefix",
                "/data",
            ])
            .output()
            .unwrap();
        let output = cli(&fixture.url)
            .args(["path-mapping", "delete", "--id", "1"])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(0));
        let mut json = envelope(output.stdout);
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("path_mapping_delete_reports_success", json);
    }
}
