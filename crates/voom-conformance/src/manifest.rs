use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use voom_worker_protocol::OperationKind;

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
    #[error("active binary {name} must declare at least one operation case")]
    MissingOperationCases { name: String },
    #[error("active binary {name} operation {operation:?} {field} must be a JSON object")]
    PayloadNotObject {
        name: String,
        operation: OperationKind,
        field: &'static str,
    },
    #[error("operation coverage missing: {missing:?}")]
    MissingOperationCoverage { missing: Vec<OperationKind> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OperationCase {
    pub operation: OperationKind,
    pub valid_payload: serde_json::Value,
    pub invalid_payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ActiveBinary {
    pub name: String,
    pub target: String,
    pub status: String,
    pub required: bool,
    #[serde(default)]
    pub operations: Vec<OperationCase>,
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
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(default_target_dir, target_dir_from_env);
    resolve_active_with_sources(
        entry,
        |key| std::env::var_os(key),
        Some(target_dir.as_path()),
    )
}

pub fn resolve_active_with<F>(entry: &ActiveBinary, env: F) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    resolve_active_with_sources(entry, env, None)
}

pub fn resolve_active_with_sources<F>(
    entry: &ActiveBinary,
    env: F,
    target_dir: Option<&Path>,
) -> Result<PathBuf, ManifestError>
where
    F: Fn(&str) -> Option<std::ffi::OsString>,
{
    if let Some(path) = &entry.path {
        return Ok(path.clone());
    }
    let env_key = format!("CARGO_BIN_EXE_{}", entry.target);
    if let Some(path) = env(&env_key) {
        return Ok(PathBuf::from(path));
    }
    if let Some(target_dir) = target_dir {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        return Ok(debug_dir(target_dir).join(format!("{}{}", entry.target, suffix)));
    }
    Err(ManifestError::MissingActiveBinary {
        name: entry.name.clone(),
        env_key,
    })
}

fn target_dir_from_env(path: std::ffi::OsString) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        workspace_root().join(path)
    }
}

fn debug_dir(target_dir: &Path) -> PathBuf {
    if let Some(target) = std::env::var_os("CARGO_BUILD_TARGET").filter(|target| !target.is_empty())
    {
        target_dir.join(target).join("debug")
    } else {
        target_dir.join("debug")
    }
}

#[must_use]
pub fn default_target_dir() -> PathBuf {
    workspace_root().join("target")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

pub fn validate_operation_coverage(manifest: &Manifest) -> Result<(), ManifestError> {
    let covered = operation_coverage(manifest);
    let missing = OperationKind::ALL
        .iter()
        .copied()
        .filter(|operation| !covered.contains(operation))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ManifestError::MissingOperationCoverage { missing })
    }
}

fn operation_coverage(manifest: &Manifest) -> HashSet<OperationKind> {
    manifest
        .active
        .iter()
        .flat_map(|entry| entry.operations.iter().map(|case| case.operation))
        .collect()
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
        validate_operations(entry)?;
    }
    Ok(Manifest {
        active: raw.active,
        scaffold: raw.scaffold.binaries,
    })
}

fn validate_operations(entry: &ActiveBinary) -> Result<(), ManifestError> {
    if entry.operations.is_empty() {
        return Err(ManifestError::MissingOperationCases {
            name: entry.name.clone(),
        });
    }
    for case in &entry.operations {
        require_object_payload(entry, case, "valid_payload", &case.valid_payload)?;
        require_object_payload(entry, case, "invalid_payload", &case.invalid_payload)?;
    }
    if matches!(
        entry.name.as_str(),
        "echo-worker" | "chaos-worker" | "benchmark-worker"
    ) && !entry
        .operations
        .iter()
        .any(|case| case.operation == OperationKind::ProbeFile)
    {
        return Err(ManifestError::MissingOperationCoverage {
            missing: vec![OperationKind::ProbeFile],
        });
    }
    Ok(())
}

fn require_object_payload(
    entry: &ActiveBinary,
    case: &OperationCase,
    field: &'static str,
    payload: &serde_json::Value,
) -> Result<(), ManifestError> {
    if payload.is_object() {
        Ok(())
    } else {
        Err(ManifestError::PayloadNotObject {
            name: entry.name.clone(),
            operation: case.operation,
            field,
        })
    }
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
