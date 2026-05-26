use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvmergeConfig {
    pub command: PathBuf,
    pub provider_version: String,
    pub timeout: Duration,
}

impl MkvmergeConfig {
    #[must_use]
    pub fn new(command: PathBuf, provider_version: String, timeout: Duration) -> Self {
        Self {
            command,
            provider_version,
            timeout,
        }
    }

    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            command: PathBuf::from("mkvmerge"),
            provider_version: "mkvmerge v80.0 ('Roundabout') 64-bit".to_owned(),
            timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MkvmergeVersion {
    pub major: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum MkvtoolnixError {
    #[error("mkvmerge preflight failed: {0}")]
    Preflight(String),
    #[error("mkvmerge failed: {0}")]
    MkvmergeFailed(String),
    #[error("mkvmerge identify failed: {0}")]
    IdentifyFailed(String),
    #[error("output facts mismatch: {0}")]
    OutputFactsMismatch(String),
    #[error("mkvmerge config invalid: {0}")]
    ConfigInvalid(String),
}

pub const MKVMERGE_BIN_ENV: &str = "VOOM_MKVMERGE_BIN";
const DEFAULT_MKVMERGE_BIN: &str = "mkvmerge";
const MINIMUM_MKVMERGE_MAJOR: u32 = 80;

pub fn preflight_from_process_env() -> Result<MkvmergeConfig, MkvtoolnixError> {
    let command =
        std::env::var_os(MKVMERGE_BIN_ENV).unwrap_or_else(|| OsString::from(DEFAULT_MKVMERGE_BIN));
    let command = resolve_binary(&command);
    preflight_mkvmerge(&command)
}

pub fn preflight_mkvmerge(command: &Path) -> Result<MkvmergeConfig, MkvtoolnixError> {
    require_executable_file(command)?;
    let provider_version = first_output_line(
        "mkvmerge --version",
        Command::new(command).arg("--version").output(),
    )?;
    let _version = parse_mkvmerge_version(&provider_version)?;
    Ok(MkvmergeConfig::new(
        command.to_owned(),
        provider_version,
        crate::mkvmerge::DEFAULT_PROCESS_TIMEOUT,
    ))
}

pub fn parse_mkvmerge_version(output: &str) -> Result<MkvmergeVersion, MkvtoolnixError> {
    let version_token = output
        .split_whitespace()
        .find(|part| part.strip_prefix('v').is_some_and(|rest| !rest.is_empty()))
        .ok_or_else(|| MkvtoolnixError::Preflight("mkvmerge version not found".to_owned()))?;
    let major_text = version_token
        .trim_start_matches('v')
        .split('.')
        .next()
        .unwrap_or_default();
    let major = major_text
        .parse::<u32>()
        .map_err(|err| MkvtoolnixError::Preflight(format!("invalid mkvmerge version: {err}")))?;
    if major < MINIMUM_MKVMERGE_MAJOR {
        return Err(MkvtoolnixError::Preflight(format!(
            "unsupported mkvmerge version v{major}; need v{MINIMUM_MKVMERGE_MAJOR} or newer"
        )));
    }
    Ok(MkvmergeVersion { major })
}

fn resolve_binary(binary: &OsStr) -> PathBuf {
    let path = PathBuf::from(binary);
    if path.components().count() > 1 {
        return path;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(&path);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    path
}

fn require_executable_file(path: &Path) -> Result<(), MkvtoolnixError> {
    if !is_executable_file(path) {
        return Err(MkvtoolnixError::Preflight(format!(
            "mkvmerge binary is missing or not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn first_output_line(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, MkvtoolnixError> {
    command_text(command_name, output)?
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| MkvtoolnixError::Preflight(format!("{command_name} produced no output")))
}

fn command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, MkvtoolnixError> {
    let output = output.map_err(|err| {
        MkvtoolnixError::Preflight(format!("{command_name} failed to start: {err}"))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}{stderr}");
    if output.status.success() {
        Ok(text)
    } else {
        Err(MkvtoolnixError::Preflight(format!(
            "{command_name} exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            text.trim()
        )))
    }
}

#[cfg(test)]
#[path = "preflight_test.rs"]
mod tests;
