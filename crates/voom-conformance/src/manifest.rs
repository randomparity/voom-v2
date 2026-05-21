use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest decode: {0}")]
    Decode(String),
    #[error("active binary {name} must have status=active")]
    NonActiveStatus { name: String },
    #[error("active binary {name} must set required=true")]
    NotRequired { name: String },
    #[error("binary {name} listed as both active and scaffold")]
    ActiveAndScaffold { name: String },
    #[error("active binary {name} missing {env_key}")]
    MissingActiveBinary { name: String, env_key: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActiveBinary {
    pub name: String,
    pub target: String,
    pub status: String,
    pub required: bool,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub active: Vec<ActiveBinary>,
    pub scaffold: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(default, rename = "binaries")]
    active: Vec<ActiveBinary>,
    #[serde(default)]
    scaffold: RawScaffold,
}

#[derive(Debug, Default, Deserialize)]
struct RawScaffold {
    #[serde(default)]
    binaries: Vec<String>,
}

impl Manifest {
    pub fn parse_str(raw: &str) -> Result<Self, ManifestError> {
        let decoded: RawManifest =
            toml::from_str(raw).map_err(|e| ManifestError::Decode(e.to_string()))?;
        validate(decoded)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let raw =
            std::fs::read_to_string(path).map_err(|e| ManifestError::Decode(e.to_string()))?;
        Self::parse_str(&raw)
    }
}

pub fn resolve_active(entry: &ActiveBinary) -> Result<PathBuf, ManifestError> {
    resolve_active_with(entry, |key| std::env::var_os(key))
}

pub fn resolve_active_with<F>(entry: &ActiveBinary, env: F) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    if let Some(path) = &entry.path {
        return Ok(path.clone());
    }
    let env_key = format!("CARGO_BIN_EXE_{}", entry.target);
    env(&env_key)
        .map(PathBuf::from)
        .ok_or_else(|| ManifestError::MissingActiveBinary {
            name: entry.name.clone(),
            env_key,
        })
}

fn validate(raw: RawManifest) -> Result<Manifest, ManifestError> {
    let scaffold: HashSet<&str> = raw.scaffold.binaries.iter().map(String::as_str).collect();
    for entry in &raw.active {
        if entry.status != "active" {
            return Err(ManifestError::NonActiveStatus {
                name: entry.name.clone(),
            });
        }
        if !entry.required {
            return Err(ManifestError::NotRequired {
                name: entry.name.clone(),
            });
        }
        if scaffold.contains(entry.name.as_str()) || scaffold.contains(entry.target.as_str()) {
            return Err(ManifestError::ActiveAndScaffold {
                name: entry.name.clone(),
            });
        }
    }
    Ok(Manifest {
        active: raw.active,
        scaffold: raw.scaffold.binaries,
    })
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
