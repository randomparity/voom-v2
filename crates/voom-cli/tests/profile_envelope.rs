#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_store::test_support::sqlite_url_for;

mod profile_envelope {
    use super::*;

    #[tokio::test]
    async fn profile_list_emits_seeded_builtins() {
        let seeded = seed().await;
        let out = profile_command(&seeded.url)
            .args(["list"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        let mut json = envelope(out.stdout);
        assert_eq!(json["command"], "profile");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("profile_list", json);
    }

    #[tokio::test]
    async fn profile_show_unknown_is_not_found() {
        let seeded = seed().await;
        let out = profile_command(&seeded.url)
            .args(["show", "--name", "nope"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let mut json = envelope(out.stdout);
        assert_eq!(json["command"], "profile");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact_local(&mut json);
        insta::assert_json_snapshot!("profile_show_unknown", json);
    }

    #[tokio::test]
    async fn profile_show_emits_full_profile() {
        let seeded = seed().await;
        let out = profile_command(&seeded.url)
            .args(["show", "--name", "hevc-archive"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        let mut json = envelope(out.stdout);
        assert_eq!(json["command"], "profile");
        assert_eq!(json["status"], "ok");
        redact_local(&mut json);
        insta::assert_json_snapshot!("profile_show_hevc_archive", json);
    }

    #[tokio::test]
    async fn create_then_show_round_trips_and_derives_codec() {
        let seeded = seed().await;
        let out = profile_command(&seeded.url)
            .args([
                "create",
                "--name",
                "home-hevc",
                "--encoder",
                "libx265",
                "--crf",
                "20",
                "--preset",
                "slow",
                "--codec-profile",
                "main10",
                "--pixel-format",
                "yuv420p10le",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        let mut json = envelope(out.stdout);
        assert_eq!(json["status"], "ok");
        assert_eq!(json["data"]["profile"]["target_codec"], "hevc");
        assert_eq!(json["data"]["profile"]["id"], "vp-home-hevc");
        redact_local(&mut json);
        insta::assert_json_snapshot!("profile_create", json);
    }

    #[tokio::test]
    async fn create_invalid_field_is_config_error() {
        let seeded = seed().await;
        let out = profile_command(&seeded.url)
            .args([
                "create",
                "--name",
                "bad",
                "--encoder",
                "libx265",
                "--crf",
                "60",
                "--preset",
                "slow",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let json = envelope(out.stdout);
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "CONFIG_INVALID");
    }

    #[tokio::test]
    async fn update_replaces_fields() {
        let seeded = seed().await;
        create_home(&seeded.url);
        let out = profile_command(&seeded.url)
            .args([
                "update",
                "--name",
                "home-hevc",
                "--encoder",
                "libsvtav1",
                "--crf",
                "32",
                "--preset",
                "8",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        let json = envelope(out.stdout);
        assert_eq!(json["data"]["profile"]["target_codec"], "av1");
        assert_eq!(json["data"]["profile"]["crf"], 32);
    }

    #[tokio::test]
    async fn retire_hides_from_list() {
        let seeded = seed().await;
        create_home(&seeded.url);
        let retire = profile_command(&seeded.url)
            .args(["retire", "--name", "home-hevc"])
            .output()
            .unwrap();
        assert_eq!(retire.status.code(), Some(0));
        let json = envelope(retire.stdout);
        assert!(json["data"]["profile"]["retired_at"].is_string());

        let list = profile_command(&seeded.url)
            .args(["list"])
            .output()
            .unwrap();
        let json = envelope(list.stdout);
        let names: Vec<&str> = json["data"]["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"home-hevc"));
    }

    fn create_home(url: &str) {
        let status = profile_command(url)
            .args([
                "create",
                "--name",
                "home-hevc",
                "--encoder",
                "libx265",
                "--crf",
                "20",
                "--preset",
                "slow",
            ])
            .status()
            .unwrap();
        assert!(status.success());
    }

    struct Seeded {
        _tmp: NamedTempFile,
        url: String,
    }

    async fn seed() -> Seeded {
        let tmp = NamedTempFile::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        Seeded { _tmp: tmp, url }
    }

    fn profile_command(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url, "profile"]);
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
}
