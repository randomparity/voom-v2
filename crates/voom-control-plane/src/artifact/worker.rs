use std::ffi::{OsStr, OsString};
use std::fmt::{Display, Formatter};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;
use voom_core::{ErrorCode, FailureClass, LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    ClientHandle, HttpClient, NdjsonOutcome, OperationKind, OperationRequest, PercentBps,
    ProgressFrame, ProtocolError, VerifyArtifactRequest, VerifyArtifactResult, WorkerCredentials,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DISPATCH_IDLE_DEADLINE_MS: u32 = 30_000;
const HEARTBEAT_DEADLINE_MS: u32 = 30_000;
const VERIFY_ARTIFACT_WORKER_BIN_ENV: &str = "VOOM_VERIFY_ARTIFACT_WORKER_BIN";

static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug, Clone)]
pub struct WorkerCommand {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) env: Vec<(OsString, OsString)>,
}

impl WorkerCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_os_string(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerStreamLabels {
    pub payload_encode: &'static str,
    pub dispatch_failed: &'static str,
    pub progress_idle_timeout: &'static str,
    pub stream_protocol: &'static str,
    pub terminal_frame_as_progress: &'static str,
    pub progress_terminal: &'static str,
    pub stream_ended: &'static str,
    pub result_decode: &'static str,
}

pub(crate) struct WorkerOperationDispatch<'a, Request> {
    pub idempotency_key: &'a str,
    pub operation: OperationKind,
    pub lease_id: LeaseId,
    pub payload: Request,
    pub heartbeat_deadline_ms: u32,
    pub progress_idle_deadline_ms: u32,
    pub labels: WorkerStreamLabels,
}

#[async_trait::async_trait]
pub(crate) trait WorkerProgressHandler: Send {
    async fn record_progress(
        &mut self,
        percent: Option<PercentBps>,
        message: Option<String>,
    ) -> Result<(), VoomError>;
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct NoopWorkerProgressHandler;

#[async_trait::async_trait]
impl WorkerProgressHandler for NoopWorkerProgressHandler {
    async fn record_progress(
        &mut self,
        _percent: Option<PercentBps>,
        _message: Option<String>,
    ) -> Result<(), VoomError> {
        Ok(())
    }
}

pub struct BundledWorkerProcess {
    pub worker_id: WorkerId,
    pub credentials: WorkerCredentials,
    pub client: HttpClient,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reaped: bool,
}

impl std::fmt::Debug for BundledWorkerProcess {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BundledWorkerProcess")
            .field("worker_id", &self.worker_id)
            .field("credentials", &self.credentials)
            .field("client", &self.client)
            .field("reaped", &self.reaped)
            .finish_non_exhaustive()
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
        let credentials = random_credentials(worker_id);
        let mut child = spawn_worker(command, &credentials)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| VerifyWorkerError::worker_crash("worker process missing stdin pipe"))?;
        let Some(stdout) = child.stdout.take() else {
            kill_and_wait(&mut child).await;
            return Err(VerifyWorkerError::worker_crash(
                "worker process missing stdout pipe",
            ));
        };
        let mut lines = BufReader::new(stdout).lines();
        let line_result = timeout(STARTUP_TIMEOUT, lines.next_line()).await;
        let line = match line_result {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => {
                kill_and_wait(&mut child).await;
                return Err(VerifyWorkerError::worker_crash(
                    "worker exited before printing bound address",
                ));
            }
            Ok(Err(err)) => {
                kill_and_wait(&mut child).await;
                return Err(VerifyWorkerError::worker_crash(format!(
                    "failed reading worker bound address: {err}"
                )));
            }
            Err(_) => {
                kill_and_wait(&mut child).await;
                return Err(VerifyWorkerError::worker_crash(format!(
                    "timed out after {STARTUP_TIMEOUT:?} waiting for worker bound address"
                )));
            }
        };
        let bound = match parse_bound_line(&line) {
            Ok(bound) => bound,
            Err(err) => {
                kill_and_wait(&mut child).await;
                return Err(err);
            }
        };

