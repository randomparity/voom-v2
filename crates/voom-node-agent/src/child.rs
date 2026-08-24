use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::{Display, Formatter};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use secrecy::ExposeSecret;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinSet;
use voom_worker_protocol::{
    ClientHandle, HttpClient, LocalWorkerBound, VAAPI_PREFLIGHT_BUDGET,
    VIDEOTOOLBOX_PREFLIGHT_BUDGET, VideoAcceleratorDescriptor, WorkerCredentials,
};

use crate::config::{WorkerConfig, WorkerDependencyPaths};

const READINESS_LIMIT_BYTES: usize = 4 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const NVIDIA_STARTUP_TIMEOUT: Duration = Duration::from_mins(5);
const RESTART_LIMIT: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildErrorKind {
    Startup,
    RestartExhausted,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildError {
    kind: ChildErrorKind,
    worker: String,
    message: String,
}

impl ChildError {
    fn startup(worker: &str, message: impl Into<String>) -> Self {
        Self {
            kind: ChildErrorKind::Startup,
            worker: worker.to_owned(),
            message: message.into(),
        }
    }

    fn shutdown(worker: &str, message: impl Into<String>) -> Self {
        Self {
            kind: ChildErrorKind::Shutdown,
            worker: worker.to_owned(),
            message: message.into(),
        }
    }

    fn restart_exhausted(worker: &str, message: impl Into<String>) -> Self {
        Self {
            kind: ChildErrorKind::RestartExhausted,
            worker: worker.to_owned(),
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ChildErrorKind {
        self.kind
    }

    #[must_use]
    pub fn worker(&self) -> &str {
        &self.worker
    }
}

impl Display for ChildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "worker {:?}: {}", self.worker, self.message)
    }
}

impl std::error::Error for ChildError {}

#[derive(Clone)]
pub struct ChildSpec {
    logical_name: String,
    program: PathBuf,
    args: Vec<OsString>,
    credentials: WorkerCredentials,
    dependencies: WorkerDependencyPaths,
    accelerator: Option<VideoAcceleratorDescriptor>,
}

impl std::fmt::Debug for ChildSpec {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChildSpec")
            .field("logical_name", &self.logical_name)
            .field("program", &self.program)
            .field("args", &self.args)
            .field("credentials", &self.credentials)
            .field("accelerator", &self.accelerator)
            .finish_non_exhaustive()
    }
}

impl ChildSpec {
    #[must_use]
    pub fn from_worker(config: &WorkerConfig, credentials: WorkerCredentials) -> Self {
        Self {
            logical_name: config.name.clone(),
            program: config.program.clone(),
            args: config.args.iter().map(OsString::from).collect(),
            dependencies: config.dependencies.clone(),
            accelerator: config.accelerator.clone(),
            credentials,
        }
    }

    #[must_use]
    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    #[must_use]
    pub fn credentials(&self) -> &WorkerCredentials {
        &self.credentials
    }
}

pub struct RunningChild {
    spec: ChildSpec,
    endpoint: SocketAddrV4,
    client: Arc<HttpClient>,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    reaped: bool,
}

impl std::fmt::Debug for RunningChild {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunningChild")
            .field("spec", &self.spec)
            .field("endpoint", &self.endpoint)
            .field("reaped", &self.reaped)
            .finish_non_exhaustive()
    }
}

impl RunningChild {
    #[must_use]
    pub fn spec(&self) -> &ChildSpec {
        &self.spec
    }

    #[must_use]
    pub const fn endpoint(&self) -> SocketAddrV4 {
        self.endpoint
    }

    #[must_use]
    pub fn client(&self) -> Arc<HttpClient> {
        Arc::clone(&self.client)
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    /// Observe a child exit without closing its parent-death pipe.
    ///
    /// # Errors
    ///
    /// Returns [`ChildErrorKind::Shutdown`] when the operating system cannot wait for the child.
    pub async fn wait_for_exit(&mut self) -> Result<ExitStatus, ChildError> {
        let name = self.spec.logical_name.clone();
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| ChildError::shutdown(&name, "child was already reaped"))?;
        let status = child
            .wait()
            .await
            .map_err(|error| ChildError::shutdown(&name, format!("wait for exit: {error}")))?;
        self.reaped = true;
        self.child.take();
        self.stdin.take();
        Ok(status)
    }

