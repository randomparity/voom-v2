use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use voom_core::{ErrorCode, FailureClass, WorkerId};
use voom_worker_protocol::{ClientHandle, FFPROBE_STARTUP_TIMEOUT};

pub use crate::worker_process::WorkerCommand;
use crate::worker_process::{
    self, BundledWorkerProcess as WorkerProcess, bundled_worker_command_from,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const FFPROBE_WORKER_BIN_ENV: &str = "VOOM_FFPROBE_WORKER_BIN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanWorkerError {
    failure_class: FailureClass,
    error_code: ErrorCode,
    message: String,
    shutdown_worker: bool,
    terminal_payload: Option<serde_json::Value>,
}

impl ScanWorkerError {
    /// Test-only accessors; the dispatch pipeline that consumed them moved to
    /// the owner-node agent (ADR 0077) and the remaining production callers
    /// only ask [`Self::should_shutdown_worker`].
    #[cfg(test)]
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[cfg(test)]
    pub(crate) fn is_unprobeable_media(&self) -> bool {
        self.error_code == ErrorCode::MalformedMedia
            || (self.error_code == ErrorCode::ExternalSystemUnavailable
                && self
                    .terminal_payload
                    .as_ref()
                    .and_then(|payload| payload.get("stage"))
                    .and_then(serde_json::Value::as_str)
                    == Some("exit"))
    }

    fn new(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
        shutdown_worker: bool,
        terminal_payload: Option<serde_json::Value>,
    ) -> Self {
        Self {
            failure_class,
            error_code,
            message: message.into(),
            shutdown_worker,
            terminal_payload,
        }
    }

    fn worker_crash(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::WorkerCrash,
            ErrorCode::WorkerCrash,
            message,
            true,
            None,
        )
    }

    #[cfg(test)]
    fn terminal_error(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self::new(failure_class, error_code, message, false, payload)
    }

    #[cfg(test)]
    pub(crate) fn terminal_error_for_test(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self::terminal_error(failure_class, error_code, message, payload)
    }
}

impl Display for ScanWorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ScanWorkerError {}

impl From<worker_process::WorkerProcessError> for ScanWorkerError {
    fn from(err: worker_process::WorkerProcessError) -> Self {
        Self::worker_crash(err.to_string())
    }
}

pub struct BundledWorkerProcess {
    inner: WorkerProcess,
}

impl std::fmt::Debug for BundledWorkerProcess {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.inner, f)
    }
}

impl BundledWorkerProcess {
    #[cfg(test)]
    async fn launch_with_startup_timeout(
        worker_id: WorkerId,
        command: WorkerCommand,
        startup_timeout: Duration,
    ) -> Result<Self, ScanWorkerError> {
        Ok(Self {
            inner: WorkerProcess::launch_with_startup_timeout(worker_id, command, startup_timeout)
                .await?,
        })
    }

    pub async fn shutdown(self, grace: Duration) -> std::io::Result<ExitStatus> {
        self.inner.shutdown(grace).await
    }
}

pub(crate) async fn verify_bundled_ffprobe_readiness() -> Result<(), ScanWorkerError> {
    verify_ffprobe_readiness(bundled_ffprobe_command()).await
}

async fn verify_ffprobe_readiness(command: WorkerCommand) -> Result<(), ScanWorkerError> {
    let process = BundledWorkerProcess {
        inner: WorkerProcess::launch_with_startup_timeout(
            WorkerId(u64::MAX),
            command,
            FFPROBE_STARTUP_TIMEOUT,
        )
        .await?,
    };
    let readiness: Result<(), ScanWorkerError> = async {
        process
            .inner
            .client
            .handshake(voom_core::PROTOCOL_VERSION)
            .await
            .map_err(|error| ScanWorkerError::worker_crash(format!("handshake failed: {error}")))?;
        process
            .inner
            .client
            .identity(&process.inner.credentials)
            .await
            .map_err(|error| ScanWorkerError::worker_crash(format!("identity failed: {error}")))?;
        Ok(())
    }
    .await;
    let shutdown = process
        .shutdown(SHUTDOWN_TIMEOUT)
        .await
        .map_err(|error| ScanWorkerError::worker_crash(format!("shutdown failed: {error}")));
    readiness?;
    shutdown?;
    Ok(())
}

fn bundled_ffprobe_command() -> WorkerCommand {
    bundled_ffprobe_command_from(
        std::env::var_os(FFPROBE_WORKER_BIN_ENV),
        std::env::current_exe(),
    )
}

fn bundled_ffprobe_command_from(
    configured_bin: Option<OsString>,
    current_exe: std::io::Result<PathBuf>,
) -> WorkerCommand {
    bundled_worker_command_from(
        configured_bin,
        current_exe,
        "voom-ffprobe-worker",
        |command, _worker_dir| command,
    )
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
