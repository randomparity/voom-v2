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
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;
use voom_core::{ErrorCode, FailureClass, LeaseId, VoomError, WorkerId};
use voom_worker_protocol::{
    DispatchStream, HttpClient, NdjsonOutcome, PercentBps, ProgressFrame, WorkerCredentials,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerProcessError {
    message: String,
}

impl WorkerProcessError {
    fn worker_crash(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for WorkerProcessError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WorkerProcessError {}

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

    #[cfg(test)]
    #[must_use]
    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }

    #[cfg(test)]
    #[must_use]
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_os_string(), value.as_ref().to_os_string()));
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerStreamLabels {
    pub(crate) progress_idle_timeout: &'static str,
    pub(crate) stream_protocol: &'static str,
    pub(crate) terminal_frame_as_progress: &'static str,
    pub(crate) progress_terminal: &'static str,
    pub(crate) stream_ended: &'static str,
    pub(crate) result_decode: &'static str,
}

#[derive(Debug)]
pub(crate) enum WorkerStreamError {
    ProgressIdleTimeout {
        message: String,
    },
    StreamProtocol {
        message: String,
    },
    TerminalFrameAsProgress {
        message: String,
    },
    ProgressFrameAsTerminal {
        message: String,
    },
    StreamEnded {
        message: String,
    },
    ResultDecode {
        message: String,
    },
    Terminal {
        class: FailureClass,
        code: ErrorCode,
        message: String,
    },
    ProgressHandler {
        source: VoomError,
    },
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

pub(crate) struct BundledWorkerProcess {
    pub(crate) worker_id: WorkerId,
    pub(crate) credentials: WorkerCredentials,
    pub(crate) client: HttpClient,
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
    pub(crate) async fn launch(
        worker_id: WorkerId,
        command: WorkerCommand,
    ) -> Result<Self, WorkerProcessError> {
        Self::launch_inner(worker_id, command, STARTUP_TIMEOUT).await
    }

    pub(crate) async fn launch_with_startup_timeout(
        worker_id: WorkerId,
        command: WorkerCommand,
        startup_timeout: Duration,
    ) -> Result<Self, WorkerProcessError> {
        Self::launch_inner(worker_id, command, startup_timeout).await
    }

    async fn launch_inner(
        worker_id: WorkerId,
        command: WorkerCommand,
        startup_timeout: Duration,
    ) -> Result<Self, WorkerProcessError> {
        let credentials = random_credentials(worker_id);
        let mut child = spawn_worker(command, &credentials)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| WorkerProcessError::worker_crash("worker process missing stdin pipe"))?;
        let Some(stdout) = child.stdout.take() else {
            kill_and_wait(&mut child).await;
            return Err(WorkerProcessError::worker_crash(
                "worker process missing stdout pipe",
            ));
        };
        let bound = match read_bound_address(stdout, startup_timeout).await {
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

    pub(crate) async fn shutdown(mut self, grace: Duration) -> std::io::Result<ExitStatus> {
        self.shutdown_inner(grace).await
    }

    pub(crate) async fn terminate(&mut self, grace: Duration) {
        let _status = self.shutdown_inner(grace).await;
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
        let kill_result = child.start_kill();
        let status = child.wait().await?;
        self.reaped = true;
        kill_result?;
        Ok(status)
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

pub(crate) fn random_hex_128() -> String {
    random_hex_bytes(16)
}

pub(crate) fn fresh_lease_id() -> LeaseId {
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

async fn read_bound_address(
    stdout: impl tokio::io::AsyncRead + Unpin,
    startup_timeout: Duration,
) -> Result<SocketAddr, WorkerProcessError> {
    let mut lines = BufReader::new(stdout).lines();
    let line_result = timeout(startup_timeout, lines.next_line()).await;
    let line = match line_result {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            return Err(WorkerProcessError::worker_crash(
                "worker exited before printing bound address",
            ));
        }
        Ok(Err(err)) => {
            return Err(WorkerProcessError::worker_crash(format!(
                "failed reading worker bound address: {err}"
            )));
        }
        Err(_) => {
            return Err(WorkerProcessError::worker_crash(format!(
                "timed out after {startup_timeout:?} waiting for worker bound address"
            )));
        }
    };
    parse_bound_line(&line)
}

pub(crate) async fn consume_worker_stream<Response, P>(
    mut dispatch: DispatchStream,
    idle_timeout: Duration,
    labels: WorkerStreamLabels,
    progress: &mut P,
) -> Result<Response, WorkerStreamError>
where
    Response: DeserializeOwned,
    P: WorkerProgressHandler + ?Sized,
{
    loop {
        let outcome = timeout(idle_timeout, dispatch.frames.next_frame())
            .await
            .map_err(|_| WorkerStreamError::ProgressIdleTimeout {
                message: labels.progress_idle_timeout.to_owned(),
            })?
            .map_err(|err| WorkerStreamError::StreamProtocol {
                message: format!("{}: {err}", labels.stream_protocol),
            })?;
        match outcome {
            NdjsonOutcome::Frame(ProgressFrame::Progress {
                percent, message, ..
            }) => {
                progress
                    .record_progress(percent, message)
                    .await
                    .map_err(|source| WorkerStreamError::ProgressHandler { source })?;
            }
            NdjsonOutcome::Frame(_) => {
                return Err(WorkerStreamError::TerminalFrameAsProgress {
                    message: labels.terminal_frame_as_progress.to_owned(),
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }) => {
                return serde_json::from_value::<Response>(payload).map_err(|err| {
                    WorkerStreamError::ResultDecode {
                        message: format!("{}: {err}", labels.result_decode),
                    }
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                code,
                message,
                ..
            }) => {
                return Err(WorkerStreamError::Terminal {
                    class,
                    code,
                    message,
                });
            }
            NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }) => {
                return Err(WorkerStreamError::ProgressFrameAsTerminal {
                    message: labels.progress_terminal.to_owned(),
                });
            }
            NdjsonOutcome::StreamEnd => {
                return Err(WorkerStreamError::StreamEnded {
                    message: labels.stream_ended.to_owned(),
                });
            }
        }
    }
}

fn worker_search_dirs(exe_dir: &Path) -> Vec<PathBuf> {
    if exe_dir.file_name().is_some_and(|name| name == "deps")
        && let Some(parent) = exe_dir.parent()
    {
        return vec![parent.to_path_buf(), exe_dir.to_path_buf()];
    }
    vec![exe_dir.to_path_buf()]
}

fn spawn_worker(
    worker_command: WorkerCommand,
    credentials: &WorkerCredentials,
) -> Result<Child, WorkerProcessError> {
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
        .map_err(|err| WorkerProcessError::worker_crash(format!("failed spawning worker: {err}")))
}

fn parse_bound_line(line: &str) -> Result<SocketAddr, WorkerProcessError> {
    let Some(addr) = line.strip_prefix("BOUND addr=") else {
        return Err(WorkerProcessError::worker_crash(format!(
            "unexpected worker stdout line: {line}"
        )));
    };
    addr.trim().parse::<SocketAddr>().map_err(|err| {
        WorkerProcessError::worker_crash(format!("worker printed invalid bound address: {err}"))
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

fn random_hex_bytes(len: usize) -> String {
    let mut bytes = vec![0_u8; len];
    let mut rng = StdRng::from_os_rng();
    rng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