        Ok(Self {
            worker_id,
            credentials,
            client: HttpClient::new(bound),
            child: Some(child),
            stdin: Some(stdin),
            reaped: false,
        })
    }

    pub async fn dispatch_verify_artifact(
        &mut self,
        request: VerifyArtifactRequest,
    ) -> Result<VerifyArtifactResult, VerifyWorkerError> {
        let result =
            dispatch_verify_artifact_with_client(&self.client, &self.credentials, request).await;
        if let Err(err) = &result
            && err.should_shutdown_worker()
        {
            self.terminate().await;
        }
        result
    }

    pub async fn shutdown(mut self, grace: Duration) -> std::io::Result<ExitStatus> {
        self.shutdown_inner(grace).await
    }

    async fn shutdown_inner(&mut self, grace: Duration) -> std::io::Result<ExitStatus> {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Err(std::io::Error::other("worker process already reaped"));
        };
        if let Ok(status) = timeout(grace, child.wait()).await {
            self.reaped = true;
            return status;
        }
        child.kill().await?;
        let status = child.wait().await?;
        self.reaped = true;
        Ok(status)
    }

    async fn terminate(&mut self) {
        let _status = self.shutdown_inner(SHUTDOWN_TIMEOUT).await;
    }
}

impl Drop for BundledWorkerProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if !self.reaped
            && let Some(mut child) = self.child.take()
        {
            let _kill = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _status = child.wait().await;
                });
            }
        }
    }
}

pub(crate) async fn dispatch_operation_with_client<C, P, Request, Response>(
    client: &C,
    credentials: &WorkerCredentials,
    operation_dispatch: WorkerOperationDispatch<'_, Request>,
    progress: &mut P,
) -> Result<Response, VoomError>
where
    C: ClientHandle + ?Sized,
    P: WorkerProgressHandler + ?Sized,
    Request: Serialize + Send,
    Response: DeserializeOwned,
{
    let labels = operation_dispatch.labels;
    let payload = serde_json::to_value(operation_dispatch.payload)
        .map_err(|err| VoomError::Internal(format!("{}: {err}", labels.payload_encode)))?;
    let request = OperationRequest {
        operation: operation_dispatch.operation,
        lease_id: operation_dispatch.lease_id,
        payload,
        heartbeat_deadline_ms: operation_dispatch.heartbeat_deadline_ms,
        progress_idle_deadline_ms: operation_dispatch.progress_idle_deadline_ms,
    };
    let dispatch = client
        .dispatch(credentials, operation_dispatch.idempotency_key, request)
        .await
        .map_err(|err| VoomError::WorkerCrash(format!("{}: {err}", labels.dispatch_failed)))?;
    consume_operation_stream(
        dispatch,
        operation_dispatch.progress_idle_deadline_ms,
        labels,
        progress,
    )
    .await
}

async fn consume_operation_stream<Response, P>(
    mut dispatch: voom_worker_protocol::DispatchStream,
    progress_idle_deadline_ms: u32,
    labels: WorkerStreamLabels,
    progress: &mut P,
) -> Result<Response, VoomError>
where
    Response: DeserializeOwned,
    P: WorkerProgressHandler + ?Sized,
{
    loop {
        let outcome = timeout(
            Duration::from_millis(u64::from(progress_idle_deadline_ms)),
            dispatch.frames.next_frame(),
        )
        .await
        .map_err(|_| VoomError::WorkerTimeout(labels.progress_idle_timeout.to_owned()))?
        .map_err(|err| {
            VoomError::MalformedWorkerResult(format!("{}: {err}", labels.stream_protocol))
        })?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress {
                percent, message, ..
            }) => {
                progress.record_progress(percent, message).await?;
            }
            NdjsonOutcome::Frame(_) => {
                return Err(VoomError::MalformedWorkerResult(
                    labels.terminal_frame_as_progress.to_owned(),
                ));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<Response>(payload).map_err(|err| {
                    VoomError::MalformedWorkerResult(format!("{}: {err}", labels.result_decode))
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }) => {
                return Err(worker_terminal_error(class, code, message));
            }
            NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }) => {
                return Err(VoomError::MalformedWorkerResult(
                    labels.progress_terminal.to_owned(),
                ));
            }
            NdjsonOutcome::StreamEnd { .. } | NdjsonOutcome::Closed => {
                return Err(VoomError::WorkerCrash(labels.stream_ended.to_owned()));
            }
        }
    }
}

