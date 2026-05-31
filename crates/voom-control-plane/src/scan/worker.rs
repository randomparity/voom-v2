use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::time::timeout;
use voom_core::{ErrorCode, FailureClass, WorkerId};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProbeFileRequest,
    ProbeFileResult, ProgressFrame, ProtocolError, WorkerCredentials,
};

pub use crate::worker_process::WorkerCommand;
use crate::worker_process::{
    self, BundledWorkerProcess as WorkerProcess, bundled_worker_command_from, fresh_lease_id,
    random_hex_128,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;
const FFPROBE_WORKER_BIN_ENV: &str = "VOOM_FFPROBE_WORKER_BIN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanWorkerError {
    failure_class: FailureClass,
    error_code: ErrorCode,
    message: String,
    shutdown_worker: bool,
}

impl ScanWorkerError {
    #[must_use]
    pub const fn failure_class(&self) -> FailureClass {
        self.failure_class
    }

    #[must_use]
    pub const fn error_code(&self) -> ErrorCode {
        self.error_code
    }

    #[must_use]
    fn should_shutdown_worker(&self) -> bool {
        self.shutdown_worker
    }

    fn new(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
        shutdown_worker: bool,
    ) -> Self {
        Self {
            failure_class,
            error_code,
            message: message.into(),
            shutdown_worker,
        }
    }

    fn worker_crash(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::WorkerCrash,
            ErrorCode::WorkerCrash,
            message,
            true,
        )
    }

    fn malformed_worker_result(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::MalformedWorkerResult,
            ErrorCode::MalformedWorkerResult,
            message,
            true,
        )
    }

    fn progress_timeout(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::ProgressTimeout,
            ErrorCode::WorkerTimeout,
            message,
            true,
        )
    }

    fn terminal_error(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self::new(failure_class, error_code, message, false)
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
    pub async fn launch_bundled_ffprobe(worker_id: WorkerId) -> Result<Self, ScanWorkerError> {
        Self::launch(worker_id, bundled_ffprobe_command()).await
    }

    pub async fn launch(
        worker_id: WorkerId,
        command: WorkerCommand,
    ) -> Result<Self, ScanWorkerError> {
        Ok(Self {
            inner: WorkerProcess::launch(worker_id, command).await?,
        })
    }

    #[must_use]
    pub const fn worker_id(&self) -> WorkerId {
        self.inner.worker_id
    }

    #[must_use]
    pub const fn credentials(&self) -> &WorkerCredentials {
        &self.inner.credentials
    }

    #[must_use]
    pub const fn client(&self) -> &HttpClient {
        &self.inner.client
    }

    pub async fn dispatch_probe_file(
        &mut self,
        request: ProbeFileRequest,
    ) -> Result<ProbeFileResult, ScanWorkerError> {
        let result =
            dispatch_probe_file_with_client(&self.inner.client, &self.inner.credentials, request)
                .await;
        if let Err(err) = &result
            && err.should_shutdown_worker()
        {
            self.terminate().await;
        }
        result
    }

    pub async fn shutdown(self, grace: Duration) -> std::io::Result<ExitStatus> {
        self.inner.shutdown(grace).await
    }

    async fn terminate(&mut self) {
        self.inner.terminate(SHUTDOWN_TIMEOUT).await;
    }
}

pub async fn dispatch_probe_file_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    probe: ProbeFileRequest,
) -> Result<ProbeFileResult, ScanWorkerError>
where
    C: ClientHandle + ?Sized,
{
    let lease_id = fresh_lease_id();
    let payload = serde_json::to_value(probe).map_err(|err| {
        ScanWorkerError::malformed_worker_result(format!("probe_file payload encode: {err}"))
    })?;
    let request = OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload,
        heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
        progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
    };
    let idempotency_key = random_hex_128();
    let dispatch = client
        .dispatch(credentials, &idempotency_key, request)
        .await
        .map_err(|err| map_dispatch_protocol_error(&err))?;
    consume_probe_file_stream(
        dispatch,
        Duration::from_millis(u64::from(DISPATCH_IDLE_DEADLINE_MS)),
    )
    .await
}

async fn consume_probe_file_stream(
    mut dispatch: voom_worker_protocol::DispatchStream,
    idle_timeout: Duration,
) -> Result<ProbeFileResult, ScanWorkerError> {
    loop {
        let outcome = timeout(idle_timeout, dispatch.frames.next_frame())
            .await
            .map_err(|_| {
                ScanWorkerError::progress_timeout(format!(
                    "worker progress idle timeout after {idle_timeout:?}"
                ))
            })?
            .map_err(|err| {
                ScanWorkerError::malformed_worker_result(format!(
                    "worker progress stream protocol error: {err}"
                ))
            })?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {}
            NdjsonOutcome::Frame(_) => {
                return Err(ScanWorkerError::malformed_worker_result(
                    "worker sent terminal frame as non-terminal progress frame",
                ));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<ProbeFileResult>(payload).map_err(|err| {
                    ScanWorkerError::malformed_worker_result(format!(
                        "probe_file result decode: {err}"
                    ))
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }) => {
                return Err(ScanWorkerError::terminal_error(class, code, message));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }) => {
                return Err(ScanWorkerError::malformed_worker_result(
                    "progress frame cannot terminate worker stream",
                ));
            }
            NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed => {
                return Err(ScanWorkerError::worker_crash(
                    "worker stream ended before terminal frame",
                ));
            }
        }
    }
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
        |command, worker_dir| {
            let ffprobe_sibling =
                worker_dir.join(format!("ffprobe{}", std::env::consts::EXE_SUFFIX));
            if ffprobe_sibling.is_file() {
                return command.env("VOOM_FFPROBE_BIN", ffprobe_sibling);
            }
            command
        },
    )
}

fn map_dispatch_protocol_error(err: &ProtocolError) -> ScanWorkerError {
    match err {
        ProtocolError::MalformedFrame { detail }
            if detail.contains("missing response/body separator")
                || detail.contains("response read")
                || detail.starts_with("response decode:") =>
        {
            ScanWorkerError::worker_crash(format!("worker dispatch failed: {err}"))
        }
        ProtocolError::InvalidPayload { detail }
            if detail.starts_with("request:") || detail.starts_with("body:") =>
        {
            ScanWorkerError::worker_crash(format!("worker dispatch failed: {err}"))
        }
        _ => ScanWorkerError::malformed_worker_result(format!("worker dispatch failed: {err}")),
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
