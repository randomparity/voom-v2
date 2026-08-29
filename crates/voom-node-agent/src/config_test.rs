use std::path::Path;

use secrecy::ExposeSecret;
use tempfile::TempDir;

use voom_core::VoomError;

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
fn worker_accelerator_config_is_structured_and_rejects_unknown_fields() {
    let fixture = ConfigFixture::new("token");
    let descriptor = r#"

[workers.accelerator]
backend = "vaapi"
pci_address = "0000:f4:00.0"
device_name = "Radeon Pro"
driver_version = "Mesa 26.1"
encoders = ["hevc_vaapi"]
decoders = ["hevc"]
max_sessions = 2
"#;
    fixture.rewrite(&format!("{}{descriptor}", fixture.document));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not declare transcode_video"),
        "{error}"
    );

    let transcode_document = fixture.document.replace(
        "operations = [\"hash_file\"]",
        "operations = [\"transcode_video\"]",
    );
    let ffmpeg = fixture.write_dependency("ffmpeg", true);
    let ffprobe = fixture.write_dependency("ffprobe", true);
    let transcode_document = with_dependencies(
        &transcode_document,
        Some(ffmpeg.as_path()),
        Some(ffprobe.as_path()),
        None,
    );
    fixture.rewrite(&format!("{transcode_document}{descriptor}"));
    let loaded = AgentConfig::load(&fixture.config_path).unwrap();
    let accelerator = loaded.config.workers[0].accelerator.as_ref();
    assert!(accelerator.is_some());
    let Some(accelerator) = accelerator else {
        return;
    };
    assert_eq!(accelerator.hardware_token(), "vaapi:pci-0000:f4:00.0");

    fixture.rewrite(&format!(
        "{transcode_document}{descriptor}unknown_descriptor_field = true\n",
    ));
    assert!(AgentConfig::load(&fixture.config_path).is_err());
}

#[test]
fn config_accepts_absolute_dependency_paths_and_rejects_path_lookup() {
    let fixture = ConfigFixture::new("token");
    let ffprobe = fixture.write_dependency("ffprobe", true);
    let probe_document = fixture.document.replace(
        "operations = [\"hash_file\"]",
        "operations = [\"probe_file\"]",
    );
    let document = with_dependencies(&probe_document, None, Some(ffprobe.as_path()), None);
    fixture.rewrite(&document);
    let loaded = AgentConfig::load(&fixture.config_path).unwrap();
    assert_eq!(
        loaded.config.workers[0].dependencies.ffprobe_bin.as_deref(),
        Some(ffprobe.as_path())
    );
    let debug = format!("{loaded:?}");
    assert!(!debug.contains(&ffprobe.display().to_string()));
    assert!(debug.contains("[CONFIGURED]"));

    fixture.rewrite(&with_dependencies(
        &probe_document,
        None,
        Some(Path::new("ffprobe")),
        None,
    ));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert_eq!(error.error_code(), voom_core::ErrorCode::ConfigInvalid);
    assert!(
        error
            .to_string()
            .contains("dependencies.ffprobe_bin must be an absolute path"),
        "{error}"
    );
}

#[test]
fn config_rejects_missing_non_file_and_non_executable_dependencies() {
    let fixture = ConfigFixture::new("token");
    let missing = fixture.path("missing-ffprobe");
    let non_executable = fixture.write_dependency("non-executable-ffprobe", false);
    let probe_document = fixture.document.replace(
        "operations = [\"hash_file\"]",
        "operations = [\"probe_file\"]",
    );
    for (path, diagnostic) in [
        (missing.as_path(), "existing regular file"),
        (fixture.temp_path(), "regular file"),
        (non_executable.as_path(), "executable"),
    ] {
        fixture.rewrite(&with_dependencies(&probe_document, None, Some(path), None));
        let error = AgentConfig::load(&fixture.config_path).unwrap_err();
        assert_eq!(error.error_code(), voom_core::ErrorCode::ConfigInvalid);
        assert!(
            error.to_string().contains("dependencies.ffprobe_bin")
                && error.to_string().contains(diagnostic),
            "{error}"
        );
    }
}

#[test]
fn config_scopes_dependency_paths_to_the_workers_that_need_them() {
    let fixture = ConfigFixture::new("token");
    let ffmpeg = fixture.write_dependency("ffmpeg", true);
    let ffprobe = fixture.write_dependency("ffprobe", true);
    let nvidia_smi = fixture.write_dependency("nvidia-smi", true);

    fixture.rewrite(&with_dependencies(
        &fixture.document,
        Some(ffmpeg.as_path()),
        Some(ffprobe.as_path()),
        None,
    ));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dependencies.ffmpeg_bin is only valid for transcode_video"),
        "{error}"
    );

    let transcode = fixture.document.replace(
        "operations = [\"hash_file\"]",
        "operations = [\"transcode_video\"]",
    );
    fixture.rewrite(&transcode);
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dependencies.ffmpeg_bin is required for transcode_video"),
        "{error}"
    );
    fixture.rewrite(&with_dependencies(
        &transcode,
        Some(ffmpeg.as_path()),
        Some(ffprobe.as_path()),
        Some(nvidia_smi.as_path()),
    ));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dependencies.nvidia_smi_bin is only valid for an NVIDIA accelerator"),
        "{error}"
    );

    let nvidia = r#"

