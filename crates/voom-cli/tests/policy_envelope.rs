#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests favor unwrap/panic over plumbing Result<()> through every assertion"
)]

use std::process::{Command, Output};

use serde_json::Value;
use tempfile::NamedTempFile;
use voom_store::test_support::sqlite_url_for;

const MINIMAL_V1: &str = "policy \"minimal\" {\n  phase inspect {\n    container mkv\n  }\n}\n";
const MINIMAL_V2: &str = "policy \"minimal\" {\n  phase inspect {\n    container mkv\n  }\n}\n\n";
const BROKEN: &str = "policy \"broken\" {\n";

#[tokio::test]
async fn policy_create_lists_and_shows_document() {
    let seeded = seed().await;
    let file = write_policy(MINIMAL_V1);

    let output = create_command(&seeded.url, "minimal", file.path().to_str().unwrap())
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    let document_id = json["data"]["document"]["document_id"].as_u64().unwrap();
    assert!(document_id > 0);
    assert_eq!(json["data"]["document"]["slug"], "minimal");
    let version_id = json["data"]["version"]["version_id"].as_u64().unwrap();
    assert!(version_id > 0);
    assert_eq!(json["data"]["version"]["version_number"], 1);
    assert_eq!(
        json["data"]["document"]["current_accepted_version_id"],
        version_id
    );

    let list = list_command(&seeded.url).output().unwrap();
    assert_status(&list, Some(0));
    let list_json = envelope(list.stdout);
    assert_eq!(list_json["command"], "policy");
    let documents = list_json["data"]["documents"].as_array().unwrap();
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0]["document_id"].as_u64().unwrap(), document_id);
    assert_eq!(documents[0]["slug"], "minimal");

    let show = show_command(&seeded.url, document_id).output().unwrap();
    assert_status(&show, Some(0));
    let show_json = envelope(show.stdout);
    assert_eq!(show_json["data"]["document"]["document_id"], document_id);
    let versions = show_json["data"]["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["version_id"].as_u64().unwrap(), version_id);
}

#[tokio::test]
async fn policy_version_add_appends_new_version() {
    let seeded = seed().await;
    let v1 = write_policy(MINIMAL_V1);
    let create = create_command(&seeded.url, "minimal", v1.path().to_str().unwrap())
        .output()
        .unwrap();
    assert_status(&create, Some(0));
    let create_json = envelope(create.stdout);
    let document_id = create_json["data"]["document"]["document_id"]
        .as_u64()
        .unwrap();

    let v2 = write_policy(MINIMAL_V2);
    let output = version_add_command(&seeded.url, document_id, v2.path().to_str().unwrap())
        .output()
        .unwrap();

    assert_status(&output, Some(0));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "ok");
    assert_eq!(json["data"]["version"]["document_id"], document_id);
    assert_eq!(json["data"]["version"]["version_number"], 2);

    let show = show_command(&seeded.url, document_id).output().unwrap();
    let show_json = envelope(show.stdout);
    assert_eq!(show_json["data"]["versions"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn policy_create_duplicate_slug_is_conflict() {
    let seeded = seed().await;
    let file = write_policy(MINIMAL_V1);
    let first = create_command(&seeded.url, "minimal", file.path().to_str().unwrap())
        .output()
        .unwrap();
    assert_status(&first, Some(0));

    let again = write_policy(MINIMAL_V1);
    let output = create_command(&seeded.url, "minimal", again.path().to_str().unwrap())
        .output()
        .unwrap();

    assert_status(&output, Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "CONFLICT");
}

#[tokio::test]
async fn policy_create_invalid_source_is_compile_error() {
    let seeded = seed().await;
    let file = write_policy(BROKEN);

    let output = create_command(&seeded.url, "broken", file.path().to_str().unwrap())
        .output()
        .unwrap();

    assert_status(&output, Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "POLICY_PARSE_ERROR");
}

#[tokio::test]
async fn policy_create_missing_file_is_bad_args() {
    let seeded = seed().await;

    let output = create_command(&seeded.url, "minimal", "/nonexistent/policy.voom")
        .output()
        .unwrap();

    assert_status(&output, Some(1));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "BAD_ARGS");
}

#[tokio::test]
async fn policy_show_missing_document_is_not_found() {
    let seeded = seed().await;

    let output = show_command(&seeded.url, 999_999).output().unwrap();

    assert_status(&output, Some(2));
    let json = envelope(output.stdout);
    assert_eq!(json["command"], "policy");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "NOT_FOUND");
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

fn write_policy(source: &str) -> NamedTempFile {
    let file = NamedTempFile::new().unwrap();
    std::fs::write(file.path(), source).unwrap();
    file
}

fn create_command(url: &str, slug: &str, file: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "create",
        "--slug",
        slug,
        "--file",
        file,
    ]);
    command
}

fn version_add_command(url: &str, document_id: u64, file: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "version",
        "add",
        "--document-id",
        &document_id.to_string(),
        "--file",
        file,
    ]);
    command
}

fn list_command(url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args(["--database-url", url, "policy", "list"]);
    command
}

fn show_command(url: &str, document_id: u64) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom"));
    command.args([
        "--database-url",
        url,
        "policy",
        "show",
        "--document-id",
        &document_id.to_string(),
    ]);
    command
}

fn assert_status(output: &Output, expected: Option<i32>) {
    assert_eq!(
        output.status.code(),
        expected,
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn envelope(stdout: Vec<u8>) -> Value {
    let stdout = String::from_utf8(stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|err| panic!("stdout must be one JSON envelope; got {stdout:?}: {err}"))
}
