use std::collections::HashMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::NodeId;
use crate::error::VoomError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    /// Parse a log format token from configuration or environment input.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when `s` is not `text` or `json`.
    pub fn parse(s: &str) -> Result<Self, VoomError> {
        match s {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(VoomError::Config(format!(
                "log_format must be 'text' or 'json', got {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub database_url: String,
    pub log_level: String,
    pub log_format: LogFormat,
    pub config_path: PathBuf,
    pub local_node_id: Option<NodeId>,
}

/// Source of environment variables. Production uses `ProcessEnv`; tests inject
/// `MapEnv` so they never touch `std::env`.
pub trait EnvSource {
    fn get(&self, key: &str) -> Option<String>;
}

#[derive(Debug)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[derive(Debug)]
pub struct MapEnv {
    map: HashMap<String, String>,
}

impl MapEnv {
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.map.insert(key.to_owned(), value.to_owned());
        self
    }
}

impl Default for MapEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }
}

impl Config {
    /// Resolve config, reading any missing values from the supplied env source.
    ///
    /// Used by tests with `MapEnv` and by `resolve()` with `ProcessEnv`.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when defaults cannot resolve user
    /// configuration directories or when the effective log format is invalid.
    pub fn resolve_from<E: EnvSource>(
        env: &E,
        database_url_override: Option<String>,
        log_level_override: Option<String>,
        log_format_override: Option<String>,
    ) -> Result<Self, VoomError> {
        let database_url = database_url_override
            .or_else(|| env.get("VOOM_DATABASE_URL"))
            .map_or_else(default_database_url, Ok)?;
        let log_level = log_level_override
            .or_else(|| env.get("VOOM_LOG_LEVEL"))
            .unwrap_or_else(|| "info".to_owned());
        let log_format_str = log_format_override
            .or_else(|| env.get("VOOM_LOG_FORMAT"))
            .unwrap_or_else(|| "json".to_owned());
        let log_format = LogFormat::parse(&log_format_str)?;
        let config_path = default_config_path()?;
        let local_node_id = env
            .get("VOOM_LOCAL_NODE_ID")
            .map(|value| parse_local_node_id(&value))
            .transpose()?;
        Ok(Self {
            database_url,
            log_level,
            log_format,
            config_path,
            local_node_id,
        })
    }

    /// Production entry point — reads from the live process environment.
    ///
    /// # Errors
    ///
    /// Returns [`VoomError::Config`] when defaults cannot resolve user
    /// configuration directories or when the effective log format is invalid.
    pub fn resolve(
        database_url_override: Option<String>,
        log_level_override: Option<String>,
        log_format_override: Option<String>,
    ) -> Result<Self, VoomError> {
        Self::resolve_from(
            &ProcessEnv,
            database_url_override,
            log_level_override,
            log_format_override,
        )
    }
}

fn parse_local_node_id(value: &str) -> Result<NodeId, VoomError> {
    let id = value.parse::<u64>().map_err(|error| {
        VoomError::Config(format!("VOOM_LOCAL_NODE_ID must be a nonzero u64: {error}"))
    })?;
    if id == 0 {
        return Err(VoomError::Config(
            "VOOM_LOCAL_NODE_ID must be a nonzero u64".to_owned(),
        ));
    }
    Ok(NodeId(id))
}

fn project_dirs() -> Result<directories::ProjectDirs, VoomError> {
    directories::ProjectDirs::from("", "", "voom")
        .ok_or_else(|| VoomError::Config("could not resolve user data directory".into()))
}

fn default_database_url() -> Result<String, VoomError> {
    let dirs = project_dirs()?;
    let path = dirs.data_dir().join("voom.db");
    Ok(format!("sqlite://{}", path.display()))
}

fn default_config_path() -> Result<PathBuf, VoomError> {
    let dirs = project_dirs()?;
    Ok(dirs.config_dir().join("config.toml"))
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
