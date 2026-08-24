use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use voom_core::{ArtifactAccessMode, NodeId, OperationKind, VoomError};
use voom_worker_protocol::VideoAcceleratorDescriptor;
/// Upper bound for the agent's poll interval, kept at half of the control
/// plane's `COMMIT_CONVERGENCE_TIMEOUT` (`Duration::from_secs(10)` in
/// voom-control-plane `artifact/commit`). voom-node-agent only depends on
/// that crate as a dev-dependency, so the deadline is restated here with this
/// comment as the coupling record: a poll interval at or above 10 s would let
/// every staged commit run out its convergence deadline and report
/// `CommitFailure`.
const MAX_COMMIT_POLL_INTERVAL_MS: u64 = 5_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub control_plane_url: String,
    pub ca_cert: Option<PathBuf>,
    pub node_id: NodeId,
    pub poll_interval_ms: u64,
    pub lease_ttl_seconds: u32,
    pub progress_idle_timeout_seconds: u32,
    pub shutdown_grace_seconds: u32,
    pub node_token: TokenSource,
    /// Filesystem provider locators backing the storage roots this node owns
    /// byte work for (ADR 0074 commit-intent executor).
    #[serde(default)]
    pub storage_roots: Vec<StorageRootBinding>,
    pub workers: Vec<WorkerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum TokenSource {
    File { path: PathBuf },
    Env { name: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfig {
    pub name: String,
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub operations: Vec<OperationKind>,
    pub artifact_access: Vec<ArtifactAccessMode>,
    #[serde(default)]
    pub dependencies: WorkerDependencyPaths,
    #[serde(default)]
    pub accelerator: Option<VideoAcceleratorDescriptor>,
    pub max_parallel: u32,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDependencyPaths {
    #[serde(default)]
    pub ffmpeg_bin: Option<PathBuf>,
    #[serde(default)]
    pub ffprobe_bin: Option<PathBuf>,
    #[serde(default)]
    pub nvidia_smi_bin: Option<PathBuf>,
}

impl std::fmt::Debug for WorkerDependencyPaths {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkerDependencyPaths")
            .field(
                "ffmpeg_bin",
                &self.ffmpeg_bin.as_ref().map(|_| "[CONFIGURED]"),
            )
            .field(
                "ffprobe_bin",
                &self.ffprobe_bin.as_ref().map(|_| "[CONFIGURED]"),
            )
            .field(
                "nvidia_smi_bin",
                &self.nvidia_smi_bin.as_ref().map(|_| "[CONFIGURED]"),
            )
            .finish()
    }
}

/// One storage root this node owns byte work for: the control plane addresses
/// staged and committed bytes by `(storage_root_id, relative_locator)`; this
/// binding supplies the filesystem root that relative locators resolve under.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageRootBinding {
    pub storage_root_id: u64,
    /// Absolute filesystem path of the root's provider locator.
    pub provider_locator: PathBuf,
}

#[derive(Clone)]
pub struct LoadedAgentConfig {
    pub config: AgentConfig,
    pub node_token: SecretString,
}

impl std::fmt::Debug for LoadedAgentConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedAgentConfig")
            .field("config", &self.config)
            .field("node_token", &"[REDACTED]")
            .finish()
    }
}

impl AgentConfig {
    /// Load and validate a node-agent configuration and its referenced token.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when the document, URL, worker manifest, or token
    /// violates the fail-closed agent configuration contract.
    pub fn load(path: &Path) -> Result<LoadedAgentConfig, VoomError> {
        let document = std::fs::read_to_string(path).map_err(|error| {
            config_error(format!("read configuration {}: {error}", path.display()))
        })?;
        let config: Self = toml::from_str(&document).map_err(|error| {
            config_error(format!("parse configuration {}: {error}", path.display()))
        })?;
        config.validate()?;
        let node_token = load_token(&config.node_token)?;
        Ok(LoadedAgentConfig { config, node_token })
    }

