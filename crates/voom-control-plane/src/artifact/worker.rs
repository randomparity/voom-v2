use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use tokio::time::timeout;
use voom_core::{ErrorCode, FailureClass, WorkerId};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, ProgressFrame,
    ProtocolError, VerifyArtifactRequest, VerifyArtifactResult, WorkerCredentials,
};

pub use crate::worker_process::WorkerCommand;
use crate::worker_process::{
    self, BundledWorkerProcess as WorkerProcess, bundled_worker_command_from, fresh_lease_id,
    random_hex_128,
};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;
const VERIFY_ARTIFACT_WORKER_BIN_ENV: &str = "VOOM_VERIFY_ARTIFACT_WORKER_BIN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyWorkerError {
    failure_class: FailureClass,
    error_code: ErrorCode,
    message: String,
    shutdown_worker: bool,
}

impl VerifyWorkerError {
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

    pub(crate) fn terminal_error(
        failure_class: FailureClass,
        error_code: ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self::new(failure_class, error_code, message, false)
    }
}

impl Display for VerifyWorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VerifyWorkerError {}

impl From<worker_process::WorkerProcessError> for VerifyWorkerError {
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
    pub async fn launch_bundled_verify_artifact(
        worker_id: WorkerId,
    ) -> Result<Self, VerifyWorkerError> {
        Self::launch(worker_id, bundled_verify_artifact_command()).await
    }

    pub async fn launch(
        worker_id: WorkerId,
        command: WorkerCommand,
    ) -> Result<Self, VerifyWorkerError> {
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

    pub async fn dispatch_verify_artifact(
        &mut self,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError> {
        let result = dispatch_verify_artifact_with_client(
            &self.inner.client,
            &self.inner.credentials,
            request,
        )
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

pub async fn dispatch_verify_artifact_with_client<C>(
    client: &C,
    credentials: &WorkerCredentials,
    verify: VerifyArtifactRequest,
) -> Result<VerifyArtifactResult, VerifyWorkerError>
where
    C: ClientHandle + ?Sized,
{
    let lease_id = fresh_lease_id();
    let payload = serde_json::to_value(verify).map_err(|err| {
        VerifyWorkerError::malformed_worker_result(format!("verify_artifact payload encode: {err}"))
    })?;
    let request = OperationRequest {
        operation: OperationKind::VerifyArtifact,
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
    consume_verify_artifact_stream(
        dispatch,
        Duration::from_millis(u64::from(DISPATCH_IDLE_DEADLINE_MS)),
    )
    .await
}

async fn consume_verify_artifact_stream(
    mut dispatch: voom_worker_protocol::DispatchStream,
    idle_timeout: Duration,
) -> Result<VerifyArtifactResult, VerifyWorkerError> {
    loop {
        let outcome = timeout(idle_timeout, dispatch.frames.next_frame())
            .await
            .map_err(|_| {
                VerifyWorkerError::progress_timeout(format!(
                    "worker progress idle timeout after {idle_timeout:?}"
                ))
            })?
            .map_err(|err| {
                VerifyWorkerError::malformed_worker_result(format!(
                    "worker progress stream protocol error: {err}"
                ))
            })?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress { .. }) => {}
            NdjsonOutcome::Frame(_) => {
                return Err(VerifyWorkerError::malformed_worker_result(
                    "worker sent terminal frame as non-terminal progress frame",
                ));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<VerifyArtifactResult>(payload).map_err(|err| {
                    VerifyWorkerError::malformed_worker_result(format!(
                        "verify_artifact result decode: {err}"
                    ))
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }) => {
                return Err(VerifyWorkerError::terminal_error(class, code, message));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }) => {
                return Err(VerifyWorkerError::malformed_worker_result(
                    "progress frame cannot terminate worker stream",
                ));
            }
            NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed => {
                return Err(VerifyWorkerError::worker_crash(
                    "worker stream ended before terminal frame",
                ));
            }
        }
    }
}

fn bundled_verify_artifact_command() -> WorkerCommand {
    bundled_verify_artifact_command_from(
        std::env::var_os(VERIFY_ARTIFACT_WORKER_BIN_ENV),
        std::env::current_exe(),
    )
}

fn bundled_verify_artifact_command_from(
    configured_bin: Option<OsString>,
    current_exe: std::io::Result<PathBuf>,
) -> WorkerCommand {
    bundled_worker_command_from(
        configured_bin,
        current_exe,
        "voom-verify-artifact-worker",
        |command, _worker_dir| command,
    )
}

fn map_dispatch_protocol_error(err: &ProtocolError) -> VerifyWorkerError {
    match err {
        ProtocolError::MalformedFrame { detail }
            if detail.contains("missing response/body separator")
                || detail.contains("response read")
                || detail.starts_with("response decode:") =>
        {
            VerifyWorkerError::worker_crash(format!("worker dispatch failed: {err}"))
        }
        ProtocolError::InvalidPayload { detail }
            if detail.starts_with("request:") || detail.starts_with("body:") =>
        {
            VerifyWorkerError::worker_crash(format!("worker dispatch failed: {err}"))
        }
        // A duplicate idempotency key (raced dispatch) and a saturated worker
        // idempotency cache (backpressure) are both transient server-side
        // conditions, not corrupt results — map them to a retriable WorkerCrash
        // rather than the terminal MalformedWorkerResult catch-all.
        ProtocolError::DuplicateIdempotencyKey { .. } | ProtocolError::ServiceAtCapacity => {
            VerifyWorkerError::worker_crash(format!("worker dispatch failed: {err}"))
        }
        _ => VerifyWorkerError::malformed_worker_result(format!("worker dispatch failed: {err}")),
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
