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