[workers.accelerator]
backend = "nvidia"
hardware_token = "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
device_uuid = "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
device_name = "RTX A6000"
driver_version = "595.80"
encoders = ["hevc_nvenc"]
decoders = ["hevc_cuvid"]
max_sessions = 2
"#;
    let dependencies = with_dependencies(
        &transcode,
        Some(ffmpeg.as_path()),
        Some(ffprobe.as_path()),
        None,
    );
    fixture.rewrite(&format!("{dependencies}{nvidia}"));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dependencies.nvidia_smi_bin is required for an NVIDIA accelerator"),
        "{error}"
    );
}

#[test]
fn config_requires_ffmpeg_and_ffprobe_paths_for_every_ffmpeg_operation() {
    let fixture = ConfigFixture::new("token");
    let ffmpeg = fixture.write_dependency("ffmpeg", true);
    let ffprobe = fixture.write_dependency("ffprobe", true);
    for operation in ["transcode_audio", "extract_audio"] {
        let document = fixture.document.replace(
            "operations = [\"hash_file\"]",
            &format!("operations = [\"{operation}\"]"),
        );
        fixture.rewrite(&document);
        let error = AgentConfig::load(&fixture.config_path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("dependencies.ffmpeg_bin is required"),
            "{operation}: {error}"
        );

        fixture.rewrite(&with_dependencies(
            &document,
            Some(ffmpeg.as_path()),
            Some(ffprobe.as_path()),
            None,
        ));
        assert!(
            AgentConfig::load(&fixture.config_path).is_ok(),
            "{operation}"
        );
    }
}

#[test]
fn config_rejects_unknown_fields_and_invalid_numeric_bounds() {
    let fixture = ConfigFixture::new("token");
    for (field, value) in [
        ("poll_interval_ms", "49"),
        ("poll_interval_ms", "5001"),
        ("lease_ttl_seconds", "4"),
        ("lease_ttl_seconds", "3601"),
        ("progress_idle_timeout_seconds", "4"),
        ("progress_idle_timeout_seconds", "3601"),
        ("shutdown_grace_seconds", "0"),
        ("shutdown_grace_seconds", "19"),
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
fn shutdown_grace_fits_the_supported_supervisor_stop_timeout() {
    let fixture = ConfigFixture::new("token");
    fixture.rewrite(&replace_assignment(
        &fixture.document,
        "shutdown_grace_seconds",
        "18",
    ));
    assert!(AgentConfig::load(&fixture.config_path).is_ok());

    fixture.rewrite(&replace_assignment(
        &fixture.document,
        "shutdown_grace_seconds",
        "19",
    ));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert_eq!(error.error_code(), voom_core::ErrorCode::ConfigInvalid);
    let diagnostic = error.to_string();
    for required in [
        "shutdown_grace_seconds must be between 1 and 18; got 19",
        "supported supervisor stop timeout is 45 seconds",
        "shutdown_grace_seconds + 26",
        "change the worker's shutdown behavior",
    ] {
        assert!(diagnostic.contains(required), "{diagnostic}");
    }
}

/// The commit coordinator must converge inside the control plane's
/// `COMMIT_CONVERGENCE_TIMEOUT` (10 s): the poll bound is capped below it and a
/// violation names that coupling.
#[test]
fn config_caps_poll_interval_below_commit_convergence_timeout() {
    let fixture = ConfigFixture::new("token");

    fixture.rewrite(&replace_assignment(
        &fixture.document,
        "poll_interval_ms",
        "5000",
    ));
    assert!(
        AgentConfig::load(&fixture.config_path).is_ok(),
        "poll_interval_ms = 5000 sits at the cap and must be accepted"
    );

    fixture.rewrite(&replace_assignment(
        &fixture.document,
        "poll_interval_ms",
        "5001",
    ));
    let error = AgentConfig::load(&fixture.config_path).unwrap_err();
    assert!(
        matches!(&error, VoomError::Config(message)
            if message.contains("COMMIT_CONVERGENCE_TIMEOUT")),
        "the rejection must name the convergence-timeout coupling, got {error:?}"
    );
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
            .replace("operations = [\"hash_file\"]", "operations = []"),
        fixture.document.replace(
            "operations = [\"hash_file\"]",
            "operations = [\"hash_file\", \"hash_file\"]",
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
    temp: TempDir,
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
            temp,
            config_path,
            token_path,
            document,
        }
    }

    fn rewrite(&self, document: &str) {
        std::fs::write(&self.config_path, document).unwrap();
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.temp.path().join(name)
    }

    fn temp_path(&self) -> &Path {
        self.temp.path()
    }

    fn write_dependency(&self, name: &str, executable: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = self.path(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mode = if executable { 0o700 } else { 0o600 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
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
operations = ["hash_file"]
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
operations = ["hash_file"]
artifact_access = ["shared_mount"]
max_parallel = 1
"#
    )
}

fn with_dependencies(
    document: &str,
    ffmpeg_bin: Option<&Path>,
    ffprobe_bin: Option<&Path>,
    nvidia_smi_bin: Option<&Path>,
) -> String {
    use std::fmt::Write as _;

    let mut document = format!("{document}\n[workers.dependencies]\n");
    for (name, path) in [
        ("ffmpeg_bin", ffmpeg_bin),
        ("ffprobe_bin", ffprobe_bin),
        ("nvidia_smi_bin", nvidia_smi_bin),
    ] {
        if let Some(path) = path {
            writeln!(document, "{name} = {:?}", path.display().to_string()).unwrap();
        }
    }
    document
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
