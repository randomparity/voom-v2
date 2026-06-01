//! Shared startup parsing for worker binaries.

use std::env::VarError;
use std::fmt::Display;
use std::net::{AddrParseError, SocketAddr};
use std::num::ParseIntError;

use secrecy::SecretString;
use thiserror::Error;
use voom_core::WorkerId;

use crate::credentials::WorkerCredentials;
use crate::envelope::ProtocolError;
use crate::transport::{ServerHandle, ServerRunning};

pub const DEFAULT_WORKER_BIND: &str = "127.0.0.1:0";
pub const WORKER_BIND_ENV: &str = "VOOM_WORKER_BIND";
pub const WORKER_EPOCH_ENV: &str = "VOOM_WORKER_EPOCH";
pub const WORKER_ID_ENV: &str = "VOOM_WORKER_ID";
pub const WORKER_SECRET_ENV: &str = "VOOM_WORKER_SECRET";

#[derive(Debug, Error)]
pub enum WorkerStartupError {
    #[error("{name} not set")]
    MissingEnv { name: &'static str },
    #[error("{name} contains non-unicode data")]
    InvalidUnicodeEnv { name: &'static str },
    #[error("{name} parse failed for value {value:?}: {source}")]
    InvalidIntegerEnv {
        name: &'static str,
        value: String,
        #[source]
        source: ParseIntError,
    },
    #[error("{name} parse failed for value {value:?}: {source}")]
    InvalidBindAddress {
        name: &'static str,
        value: String,
        #[source]
        source: AddrParseError,
    },
    #[error("worker bind failed for {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("{operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("worker server failed: {source}")]
    Server {
        #[source]
        source: ProtocolError,
    },
    #[error("{detail}")]
    Dependency { detail: String },
    #[error("unknown worker provider binary {binary_name}")]
    UnknownProvider { binary_name: String },
}

impl WorkerStartupError {
    #[must_use]
    pub fn bind(addr: SocketAddr, source: std::io::Error) -> Self {
        Self::Bind { addr, source }
    }

    #[must_use]
    pub fn dependency(error: impl Display) -> Self {
        Self::Dependency {
            detail: error.to_string(),
        }
    }

    #[must_use]
    pub fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io { operation, source }
    }

    #[must_use]
    pub fn server(source: ProtocolError) -> Self {
        Self::Server { source }
    }

    #[must_use]
    pub fn unknown_provider(binary_name: &str) -> Self {
        Self::UnknownProvider {
            binary_name: binary_name.to_owned(),
        }
    }
}

pub fn load_worker_credentials_from_env() -> Result<WorkerCredentials, WorkerStartupError> {
    let secret = required_env(WORKER_SECRET_ENV)?;
    let worker_id = parse_integer_env(WORKER_ID_ENV, required_env(WORKER_ID_ENV)?)?;
    let worker_epoch = parse_integer_env(WORKER_EPOCH_ENV, required_env(WORKER_EPOCH_ENV)?)?;
    Ok(WorkerCredentials {
        worker_id: WorkerId(worker_id),
        worker_epoch,
        secret: SecretString::from(secret),
    })
}

pub fn load_worker_bind_addr_from_env() -> Result<SocketAddr, WorkerStartupError> {
    let value = optional_env(WORKER_BIND_ENV)?.unwrap_or_else(|| DEFAULT_WORKER_BIND.to_owned());
    parse_bind_addr(WORKER_BIND_ENV, value)
}

pub async fn serve_worker_http(
    server: &impl ServerHandle,
    bind: SocketAddr,
) -> Result<ServerRunning, WorkerStartupError> {
    server.serve(bind).await.map_err(WorkerStartupError::server)
}

fn required_env(name: &'static str) -> Result<String, WorkerStartupError> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(VarError::NotPresent) => Err(WorkerStartupError::MissingEnv { name }),
        Err(VarError::NotUnicode(_)) => Err(WorkerStartupError::InvalidUnicodeEnv { name }),
    }
}

fn optional_env(name: &'static str) -> Result<Option<String>, WorkerStartupError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(WorkerStartupError::InvalidUnicodeEnv { name }),
    }
}

fn parse_integer_env(name: &'static str, value: String) -> Result<u64, WorkerStartupError> {
    value
        .parse::<u64>()
        .map_err(|source| WorkerStartupError::InvalidIntegerEnv {
            name,
            value,
            source,
        })
}

fn parse_bind_addr(name: &'static str, value: String) -> Result<SocketAddr, WorkerStartupError> {
    value
        .parse::<SocketAddr>()
        .map_err(|source| WorkerStartupError::InvalidBindAddress {
            name,
            value,
            source,
        })
}

#[cfg(test)]
#[path = "startup_test.rs"]
mod tests;