    pub(crate) fn validate(&self) -> Result<(), VoomError> {
        validate_url(&self.control_plane_url)?;
        if !(50..=MAX_COMMIT_POLL_INTERVAL_MS).contains(&self.poll_interval_ms) {
            return Err(config_error(format!(
                "poll_interval_ms must be between 50 and {MAX_COMMIT_POLL_INTERVAL_MS}; got {}. \
                 The commit coordinator must poll well inside the control plane's \
                 COMMIT_CONVERGENCE_TIMEOUT (10 s, voom-control-plane artifact/commit) or every \
                 staged commit reports CommitFailure",
                self.poll_interval_ms
            )));
        }
        validate_bound(
            "lease_ttl_seconds",
            u64::from(self.lease_ttl_seconds),
            5,
            3_600,
        )?;
        validate_bound(
            "progress_idle_timeout_seconds",
            u64::from(self.progress_idle_timeout_seconds),
            5,
            3_600,
        )?;
        validate_bound(
            "shutdown_grace_seconds",
            u64::from(self.shutdown_grace_seconds),
            1,
            60,
        )?;
        if !(1..=64).contains(&self.workers.len()) {
            return Err(config_error(format!(
                "workers must contain between 1 and 64 entries; got {}",
                self.workers.len()
            )));
        }

        let mut names = HashSet::with_capacity(self.workers.len());
        for worker in &self.workers {
            worker.validate()?;
            if !names.insert(worker.name.as_str()) {
                return Err(config_error(format!(
                    "worker name {:?} is duplicated",
                    worker.name
                )));
            }
        }

        let mut storage_root_ids = HashSet::with_capacity(self.storage_roots.len());
        for root in &self.storage_roots {
            if !root.provider_locator.is_absolute() {
                return Err(config_error(format!(
                    "storage root {} provider_locator {:?} must be an absolute path",
                    root.storage_root_id,
                    root.provider_locator.display()
                )));
            }
            if !storage_root_ids.insert(root.storage_root_id) {
                return Err(config_error(format!(
                    "storage root {} is bound more than once",
                    root.storage_root_id
                )));
            }
        }
        Ok(())
    }
}

impl WorkerConfig {
    fn validate(&self) -> Result<(), VoomError> {
        if self.name.is_empty()
            || self.name.len() > 64
            || !self.name.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(config_error(format!(
                "worker name {:?} must be 1..=64 bytes of lowercase ASCII letters, digits, '.', '_', or '-'",
                self.name
            )));
        }
        if !self.program.is_absolute() {
            return Err(config_error(format!(
                "worker {:?} program must be an absolute path; got {}",
                self.name,
                self.program.display()
            )));
        }
        validate_unique_values(
            &self.name,
            "operations",
            self.operations.iter().map(|value| value.as_str()),
        )?;
        validate_unique_values(
            &self.name,
            "artifact_access",
            self.artifact_access.iter().map(|value| value.as_str()),
        )?;
        validate_bound(
            &format!("worker {:?} max_parallel", self.name),
            u64::from(self.max_parallel),
            1,
            256,
        )?;
        if self.accelerator.is_some() && !self.operations.contains(&OperationKind::TranscodeVideo) {
            return Err(config_error(format!(
                "worker {:?} has an accelerator descriptor but does not declare transcode_video",
                self.name
            )));
        }
        if let Some(accelerator) = self.accelerator.as_ref() {
            accelerator
                .validate_declaration()
                .map_err(|message| config_error(format!("worker {:?} {message}", self.name)))?;
        }
        self.validate_dependencies()?;
        Ok(())
    }

    fn validate_dependencies(&self) -> Result<(), VoomError> {
        let uses_ffmpeg = self.operations.contains(&OperationKind::TranscodeVideo)
            || self.operations.contains(&OperationKind::TranscodeAudio)
            || self.operations.contains(&OperationKind::ExtractAudio);
        let uses_ffprobe = uses_ffmpeg || self.operations.contains(&OperationKind::ProbeFile);
        let uses_nvidia = match self.accelerator.as_ref() {
            Some(VideoAcceleratorDescriptor::Nvidia(_)) => true,
            Some(
                VideoAcceleratorDescriptor::Vaapi(_) | VideoAcceleratorDescriptor::VideoToolbox(_),
            )
            | None => false,
        };
        validate_dependency_path(
            &self.name,
            "ffmpeg_bin",
            self.dependencies.ffmpeg_bin.as_deref(),
            uses_ffmpeg,
            "transcode_video, transcode_audio, or extract_audio",
        )?;
        validate_dependency_path(
            &self.name,
            "ffprobe_bin",
            self.dependencies.ffprobe_bin.as_deref(),
            uses_ffprobe,
            "probe_file or an FFmpeg operation",
        )?;
        validate_dependency_path(
            &self.name,
            "nvidia_smi_bin",
            self.dependencies.nvidia_smi_bin.as_deref(),
            uses_nvidia,
            "an NVIDIA accelerator",
        )
    }
}

