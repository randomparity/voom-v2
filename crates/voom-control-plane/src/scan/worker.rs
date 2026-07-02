use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use voom_core::{ErrorCode, FailureClass, WorkerId};
#[cfg(test)]
use voom_worker_protocol::HttpClient;
use voom_worker_protocol::{
    ClientHandle, OperationKind, ProbeFileRequest, ProbeFileResult, ProtocolError,
    WorkerCredentials,
};

pub use crate::worker_process::WorkerCommand;
use crate::worker_process::{
    self, BundledWorkerProcess as WorkerProcess, NoopWorkerProgressHandler, WorkerDispatchError,
    WorkerOperationDispatch, WorkerStreamError, WorkerStreamLabels, bundled_worker_command_from,
    consume_worker_stream, dispatch_worker_operation_with_client, fresh_lease_id, random_hex_128,
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
    terminal_payload: Option<serde_json::Value>,
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

    fn malformed_worker_result(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::MalformedWorkerResult,
            ErrorCode::MalformedWorkerResult,
            message,
            true,
            None,
        )
    }

    fn progress_timeout(message: impl Into<String>) -> Self {
        Self::new(
            FailureClass::ProgressTimeout,
            ErrorCode::WorkerTimeout,
            message,
            true,
            None,
        )
    }

    fn terminal_error(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self::new(failure_class, error_code, message, false, payload)
    }

    /// A per-file probe fault the directory scan can survive: the file itself is
    /// unprobeable (either a transient probe `exit` failure, or the permanent
    /// `MalformedMedia` — structurally corrupt source, #248/#287), as opposed to
    /// a worker-level fault (crash, protocol error) that should abort the group.
    pub(crate) fn is_unprobeable_media(&self) -> bool {
        if self.error_code == ErrorCode::MalformedMedia {
            return true;
        }
        self.error_code == ErrorCode::ExternalSystemUnavailable
            && self
                .terminal_payload
                .as_ref()
                .and_then(|payload| payload.get("stage"))
                .and_then(serde_json::Value::as_str)
                == Some("exit")
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

    #[must_use]
    pub const fn worker_id(&self) -> WorkerId {
        self.inner.worker_id
    }

    #[cfg(test)]
    #[must_use]
    pub const fn credentials(&self) -> &WorkerCredentials {
        &self.inner.credentials
    }

    #[cfg(test)]
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
    let idempotency_key = random_hex_128();
    let labels = probe_file_stream_labels();
    let dispatch = dispatch_worker_operation_with_client(
        client,
        credentials,
        WorkerOperationDispatch {
            idempotency_key: &idempotency_key,
            operation: OperationKind::ProbeFile,
            lease_id,
            payload: probe,
            heartbeat_deadline_ms: HEARTBEAT_DEADLINE_MS,
            progress_idle_deadline_ms: DISPATCH_IDLE_DEADLINE_MS,
            labels,
        },
    )
    .await
    .map_err(map_probe_dispatch_error)?;
    let mut progress = NoopWorkerProgressHandler;
    consume_worker_stream(
        dispatch,
        Duration::from_millis(u64::from(DISPATCH_IDLE_DEADLINE_MS)),
        labels,
        &mut progress,
    )
    .await
    .map_err(map_probe_stream_error)
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

fn map_dispatch_protocol_error_message(err: &ProtocolError, message: String) -> ScanWorkerError {
    match err {
        ProtocolError::MalformedFrame { detail }
            if detail.contains("missing response/body separator")
                || detail.contains("response read")
                || detail.starts_with("response decode:") =>
        {
            ScanWorkerError::worker_crash(message)
        }
        ProtocolError::InvalidPayload { detail }
            if detail.starts_with("request:") || detail.starts_with("body:") =>
        {
            ScanWorkerError::worker_crash(message)
        }
        // Transient: raced dispatch, worker backpressure, or an unresponsive
        // worker (client-side timeout). All retriable, not corrupt results.
        ProtocolError::DuplicateIdempotencyKey { .. }
        | ProtocolError::ServiceAtCapacity
        | ProtocolError::Timeout { .. } => ScanWorkerError::worker_crash(message),
        _ => ScanWorkerError::malformed_worker_result(message),
    }
}

fn map_probe_dispatch_error(err: WorkerDispatchError) -> ScanWorkerError {
    match err {
        WorkerDispatchError::PayloadEncode { message } => {
            ScanWorkerError::malformed_worker_result(message)
        }
        WorkerDispatchError::DispatchFailed { source, message } => {
            map_dispatch_protocol_error_message(&source, message)
        }
    }
}

fn map_probe_stream_error(err: WorkerStreamError) -> ScanWorkerError {
    match err {
        WorkerStreamError::ProgressIdleTimeout { message } => {
            ScanWorkerError::progress_timeout(message)
        }
        WorkerStreamError::StreamProtocol { message }
        | WorkerStreamError::TerminalFrameAsProgress { message }
        | WorkerStreamError::ProgressFrameAsTerminal { message }
        | WorkerStreamError::ResultDecode { message } => {
            ScanWorkerError::malformed_worker_result(message)
        }
        WorkerStreamError::StreamEnded { message } => ScanWorkerError::worker_crash(message),
        WorkerStreamError::Terminal {
            class,
            code,
            message,
            payload,
        } => ScanWorkerError::terminal_error(class, code, message, payload),
        WorkerStreamError::ProgressHandler { source } => ScanWorkerError::malformed_worker_result(
            format!("probe_file progress handler failed: {source}"),
        ),
    }
}

const fn probe_file_stream_labels() -> WorkerStreamLabels {
    WorkerStreamLabels {
        payload_encode: "probe_file payload encode",
        dispatch_failed: "probe_file dispatch failed",
        progress_idle_timeout: "probe_file worker progress idle timeout",
        stream_protocol: "worker progress stream protocol error",
        terminal_frame_as_progress: "worker sent terminal frame as non-terminal progress frame",
        progress_terminal: "progress frame cannot terminate worker stream",
        stream_ended: "worker stream ended before terminal frame",
        result_decode: "probe_file result decode",
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