    async fn shutdown(&mut self, grace: Duration) -> Result<(), ChildError> {
        drop(self.stdin.take());
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let name = self.spec.logical_name.clone();
        if let Ok(result) = tokio::time::timeout(grace, child.wait()).await {
            result.map_err(|error| {
                ChildError::shutdown(&name, format!("wait after stdin EOF: {error}"))
            })?;
        } else {
            child.start_kill().map_err(|error| {
                ChildError::shutdown(&name, format!("kill after shutdown timeout: {error}"))
            })?;
            child.wait().await.map_err(|error| {
                ChildError::shutdown(&name, format!("reap after kill: {error}"))
            })?;
        }
        self.reaped = true;
        Ok(())
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.reaped {
            return;
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum StartupDeadline {
    BackendSpecific,
    #[cfg(all(test, target_os = "linux"))]
    Fixed(Duration),
}

impl StartupDeadline {
    fn timeout(self, accelerator: Option<&VideoAcceleratorDescriptor>) -> Duration {
        match self {
            #[cfg(all(test, target_os = "linux"))]
            Self::Fixed(timeout) => timeout,
            Self::BackendSpecific => match accelerator {
                Some(VideoAcceleratorDescriptor::Nvidia(_)) => NVIDIA_STARTUP_TIMEOUT,
                Some(VideoAcceleratorDescriptor::Vaapi(_)) => VAAPI_PREFLIGHT_BUDGET,
                Some(VideoAcceleratorDescriptor::VideoToolbox(_)) => VIDEOTOOLBOX_PREFLIGHT_BUDGET,
                None => STARTUP_TIMEOUT,
            },
        }
    }
}

#[derive(Debug)]
pub struct ChildSupervisor {
    startup_deadline: StartupDeadline,
    shutdown_grace: Duration,
    restart_failures: HashMap<String, u8>,
}

impl ChildSupervisor {
    #[must_use]
    pub fn new(shutdown_grace: Duration) -> Self {
        Self {
            startup_deadline: StartupDeadline::BackendSpecific,
            shutdown_grace,
            restart_failures: HashMap::new(),
        }
    }

    // Gated exactly like the module that uses it: `child_test.rs` is Linux-only, so a plain
    // `cfg(test)` leaves this with no callers on other targets and fails the dead-code lint.
    #[cfg(all(test, target_os = "linux"))]
    fn with_timeouts(startup_timeout: Duration, shutdown_grace: Duration) -> Self {
        Self {
            startup_deadline: StartupDeadline::Fixed(startup_timeout),
            shutdown_grace,
            restart_failures: HashMap::new(),
        }
    }

    /// Start and authenticate every configured child concurrently.
    ///
    /// # Errors
    ///
    /// Returns the first startup error after terminating and reaping every started sibling.
    pub async fn start_all(&self, specs: Vec<ChildSpec>) -> Result<Vec<RunningChild>, ChildError> {
        let child_count = specs.len();
        let mut launches = JoinSet::new();
        for (index, spec) in specs.into_iter().enumerate() {
            let timeout = self.startup_deadline.timeout(spec.accelerator.as_ref());
            launches.spawn(async move { (index, launch(spec, timeout).await) });
        }

        let mut children = (0..child_count).map(|_| None).collect::<Vec<_>>();
        let mut first_error = None;
        while let Some(joined) = launches.join_next().await {
            match joined {
                Ok((index, Ok(child))) => children[index] = Some(child),
                Ok((_, Err(error))) => {
                    first_error.get_or_insert(error);
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        ChildError::startup("unknown", format!("startup task failed: {error}"))
                    });
                }
            }
        }

        let started = children.into_iter().flatten().collect::<Vec<_>>();
        if let Some(error) = first_error {
            let _ = self.shutdown_all(started).await;
            return Err(error);
        }
        Ok(started)
    }

    /// Restart one fully specified child while retaining its identity.
    ///
    /// # Errors
    ///
    /// Returns [`ChildErrorKind::RestartExhausted`] on the third consecutive failed startup.
    pub async fn restart(&mut self, spec: &ChildSpec) -> Result<RunningChild, ChildError> {
        let startup_timeout = self.startup_deadline.timeout(spec.accelerator.as_ref());
        match launch(spec.clone(), startup_timeout).await {
            Ok(child) => {
                self.restart_failures.insert(spec.logical_name.clone(), 0);
                Ok(child)
            }
            Err(error) => {
                let failures = self
                    .restart_failures
                    .entry(spec.logical_name.clone())
                    .or_default();
                *failures = failures.saturating_add(1);
                if *failures >= RESTART_LIMIT {
                    Err(ChildError::restart_exhausted(
                        &spec.logical_name,
                        format!("three consecutive startup failures; last error: {error}"),
                    ))
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Close every stdin watchdog concurrently, then kill and reap children that exceed grace.
    ///
    /// # Errors
    ///
    /// Returns the first shutdown error after attempting cleanup for every child.
    pub async fn shutdown_all(&self, children: Vec<RunningChild>) -> Result<(), ChildError> {
        let mut shutdowns = JoinSet::new();
        for mut child in children {
            let grace = self.shutdown_grace;
            shutdowns.spawn(async move { child.shutdown(grace).await });
        }
        let mut first_error = None;
        while let Some(joined) = shutdowns.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    first_error.get_or_insert(error);
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        ChildError::shutdown("unknown", format!("shutdown task failed: {error}"))
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

async fn launch(spec: ChildSpec, startup_timeout: Duration) -> Result<RunningChild, ChildError> {
    if !spec.program.is_absolute() {
        return Err(ChildError::startup(
            &spec.logical_name,
            format!("program must be absolute: {}", spec.program.display()),
        ));
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .env_clear()
        .env("VOOM_WORKER_ID", spec.credentials.worker_id.0.to_string())
        .env(
            "VOOM_WORKER_EPOCH",
            spec.credentials.worker_epoch.to_string(),
        )
        .env(
            "VOOM_WORKER_SECRET",
            spec.credentials.secret.expose_secret(),
        )
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    apply_dependency_environment(&mut command, &spec.dependencies);
    apply_accelerator_environment(&mut command, spec.accelerator.as_ref());
    let mut child = command.spawn().map_err(|error| {
        ChildError::startup(
            &spec.logical_name,
            format!("spawn {}: {error}", spec.program.display()),
        )
    })?;
    let Some(stdin) = child.stdin.take() else {
        kill_and_wait(&mut child).await;
        return Err(ChildError::startup(
            &spec.logical_name,
            "spawned child has no stdin watchdog",
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        kill_and_wait(&mut child).await;
        return Err(ChildError::startup(
            &spec.logical_name,
            "spawned child has no stdout readiness pipe",
        ));
    };
    let endpoint = read_verified_endpoint(&mut child, stdout, startup_timeout, &spec).await?;
    let client = Arc::new(HttpClient::new(SocketAddr::V4(endpoint)));
    if let Err(error) = client.handshake(voom_core::PROTOCOL_VERSION).await {
        kill_and_wait(&mut child).await;
        return Err(ChildError::startup(
            &spec.logical_name,
            format!("protocol handshake failed: {error}"),
        ));
    }
    if let Err(error) = client.identity(&spec.credentials).await {
        kill_and_wait(&mut child).await;
        return Err(ChildError::startup(
            &spec.logical_name,
            format!("worker identity proof failed: {error}"),
        ));
    }
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            return Err(ChildError::startup(
                &spec.logical_name,
                format!("child exited during startup with {status}"),
            ));
        }
        Err(error) => {
            kill_and_wait(&mut child).await;
            return Err(ChildError::startup(
                &spec.logical_name,
                format!("inspect child after identity proof: {error}"),
            ));
        }
    }
    Ok(RunningChild {
        spec,
        endpoint,
        client,
        child: Some(child),
        stdin: Some(stdin),
        reaped: false,
    })
}

async fn read_verified_endpoint(
    child: &mut Child,
    stdout: ChildStdout,
    startup_timeout: Duration,
    spec: &ChildSpec,
) -> Result<SocketAddrV4, ChildError> {
    let readiness = match read_bound(stdout, startup_timeout).await {
        Ok(readiness) => readiness,
        Err(message) => {
            kill_and_wait(child).await;
            return Err(ChildError::startup(&spec.logical_name, message));
        }
    };
    if let Err(message) =
        validate_bound_accelerator(readiness.accelerator.as_ref(), spec.accelerator.as_ref())
    {
        kill_and_wait(child).await;
        return Err(ChildError::startup(&spec.logical_name, message));
    }
    let SocketAddr::V4(endpoint) = readiness.addr else {
        kill_and_wait(child).await;
        return Err(ChildError::startup(
            &spec.logical_name,
            "readiness address must be IPv4 loopback",
        ));
    };
    Ok(endpoint)
}

fn apply_dependency_environment(command: &mut Command, dependencies: &WorkerDependencyPaths) {
    for (name, path) in [
        ("VOOM_FFMPEG_BIN", dependencies.ffmpeg_bin.as_ref()),
        ("VOOM_FFPROBE_BIN", dependencies.ffprobe_bin.as_ref()),
        ("VOOM_NVIDIA_SMI_BIN", dependencies.nvidia_smi_bin.as_ref()),
    ] {
        if let Some(path) = path {
            command.env(name, path.as_os_str());
        }
    }
}

fn apply_accelerator_environment(
    command: &mut Command,
    accelerator: Option<&VideoAcceleratorDescriptor>,
) {
    let Some(accelerator) = accelerator else {
        return;
    };
    match accelerator {
        VideoAcceleratorDescriptor::Nvidia(nvidia) => {
            command
                .env("VOOM_NVIDIA_DEVICE", &nvidia.device_uuid)
                .env("VOOM_NVIDIA_MAX_SESSIONS", nvidia.max_sessions.to_string())
                .env("CUDA_VISIBLE_DEVICES", &nvidia.device_uuid);
        }
        VideoAcceleratorDescriptor::Vaapi(vaapi) => {
            command
                .env("VOOM_VAAPI_DEVICE", &vaapi.pci_address)
                .env("VOOM_VAAPI_MAX_SESSIONS", vaapi.max_sessions.to_string());
        }
        VideoAcceleratorDescriptor::VideoToolbox(videotoolbox) => {
            command
                .env("VOOM_VIDEOTOOLBOX_RESOURCE_ID", &videotoolbox.resource_id)
                .env(
                    "VOOM_VIDEOTOOLBOX_MAX_SESSIONS",
                    videotoolbox.max_sessions.to_string(),
                );
        }
    }
}

async fn read_bound(
    stdout: ChildStdout,
    startup_timeout: Duration,
) -> Result<LocalWorkerBound, String> {
    tokio::time::timeout(startup_timeout, async move {
        let limited = stdout.take((READINESS_LIMIT_BYTES + 1) as u64);
        let mut reader = BufReader::new(limited);
        let mut bytes = Vec::new();
        let count = reader
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|error| format!("read readiness line: {error}"))?;
        if count == 0 {
            return Err("child exited before readiness line".to_owned());
        }
        if bytes.len() > READINESS_LIMIT_BYTES {
            return Err(format!(
                "readiness line exceeds {READINESS_LIMIT_BYTES} bytes"
            ));
        }
        if bytes.last() != Some(&b'\n') {
            return Err("readiness output ended without a newline".to_owned());
        }
        bytes.pop();
        let line = std::str::from_utf8(&bytes)
            .map_err(|error| format!("readiness line is not UTF-8: {error}"))?;
        parse_bound_line(line).map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| format!("timed out after {startup_timeout:?} waiting for readiness"))?
}

/// Parse and validate the one-line worker readiness contract.
///
/// # Errors
///
/// Rejects malformed metadata, unknown fields, invalid accelerator declarations,
/// IPv6, non-loopback, and zero-port addresses.
pub fn parse_bound_line(line: &str) -> Result<LocalWorkerBound, ChildError> {
    let payload = line
        .strip_prefix("BOUND ")
        .ok_or_else(|| ChildError::startup("unknown", "readiness line must start with BOUND "))?;
    let bound = if let Some(address) = payload.strip_prefix("addr=") {
        LocalWorkerBound {
            addr: address.parse::<SocketAddr>().map_err(|error| {
                ChildError::startup("unknown", format!("invalid readiness address: {error}"))
            })?,
            accelerator: None,
        }
    } else {
        serde_json::from_str::<LocalWorkerBound>(payload).map_err(|error| {
            ChildError::startup(
                "unknown",
                format!("invalid structured readiness metadata: {error}"),
            )
        })?
    };
    let SocketAddr::V4(address) = bound.addr else {
        return Err(ChildError::startup(
            "unknown",
            "readiness address must be IPv4 loopback",
        ));
    };
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err(ChildError::startup(
            "unknown",
            "readiness address must be IPv4 loopback with a nonzero port",
        ));
    }
    if let Some(accelerator) = bound.accelerator.as_ref() {
        accelerator.validate_declaration().map_err(|message| {
            ChildError::startup(
                "unknown",
                format!("invalid accelerator readiness metadata: {message}"),
            )
        })?;
    }
    Ok(bound)
}

fn validate_bound_accelerator(
    actual: Option<&VideoAcceleratorDescriptor>,
    declared: Option<&VideoAcceleratorDescriptor>,
) -> Result<(), String> {
    if actual == declared {
        return Ok(());
    }
    Err(format!(
        "accelerator readiness metadata did not match activation declaration: \
         declared {declared:?}, child reported {actual:?}"
    ))
}

async fn kill_and_wait(child: &mut Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(all(test, target_os = "linux"))]
#[path = "child_test.rs"]
mod tests;