fn validate_dependency_path(
    worker_name: &str,
    field: &str,
    path: Option<&Path>,
    required: bool,
    purpose: &str,
) -> Result<(), VoomError> {
    let Some(path) = path else {
        if required {
            return Err(config_error(format!(
                "worker {worker_name:?} dependencies.{field} is required for {purpose} and must \
                 name an absolute executable file"
            )));
        }
        return Ok(());
    };
    if !required {
        return Err(config_error(format!(
            "worker {worker_name:?} dependencies.{field} is only valid for {purpose}"
        )));
    }
    if !path.is_absolute() {
        return Err(config_error(format!(
            "worker {worker_name:?} dependencies.{field} must be an absolute path"
        )));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        config_error(format!(
            "worker {worker_name:?} dependencies.{field} must name an existing regular file: \
             {error}"
        ))
    })?;
    if !metadata.is_file() {
        return Err(config_error(format!(
            "worker {worker_name:?} dependencies.{field} must name a regular file"
        )));
    }
    if !is_executable(&metadata) {
        return Err(config_error(format!(
            "worker {worker_name:?} dependencies.{field} must name an executable regular file"
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn validate_url(raw: &str) -> Result<(), VoomError> {
    let url = reqwest::Url::parse(raw)
        .map_err(|error| config_error(format!("control_plane_url {raw:?} is invalid: {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| config_error("control_plane_url must include a host"))?;
    if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback(host)) {
        return Err(config_error(
            "control_plane_url must use https; http is allowed only for an explicit loopback host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(config_error(
            "control_plane_url must not include user credentials",
        ));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(config_error(
            "control_plane_url must contain only scheme and authority (no path, query, or fragment)",
        ));
    }
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn validate_bound(name: &str, value: u64, minimum: u64, maximum: u64) -> Result<(), VoomError> {
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        Err(config_error(format!(
            "{name} must be between {minimum} and {maximum}; got {value}"
        )))
    }
}

fn validate_unique_values<'a>(
    worker_name: &str,
    field: &str,
    values: impl Iterator<Item = &'a str>,
) -> Result<(), VoomError> {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        return Err(config_error(format!(
            "worker {worker_name:?} {field} must not be empty"
        )));
    }
    let mut unique = HashSet::with_capacity(values.len());
    for value in values {
        if !unique.insert(value) {
            return Err(config_error(format!(
                "worker {worker_name:?} {field} contains duplicate value {value:?}"
            )));
        }
    }
    Ok(())
}

fn load_token(source: &TokenSource) -> Result<SecretString, VoomError> {
    let raw = match source {
        TokenSource::File { path } => std::fs::read_to_string(path).map_err(|error| {
            config_error(format!("read node token {}: {error}", path.display()))
        })?,
        TokenSource::Env { name } => {
            if name.is_empty() {
                return Err(config_error(
                    "node token environment variable name is empty",
                ));
            }
            // Never format the VarError: NotUnicode embeds the variable's raw value, which
            // would put the token itself into stderr and the service journal.
            std::env::var(name).map_err(|error| {
                let cause = match error {
                    std::env::VarError::NotPresent => "not set",
                    std::env::VarError::NotUnicode(_) => "not valid unicode",
                };
                config_error(format!(
                    "read node token environment variable {name}: {cause}"
                ))
            })?
        }
    };
    let token = raw
        .strip_suffix("\r\n")
        .or_else(|| raw.strip_suffix('\n'))
        .unwrap_or(&raw);
    if token.is_empty() || token.contains(['\r', '\n']) {
        return Err(config_error(
            "node token must be non-empty and contain no embedded newlines",
        ));
    }
    Ok(SecretString::from(token.to_owned()))
}

fn config_error(message: impl Into<String>) -> VoomError {
    VoomError::Config(message.into())
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
