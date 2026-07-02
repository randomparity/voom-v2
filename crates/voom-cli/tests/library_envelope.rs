#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::Command;

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};

mod library_envelope {
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

    fn voom(url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
        command.args(["--database-url", url]);
        command
    }

    fn run(url: &str, args: &[&str]) -> (i32, Value) {
        let output = voom(url).args(args).output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let json = serde_json::from_str(stdout.trim())
            .unwrap_or_else(|e| panic!("stdout must be one JSON envelope; got {stdout:?}: {e}"));
        (output.status.code().unwrap(), json)
    }

    fn redact(json: &mut Value) {
        json["local"]["db_url"] = Value::String("[db-url]".to_owned());
        json["local"]["config_path"] = Value::String("[config-path]".to_owned());
        redact_walk(&mut json["data"]);
    }

    /// Timestamps and canonical filesystem paths are environment-specific;
    /// replace them recursively so snapshots stay stable.
    fn redact_walk(node: &mut Value) {
        match node {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    if key == "created_at" || key == "updated_at" {
                        *value = Value::String("[ts]".to_owned());
                    } else if key == "canonical_path" || key == "display_path" {
                        *value = Value::String("[path]".to_owned());
                    } else {
                        redact_walk(value);
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(redact_walk),
            _ => {}
        }
    }

    fn library_and_root(url: &str) -> (String, TempDir) {
        let (code, _) = run(
            url,
            &[
                "library",
                "add",
                "--slug",
                "films",
                "--display-name",
                "Films",
                "--media-kind",
                "movie",
            ],
        );
        assert_eq!(code, 0);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_owned();
        let (code, _) = run(
            url,
            &[
                "library",
                "root",
                "add",
                "--library-id",
                "1",
                "--path",
                &path,
            ],
        );
        assert_eq!(code, 0);
        (path, dir)
    }

    #[tokio::test]
    async fn library_add_outputs_record() {
        let fx = fixture().await;
        let (code, mut json) = run(
            &fx.url,
            &[
                "library",
                "add",
                "--slug",
                "films",
                "--display-name",
                "Films",
                "--media-kind",
                "movie",
                "--description",
                "home movies",
            ],
        );
        assert_eq!(code, 0);
        assert_eq!(json["command"], "library");
        assert_eq!(json["status"], "ok");
        redact(&mut json);
        insta::assert_json_snapshot!("library_add_outputs_record", json);
    }

    #[tokio::test]
    async fn library_list_and_update_and_disable() {
        let fx = fixture().await;
        run(
            &fx.url,
            &[
                "library",
                "add",
                "--slug",
                "films",
                "--display-name",
                "Films",
            ],
        );

        let (code, mut list) = run(&fx.url, &["library", "list"]);
        assert_eq!(code, 0);
        redact(&mut list);
        insta::assert_json_snapshot!("library_list", list);

        let (code, updated) = run(
            &fx.url,
            &[
                "library",
                "update",
                "--library-id",
                "1",
                "--display-name",
                "Renamed",
            ],
        );
        assert_eq!(code, 0);
        assert_eq!(updated["data"]["display_name"], "Renamed");

        let (code, disabled) = run(&fx.url, &["library", "disable", "--library-id", "1"]);
        assert_eq!(code, 0);
        assert_eq!(disabled["data"]["enabled"], false);
    }

    #[tokio::test]
    async fn library_show_missing_is_not_found() {
        let fx = fixture().await;
        let (code, mut json) = run(&fx.url, &["library", "show", "--library-id", "42"]);
        assert_eq!(code, 2);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("library_show_missing_is_not_found", json);
    }

    #[tokio::test]
    async fn library_remove_cascades() {
        let fx = fixture().await;
        library_and_root(&fx.url);
        let (code, removed) = run(&fx.url, &["library", "remove", "--library-id", "1"]);
        assert_eq!(code, 0);
        assert_eq!(removed["data"]["removed"], true);
        // The cascade removed the root too.
        let (code, roots) = run(&fx.url, &["library", "root", "list"]);
        assert_eq!(code, 0);
        assert_eq!(roots["data"]["roots"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn root_add_outputs_record() {
        let fx = fixture().await;
        let (_, dir) = library_and_root(&fx.url);
        let _ = &dir;
        let (code, mut json) = run(&fx.url, &["library", "root", "show", "--root-id", "1"]);
        assert_eq!(code, 0);
        assert_eq!(json["command"], "library");
        redact(&mut json);
        insta::assert_json_snapshot!("root_show_outputs_record", json);
    }

    #[tokio::test]
    async fn root_add_missing_library_is_not_found() {
        let fx = fixture().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_owned();
        let (code, mut json) = run(
            &fx.url,
            &[
                "library",
                "root",
                "add",
                "--library-id",
                "99",
                "--path",
                &path,
            ],
        );
        assert_eq!(code, 2);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
        redact(&mut json);
        insta::assert_json_snapshot!("root_add_missing_library_is_not_found", json);
    }

    #[tokio::test]
    async fn root_add_overlapping_is_conflict() {
        let fx = fixture().await;
        let (_, dir) = library_and_root(&fx.url);
        let nested = dir.path().join("2024");
        std::fs::create_dir(&nested).unwrap();
        let nested = nested.to_str().unwrap().to_owned();
        let (code, json) = run(
            &fx.url,
            &[
                "library",
                "root",
                "add",
                "--library-id",
                "1",
                "--path",
                &nested,
            ],
        );
        assert_eq!(code, 2);
        assert_eq!(json["error"]["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn scan_root_disabled_is_blocked() {
        let fx = fixture().await;
        library_and_root(&fx.url);
        run(&fx.url, &["library", "root", "disable", "--root-id", "1"]);
        let (code, mut json) = run(&fx.url, &["scan", "--root", "1"]);
        assert_eq!(code, 2);
        assert_eq!(json["command"], "scan");
        assert_eq!(json["error"]["code"], "BLOCKED");
        assert_eq!(json["data"]["status"], "blocked");
        assert_eq!(json["data"]["reason"], "root_disabled");
        redact(&mut json);
        insta::assert_json_snapshot!("scan_root_disabled_is_blocked", json);
    }

    #[tokio::test]
    async fn scan_root_missing_is_not_found() {
        let fx = fixture().await;
        let (code, json) = run(&fx.url, &["scan", "--root", "7"]);
        assert_eq!(code, 2);
        assert_eq!(json["error"]["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn scan_requires_path_or_root() {
        let fx = fixture().await;
        let output = voom(&fx.url).arg("scan").output().unwrap();
        // clap rejects "neither --path nor --root" at parse time (exit 1).
        assert_eq!(output.status.code(), Some(1));
    }
}
