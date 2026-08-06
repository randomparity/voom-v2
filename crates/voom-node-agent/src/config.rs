use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use secrecy::SecretString;
use serde::Deserialize;
use voom_core::{ArtifactAccessMode, NodeId, OperationKind, VoomError};

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
    pub max_parallel: u32,
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

    fn validate(&self) -> Result<(), VoomError> {
        validate_url(&self.control_plane_url)?;
        validate_bound("poll_interval_ms", self.poll_interval_ms, 50, 60_000)?;
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
        )
    }
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
            std::env::var(name).map_err(|error| {
                config_error(format!(
                    "read node token environment variable {name}: {error}"
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