pub(crate) fn bundled_worker_command_from<F>(
    configured_bin: Option<OsString>,
    current_exe: std::io::Result<PathBuf>,
    worker_binary: &str,
    configure_sibling: F,
) -> WorkerCommand
where
    F: Fn(WorkerCommand, &Path) -> WorkerCommand,
{
    if let Some(configured_bin) = configured_bin {
        return WorkerCommand::new(configured_bin);
    }
    if let Ok(current_exe) = current_exe
        && let Some(exe_dir) = current_exe.parent()
    {
        for worker_dir in worker_search_dirs(exe_dir) {
            let sibling =
                worker_dir.join(format!("{worker_binary}{}", std::env::consts::EXE_SUFFIX));
            if sibling.is_file() {
                return configure_sibling(WorkerCommand::new(sibling), &worker_dir);
            }
        }
    }
    WorkerCommand::new(worker_binary)
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

fn worker_search_dirs(exe_dir: &Path) -> Vec<PathBuf> {
    if exe_dir.file_name().is_some_and(|name| name == "deps")
        && let Some(parent) = exe_dir.parent()
    {
        return vec![parent.to_path_buf(), exe_dir.to_path_buf()];
    }
    vec![exe_dir.to_path_buf()]
}

fn worker_terminal_error(class: FailureClass, code: ErrorCode, message: String) -> VoomError {
    match code {
        ErrorCode::ArtifactUnavailable => VoomError::ArtifactUnavailable(message),
        ErrorCode::ArtifactChecksumMismatch => VoomError::ArtifactChecksumMismatch(message),
        ErrorCode::MalformedWorkerResult => VoomError::MalformedWorkerResult(message),
        ErrorCode::WorkerTimeout => VoomError::WorkerTimeout(message),
        ErrorCode::WorkerCrash => VoomError::WorkerCrash(message),
        _ if class == FailureClass::MalformedWorkerResult => {
            VoomError::MalformedWorkerResult(message)
        }
        _ => VoomError::WorkerCrash(message),
    }
}

fn spawn_worker(
    worker_command: WorkerCommand,
    credentials: &WorkerCredentials,
) -> Result<Child, VerifyWorkerError> {
    let mut command = Command::new(worker_command.program);
    command
        .args(worker_command.args)
        .env("VOOM_WORKER_ID", credentials.worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", credentials.worker_epoch.to_string())
        .env("VOOM_WORKER_SECRET", credentials.secret.expose_secret())
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    for (key, value) in worker_command.env {
        command.env(key, value);
    }
    command
        .spawn()
        .map_err(|err| VerifyWorkerError::worker_crash(format!("failed spawning worker: {err}")))
}

fn parse_bound_line(line: &str) -> Result<SocketAddr, VerifyWorkerError> {
    let Some(addr) = line.strip_prefix("BOUND addr=") else {
        return Err(VerifyWorkerError::worker_crash(format!(
            "unexpected worker stdout line: {line}"
        )));
    };
    addr.trim().parse::<SocketAddr>().map_err(|err| {
        VerifyWorkerError::worker_crash(format!("worker printed invalid bound address: {err}"))
    })
}

async fn kill_and_wait(child: &mut Child) {
    let _kill = child.kill().await;
    let _status = child.wait().await;
}

fn random_credentials(worker_id: WorkerId) -> WorkerCredentials {
    WorkerCredentials {
        worker_id,
        worker_epoch: 0,
        secret: SecretString::from(random_hex_bytes(32)),
    }
}

fn random_hex_128() -> String {
    random_hex_bytes(16)
}

fn random_hex_bytes(len: usize) -> String {
    let mut bytes = vec![0_u8; len];
    let mut rng = StdRng::from_os_rng();
    rng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn fresh_lease_id() -> LeaseId {
    let next = NEXT_LEASE_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(
                current
                    .checked_add(1)
                    .filter(|value| *value != 0)
                    .unwrap_or(1),
            )
        })
        .unwrap_or(1);
    if next == 0 { LeaseId(1) } else { LeaseId(next) }
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
        _ => VerifyWorkerError::malformed_worker_result(format!("worker dispatch failed: {err}")),
    }
}

#[cfg(test)]
#[path = "worker_test.rs"]
mod tests;
