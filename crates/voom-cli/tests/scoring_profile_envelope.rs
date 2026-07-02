#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_store::test_support::sqlite_url_for;

mod scoring_profile_envelope {
    use super::*;

    struct Fixture {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn fixture() -> Fixture {
        let tmp = NamedTempFile::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        Fixture { _tmp: tmp, url }
    }

    fn cli(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "scoring-profile"]);
        command
    }

    fn envelope(stdout: Vec<u8>) -> Value {
        let stdout = String::from_utf8(stdout).unwrap();
        serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"))
    }

    fn redact(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
        if json["data"].get("created_at").is_some() {
            json["data"]["created_at"] = Value::String("[ts]".to_owned());
        }
    }

    fn create_balanced(url: &str) {
        let status = cli(url)
            .args([
                "create",
                "--name",
                "balanced-home",
                "--definition",
                r#"{"weights":{"resolution":3}}"#,
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn create_outputs_the_record() {
        let fixture = fixture().await;
        let out = cli(&fixture.url)
            .args([
                "create",
                "--name",
                "balanced-home",
                "--version",
                "2",
                "--definition",
                r#"{"weights":{"resolution":3}}"#,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        let mut json = envelope(out.stdout);
        assert_eq!(json["command"], "scoring-profile");
        assert_eq!(json["data"]["version"], 2);
        redact(&mut json);
        insta::assert_json_snapshot!("scoring_profile_create", json);
    }

    #[tokio::test]
    async fn create_non_object_definition_is_config_error() {
        let fixture = fixture().await;
        let out = cli(&fixture.url)
            .args(["create", "--name", "scalar", "--definition", "5"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let json = envelope(out.stdout);
        assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    }

    #[tokio::test]
    async fn create_malformed_json_is_bad_args() {
        let fixture = fixture().await;
        let out = cli(&fixture.url)
            .args(["create", "--name", "broken", "--definition", "{not json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let json = envelope(out.stdout);
        assert_eq!(json["error"]["code"], "BAD_ARGS");
    }

    #[tokio::test]
    async fn duplicate_name_is_conflict() {
        let fixture = fixture().await;
        create_balanced(&fixture.url);
        let out = cli(&fixture.url)
            .args(["create", "--name", "balanced-home", "--definition", "{}"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let json = envelope(out.stdout);
        assert_eq!(json["error"]["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn show_unknown_is_not_found() {
        let fixture = fixture().await;
        let out = cli(&fixture.url)
            .args(["show", "--name", "nope"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let json = envelope(out.stdout);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn update_then_retire_hides_from_list() {
        let fixture = fixture().await;
        create_balanced(&fixture.url);

        let update = cli(&fixture.url)
            .args([
                "update",
                "--name",
                "balanced-home",
                "--version",
                "3",
                "--definition",
                r#"{"weights":{"hdr":5}}"#,
            ])
            .output()
            .unwrap();
        assert_eq!(update.status.code(), Some(0));
        assert_eq!(envelope(update.stdout)["data"]["version"], 3);

        let retire = cli(&fixture.url)
            .args(["retire", "--name", "balanced-home"])
            .output()
            .unwrap();
        assert_eq!(retire.status.code(), Some(0));
        assert!(envelope(retire.stdout)["data"]["retired_at"].is_string());

        let list = cli(&fixture.url).args(["list"]).output().unwrap();
        assert!(
            envelope(list.stdout)["data"]["profiles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
