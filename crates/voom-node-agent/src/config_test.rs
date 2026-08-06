use std::path::Path;

use secrecy::ExposeSecret;
use tempfile::TempDir;

use super::AgentConfig;

#[test]
fn valid_file_token_config_loads_without_exposing_the_secret() {
    let fixture = ConfigFixture::new("voom-node-v1.top-secret\n");
    let loaded = AgentConfig::load(&fixture.config_path).unwrap();

    assert_eq!(loaded.node_token.expose_secret(), "voom-node-v1.top-secret");
    assert_eq!(loaded.config.workers.len(), 1);
    let debug = format!("{loaded:?}");
    assert!(!debug.contains("top-secret"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn config_rejects_unknown_fields_and_invalid_numeric_bounds() {
    let fixture = ConfigFixture::new("token");
    for (field, value) in [
        ("poll_interval_ms", "49"),
        ("poll_interval_ms", "60001"),
        ("lease_ttl_seconds", "4"),
        ("lease_ttl_seconds", "3601"),
        ("progress_idle_timeout_seconds", "4"),
        ("progress_idle_timeout_seconds", "3601"),
        ("shutdown_grace_seconds", "0"),
        ("shutdown_grace_seconds", "61"),
    ] {
        fixture.rewrite(&replace_assignment(&fixture.document, field, value));
        assert!(
            AgentConfig::load(&fixture.config_path).is_err(),
            "{field}={value}"
        );
    }
    fixture.rewrite(&format!("{}\nunknown = true\n", fixture.document));
    assert!(AgentConfig::load(&fixture.config_path).is_err());
}

#[test]
fn config_rejects_invalid_worker_manifests() {
    let fixture = ConfigFixture::new("token");
    let no_workers = fixture.document[..fixture.document.find("[[workers]]").unwrap()].to_owned();
    for document in [
        no_workers,
        fixture
            .document
            .replace("program = \"/bin/echo\"", "program = \"echo\""),
        fixture
            .document
            .replace("name = \"echo\"", "name = \"Bad Name\""),
        fixture
            .document
            .replace("name = \"echo\"", &format!("name = \"{}\"", "a".repeat(65))),
        fixture
            .document
            .replace("operations = [\"probe_file\"]", "operations = []"),
        fixture.document.replace(
            "operations = [\"probe_file\"]",
            "operations = [\"probe_file\", \"probe_file\"]",
        ),
        fixture.document.replace(
            "artifact_access = [\"shared_mount\"]",
            "artifact_access = []",
        ),
        fixture.document.replace(
            "artifact_access = [\"shared_mount\"]",
            "artifact_access = [\"shared_mount\", \"shared_mount\"]",
        ),
        fixture
            .document
            .replace("max_parallel = 1", "max_parallel = 0"),
        fixture
            .document
            .replace("max_parallel = 1", "max_parallel = 257"),
        format!("{}\n{}", fixture.document, worker_document("echo")),
    ] {
        fixture.rewrite(&document);
        assert!(AgentConfig::load(&fixture.config_path).is_err());
    }

    let mut too_many = fixture.document.clone();
    for index in 1..65 {
        too_many.push_str(&worker_document(&format!("worker-{index}")));
    }
    fixture.rewrite(&too_many);
    assert!(AgentConfig::load(&fixture.config_path).is_err());
}

#[test]
fn config_rejects_invalid_urls_and_token_contents() {
    let fixture = ConfigFixture::new("token");
    for url in [
        "http://example.com:7443",
        "https://user@example.com:7443",
        "https://example.com/path",
        "https://example.com?query=yes",
        "https://example.com#fragment",
    ] {
        fixture.rewrite(&fixture.document.replace("http://127.0.0.1:7443", url));
        assert!(AgentConfig::load(&fixture.config_path).is_err(), "{url}");
    }

    for token in ["", "token\nwith-newline", "token\rwith-newline"] {
        std::fs::write(&fixture.token_path, token).unwrap();
        fixture.rewrite(&fixture.document);
        assert!(AgentConfig::load(&fixture.config_path).is_err());
    }
}

#[test]
fn config_requires_exactly_one_valid_token_source() {
    let fixture = ConfigFixture::new("token");
    for token_table in [
        "[node_token]\nsource = \"file\"",
        "[node_token]\nsource = \"env\"\nname = \"PATH\"\npath = \"/tmp/token\"",
        "[node_token]\nsource = \"file\"\npath = \"/tmp/token\"\nname = \"PATH\"",
        "[node_token]\nsource = \"other\"\nname = \"PATH\"",
    ] {
        let document = replace_token_table(&fixture.document, token_table);
        fixture.rewrite(&document);
        assert!(AgentConfig::load(&fixture.config_path).is_err());
    }

    let document = replace_token_table(
        &fixture.document,
        "[node_token]\nsource = \"env\"\nname = \"PATH\"",
    );
    fixture.rewrite(&document);
    assert!(AgentConfig::load(&fixture.config_path).is_ok());
}

#[test]
fn config_accepts_https_and_explicit_loopback_http_only() {
    let fixture = ConfigFixture::new("token");
    for url in [
        "https://control.example:7443",
        "http://localhost:7443",
        "http://[::1]:7443",
    ] {
        fixture.rewrite(&fixture.document.replace("http://127.0.0.1:7443", url));
        assert!(AgentConfig::load(&fixture.config_path).is_ok(), "{url}");
    }
}

struct ConfigFixture {
    _temp: TempDir,
    config_path: std::path::PathBuf,
    token_path: std::path::PathBuf,
    document: String,
}

impl ConfigFixture {
    fn new(token: &str) -> Self {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join("agent.toml");
        let token_path = temp.path().join("token");
        std::fs::write(&token_path, token).unwrap();
        let document = valid_document(&token_path);
        std::fs::write(&config_path, &document).unwrap();
        Self {
            _temp: temp,
            config_path,
            token_path,
            document,
        }
    }

    fn rewrite(&self, document: &str) {
        std::fs::write(&self.config_path, document).unwrap();
    }
}

fn valid_document(token_path: &Path) -> String {
    format!(
        r#"control_plane_url = "http://127.0.0.1:7443"
node_id = 7
poll_interval_ms = 1000
lease_ttl_seconds = 30
progress_idle_timeout_seconds = 300
shutdown_grace_seconds = 10

[node_token]
source = "file"
path = "{}"

[[workers]]
name = "echo"
program = "/bin/echo"
args = []
operations = ["probe_file"]
artifact_access = ["shared_mount"]
max_parallel = 1
"#,
        token_path.display()
    )
}

fn worker_document(name: &str) -> String {
    format!(
        r#"
[[workers]]
name = "{name}"
program = "/bin/echo"
args = []
operations = ["probe_file"]
artifact_access = ["shared_mount"]
max_parallel = 1
"#
    )
}

fn replace_assignment(document: &str, key: &str, value: &str) -> String {
    document.replace(
        &format!("{key} = {}", default_value(key)),
        &format!("{key} = {value}"),
    )
}

fn default_value(key: &str) -> &'static str {
    match key {
        "poll_interval_ms" => "1000",
        "lease_ttl_seconds" => "30",
        "progress_idle_timeout_seconds" => "300",
        "shutdown_grace_seconds" => "10",
        _ => "",
    }
}

fn replace_token_table(document: &str, replacement: &str) -> String {
    let start = document.find("[node_token]").unwrap();
    let end = document.find("[[workers]]").unwrap();
    format!(
        "{}{}\n\n{}",
        &document[..start],
        replacement,
        &document[end..]
    )
}
