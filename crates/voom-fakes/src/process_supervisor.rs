use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use secrecy::ExposeSecret;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command as ProcessCommand};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{Id as TaskId, JoinError, JoinHandle, JoinSet};
use voom_worker_protocol::WorkerCredentials;

const COMMAND_CAPACITY: usize = 16;
const READINESS_LIMIT_BYTES: usize = 4 * 1024;
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

type SpawnReply = oneshot::Sender<Result<ReadyChild, ProcessSupervisorError>>;
type WaitReply = oneshot::Sender<Result<ChildExit, ProcessSupervisorError>>;
type ReadinessReader = Pin<Box<dyn AsyncRead + Send>>;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessSupervisorMilestone {
    ChildRegistered(ChildId),
    AwaitingReadiness(ChildId),
    WaitRegistered(ChildId),
}

#[cfg(test)]
#[derive(Clone, Default)]
struct TestMilestones {
    sender: Option<mpsc::UnboundedSender<ProcessSupervisorMilestone>>,
}

#[cfg(not(test))]
#[derive(Clone, Default)]
struct TestMilestones;

impl TestMilestones {
    #[cfg(test)]
    fn send(&self, milestone: ProcessSupervisorMilestone) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(milestone);
        }
    }

    fn child_registered(&self, child_id: ChildId) {
        #[cfg(test)]
        self.send(ProcessSupervisorMilestone::ChildRegistered(child_id));
        #[cfg(not(test))]
        let _ = child_id;
    }

    fn awaiting_readiness(&self, child_id: ChildId) {
        #[cfg(test)]
        self.send(ProcessSupervisorMilestone::AwaitingReadiness(child_id));
        #[cfg(not(test))]
        let _ = child_id;
    }

    fn wait_registered(&self, child_id: ChildId) {
        #[cfg(test)]
        self.send(ProcessSupervisorMilestone::WaitRegistered(child_id));
        #[cfg(not(test))]
        let _ = child_id;
    }
}

pub(crate) struct ProcessSupervisor {
    commands: mpsc::Sender<SupervisorCommand>,
    actor: JoinHandle<Result<Vec<ChildExit>, ProcessSupervisorError>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ChildId(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadyChild {
    pub(crate) child_id: ChildId,
    pub(crate) pid: u32,
    pub(crate) bound: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChildExit {
    pub(crate) child_id: ChildId,
    pub(crate) code: Option<i32>,
    pub(crate) success: bool,
}

trait SupervisedChild {
    fn wait(&mut self, child_id: ChildId) -> impl Future<Output = io::Result<ChildExit>> + Send;

    fn start_kill(&mut self) -> io::Result<()>;
}

impl SupervisedChild for Child {
    async fn wait(&mut self, child_id: ChildId) -> io::Result<ChildExit> {
        Child::wait(self)
            .await
            .map(|status| child_exit(child_id, status))
    }

    fn start_kill(&mut self) -> io::Result<()> {
        Child::start_kill(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessSupervisorError {
    Spawn { binary: PathBuf, detail: String },
    Readiness { child_id: ChildId, detail: String },
    Wait { child_id: ChildId, detail: String },
    Protocol { detail: String },
    Join { detail: String },
}

impl Display for ProcessSupervisorError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { binary, detail } => {
                write!(formatter, "spawn {}: {detail}", binary.display())
            }
            Self::Readiness { child_id, detail } => {
                write!(formatter, "child {} readiness: {detail}", child_id.0)
            }
            Self::Wait { child_id, detail } => {
                write!(formatter, "child {} wait: {detail}", child_id.0)
            }
            Self::Protocol { detail } => write!(formatter, "supervisor protocol: {detail}"),
            Self::Join { detail } => write!(formatter, "supervisor join: {detail}"),
        }
    }
}

impl std::error::Error for ProcessSupervisorError {}

impl ProcessSupervisor {
    pub(crate) fn start() -> Self {
        Self::start_with_milestones(TestMilestones::default())
    }

    #[cfg(test)]
    pub(crate) fn start_with_test_milestones()
    -> (Self, mpsc::UnboundedReceiver<ProcessSupervisorMilestone>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self::start_with_milestones(TestMilestones {
                sender: Some(sender),
            }),
            receiver,
        )
    }

    fn start_with_milestones(milestones: TestMilestones) -> Self {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let actor = tokio::spawn(run_actor(receiver, milestones));
        Self { commands, actor }
    }

    pub(crate) async fn spawn(
        &self,
        binary: PathBuf,
        credentials: WorkerCredentials,
    ) -> Result<ReadyChild, ProcessSupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Spawn {
                binary,
                credentials,
                reply,
            })
            .await
            .map_err(|_| actor_unavailable("send spawn command"))?;
        response
            .await
            .map_err(|_| actor_unavailable("receive spawn response"))?
    }

    pub(crate) async fn wait(
        &self,
        child_id: ChildId,
    ) -> Result<ChildExit, ProcessSupervisorError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(SupervisorCommand::Wait { child_id, reply })
            .await
            .map_err(|_| actor_unavailable("send wait command"))?;
        response
            .await
            .map_err(|_| actor_unavailable("receive wait response"))?
    }

    pub(crate) async fn shutdown(self) -> Result<Vec<ChildExit>, ProcessSupervisorError> {
        let _ = self.commands.send(SupervisorCommand::Shutdown).await;
        drop(self.commands);
        self.actor
            .await
            .map_err(|error| ProcessSupervisorError::Join {
                detail: format!("actor task failed: {error}"),
            })?
    }
}

fn actor_unavailable(operation: &str) -> ProcessSupervisorError {
    ProcessSupervisorError::Protocol {
        detail: format!("cannot {operation}: actor is not running"),
    }
}

enum SupervisorCommand {
    Spawn {
        binary: PathBuf,
        credentials: WorkerCredentials,
        reply: SpawnReply,
    },
    Wait {
        child_id: ChildId,
        reply: WaitReply,
    },
    Shutdown,
}

enum ChildState {
    Running {
        shutdown: Option<oneshot::Sender<()>>,
        waiter: Option<WaitReply>,
    },
    Exited {
        status: ChildExit,
    },
}

struct WatcherCompletion {
    child_id: ChildId,
    ready: bool,
    exit: Result<ChildExit, ProcessSupervisorError>,
    spawn_failure: Option<(SpawnReply, ProcessSupervisorError)>,
}

struct Actor {
    next_child_id: u64,
    registry: HashMap<ChildId, ChildState>,
    watchers: JoinSet<WatcherCompletion>,
    watcher_ids: HashMap<TaskId, ChildId>,
    exits: Vec<ChildExit>,
    first_error: Option<ProcessSupervisorError>,
    milestones: TestMilestones,
}

impl Actor {
    fn new(milestones: TestMilestones) -> Self {
        Self {
            next_child_id: 1,
            registry: HashMap::new(),
            watchers: JoinSet::new(),
            watcher_ids: HashMap::new(),
            exits: Vec::new(),
            first_error: None,
            milestones,
        }
    }

    fn spawn_child(&mut self, binary: PathBuf, credentials: &WorkerCredentials, reply: SpawnReply) {
        let child_id = ChildId(self.next_child_id);
        self.next_child_id = self.next_child_id.saturating_add(1);
        let (mut command, helper_readiness) = child_command(&binary, credentials);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = reply.send(Err(ProcessSupervisorError::Spawn {
                    binary,
                    detail: error.to_string(),
                }));
                return;
            }
        };
        let (shutdown, shutdown_rx) = oneshot::channel();
        let watcher = watch_child(
            child_id,
            child,
            helper_readiness,
            shutdown_rx,
            reply,
            self.milestones.clone(),
        );
        let task = self.watchers.spawn(watcher);
        self.watcher_ids.insert(task.id(), child_id);
        self.registry.insert(
            child_id,
            ChildState::Running {
                shutdown: Some(shutdown),
                waiter: None,
            },
        );
        self.milestones.child_registered(child_id);
    }

    fn register_wait(&mut self, child_id: ChildId, reply: WaitReply) {
        match self.registry.get_mut(&child_id) {
            Some(ChildState::Running { waiter, .. }) if waiter.is_none() => {
                *waiter = Some(reply);
                self.milestones.wait_registered(child_id);
            }
            Some(ChildState::Running { .. }) => {
                send_protocol_error(reply, format!("child {} already has a waiter", child_id.0));
            }
            Some(ChildState::Exited { .. }) => {
                if let Some(ChildState::Exited { status }) = self.registry.remove(&child_id) {
                    self.deliver_or_store(Some(reply), status);
                }
            }
            None => {
                send_protocol_error(
                    reply,
                    format!("child {} is unknown or consumed", child_id.0),
                );
            }
        }
    }

    fn finish_live(&mut self, joined: Result<(TaskId, WatcherCompletion), JoinError>) {
        let Some(completion) = self.resolve_join(joined) else {
            return;
        };
        let state = self.registry.remove(&completion.child_id);
        let Some(ChildState::Running { waiter, .. }) = state else {
            self.record_error(ProcessSupervisorError::Join {
                detail: format!(
                    "child {} completed without a running registry entry",
                    completion.child_id.0
                ),
            });
            return;
        };
        if let Some((reply, error)) = completion.spawn_failure {
            let _ = reply.send(Err(error));
        }
        match completion.exit {
            Ok(status) if completion.ready => self.deliver_or_store(waiter, status),
            Ok(_) => {}
            Err(error) => {
                if let Some(waiter) = waiter {
                    let _ = waiter.send(Err(error.clone()));
                }
                self.record_error(error);
            }
        }
    }

    fn finish_terminal(&mut self, joined: Result<(TaskId, WatcherCompletion), JoinError>) {
        let Some(completion) = self.resolve_join(joined) else {
            return;
        };
        let waiter = match self.registry.remove(&completion.child_id) {
            Some(ChildState::Running { waiter, .. }) => waiter,
            Some(ChildState::Exited { status }) => {
                self.exits.push(status);
                None
            }
            None => None,
        };
        if let Some((reply, error)) = completion.spawn_failure {
            let _ = reply.send(Err(error));
        }
        match completion.exit {
            Ok(status) => {
                if let Some(waiter) = waiter {
                    let _ = waiter.send(Ok(status.clone()));
                }
                self.exits.push(status);
            }
            Err(error) => {
                if let Some(waiter) = waiter {
                    let _ = waiter.send(Err(error.clone()));
                }
                self.record_error(error);
            }
        }
    }

    fn resolve_join(
        &mut self,
        joined: Result<(TaskId, WatcherCompletion), JoinError>,
    ) -> Option<WatcherCompletion> {
        match joined {
            Ok((task_id, completion)) => {
                self.watcher_ids.remove(&task_id);
                Some(completion)
            }
            Err(error) => {
                let child_id = self.watcher_ids.remove(&error.id());
                if let Some(child_id) = child_id {
                    self.registry.remove(&child_id);
                }
                self.record_error(ProcessSupervisorError::Join {
                    detail: format!("child watcher task failed: {error}"),
                });
                None
            }
        }
    }

    fn deliver_or_store(&mut self, waiter: Option<WaitReply>, status: ChildExit) {
        if let Some(waiter) = waiter
            && waiter.send(Ok(status.clone())).is_ok()
        {
            return;
        }
        self.registry
            .insert(status.child_id, ChildState::Exited { status });
    }

    fn record_error(&mut self, error: ProcessSupervisorError) {
        self.first_error.get_or_insert(error);
    }

    async fn shutdown_all(mut self) -> Result<Vec<ChildExit>, ProcessSupervisorError> {
        for state in self.registry.values_mut() {
            if let ChildState::Running { shutdown, .. } = state
                && let Some(shutdown) = shutdown.take()
            {
                let _ = shutdown.send(());
            }
        }
        while let Some(joined) = self.watchers.join_next_with_id().await {
            self.finish_terminal(joined);
        }
        for (_, state) in std::mem::take(&mut self.registry) {
            match state {
                ChildState::Exited { status } => self.exits.push(status),
                ChildState::Running { waiter, .. } => {
                    let error = ProcessSupervisorError::Join {
                        detail: "actor ended with an unjoined child watcher".to_owned(),
                    };
                    if let Some(waiter) = waiter {
                        let _ = waiter.send(Err(error.clone()));
                    }
                    self.record_error(error);
                }
            }
        }
        if let Some(error) = self.first_error {
            return Err(error);
        }
        self.exits.sort_by_key(|status| status.child_id.0);
        Ok(self.exits)
    }
}

fn send_protocol_error(reply: WaitReply, detail: String) {
    let _ = reply.send(Err(ProcessSupervisorError::Protocol { detail }));
}

async fn run_actor(
    mut commands: mpsc::Receiver<SupervisorCommand>,
    milestones: TestMilestones,
) -> Result<Vec<ChildExit>, ProcessSupervisorError> {
    let mut actor = Actor::new(milestones);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(SupervisorCommand::Spawn { binary, credentials, reply }) => {
                    actor.spawn_child(binary, &credentials, reply);
                }
                Some(SupervisorCommand::Wait { child_id, reply }) => {
                    actor.register_wait(child_id, reply);
                }
                Some(SupervisorCommand::Shutdown) | None => break,
            },
            joined = actor.watchers.join_next_with_id(), if !actor.watchers.is_empty() => {
                if let Some(joined) = joined {
                    actor.finish_live(joined);
                    if actor.first_error.is_some() {
                        break;
                    }
                }
            }
        }
    }
    commands.close();
    actor.shutdown_all().await
}

fn child_command(binary: &Path, credentials: &WorkerCredentials) -> (ProcessCommand, bool) {
    let mut command = ProcessCommand::new(binary);
    command
        .env_clear()
        .env("VOOM_WORKER_ID", credentials.worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", credentials.worker_epoch.to_string())
        .env("VOOM_WORKER_SECRET", credentials.secret.expose_secret())
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let helper_readiness = configure_test_helper(&mut command, binary, credentials);
    (command, helper_readiness)
}

#[cfg(test)]
fn configure_test_helper(
    command: &mut ProcessCommand,
    binary: &Path,
    credentials: &WorkerCredentials,
) -> bool {
    if std::env::current_exe().is_ok_and(|current| current == binary) {
        command
            .args([
                "--exact",
                "process_supervisor::tests::process_supervisor_test_helper",
                "--nocapture",
            ])
            .env(
                "VOOM_PROCESS_SUPERVISOR_TEST_HELPER",
                credentials.worker_id.0.to_string(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        true
    } else {
        false
    }
}

#[cfg(not(test))]
fn configure_test_helper(
    _command: &mut ProcessCommand,
    _binary: &Path,
    _credentials: &WorkerCredentials,
) -> bool {
    false
}

async fn watch_child(
    child_id: ChildId,
    mut child: Child,
    helper_readiness: bool,
    mut shutdown: oneshot::Receiver<()>,
    spawn_reply: SpawnReply,
    milestones: TestMilestones,
) -> WatcherCompletion {
    let pid = child.id();
    let mut stdin = child.stdin.take();
    let readiness = take_readiness_reader(&mut child, helper_readiness);
    let ready = await_readiness(child_id, &mut child, readiness, &mut shutdown, &milestones).await;
    match ready {
        Ok(ReadinessOutcome::Ready(bound)) => {
            let ready = ReadyChild {
                child_id,
                pid: pid.unwrap_or_default(),
                bound,
            };
            let _ = spawn_reply.send(Ok(ready));
            let exit = wait_or_shutdown(child_id, &mut child, &mut stdin, &mut shutdown).await;
            WatcherCompletion {
                child_id,
                ready: true,
                exit,
                spawn_failure: None,
            }
        }
        Ok(ReadinessOutcome::Exited(status)) => {
            let error =
                readiness_error(child_id, format!("child exited before readiness: {status}"));
            WatcherCompletion {
                child_id,
                ready: false,
                exit: Ok(child_exit(child_id, status)),
                spawn_failure: Some((spawn_reply, error)),
            }
        }
        Ok(ReadinessOutcome::Shutdown) => {
            finish_failed_readiness(
                child_id,
                child,
                stdin,
                spawn_reply,
                readiness_error(child_id, "supervisor shut down before readiness"),
            )
            .await
        }
        Err(error) => finish_failed_readiness(child_id, child, stdin, spawn_reply, error).await,
    }
}

fn take_readiness_reader(child: &mut Child, helper_readiness: bool) -> Option<ReadinessReader> {
    if helper_readiness {
        child
            .stderr
            .take()
            .map(|stderr| Box::pin(stderr) as ReadinessReader)
    } else {
        child
            .stdout
            .take()
            .map(|stdout| Box::pin(stdout) as ReadinessReader)
    }
}

enum ReadinessOutcome {
    Ready(SocketAddr),
    Exited(ExitStatus),
    Shutdown,
}

async fn await_readiness(
    child_id: ChildId,
    child: &mut Child,
    reader: Option<ReadinessReader>,
    shutdown: &mut oneshot::Receiver<()>,
    milestones: &TestMilestones,
) -> Result<ReadinessOutcome, ProcessSupervisorError> {
    let Some(reader) = reader else {
        return Err(readiness_error(
            child_id,
            "spawned child has no readiness pipe",
        ));
    };
    milestones.awaiting_readiness(child_id);
    let readiness = read_readiness(child_id, reader);
    tokio::pin!(readiness);
    tokio::select! {
        result = &mut readiness => result.map(ReadinessOutcome::Ready),
        status = child.wait() => status
            .map(ReadinessOutcome::Exited)
            .map_err(|error| wait_error(child_id, format!("before readiness: {error}"))),
        _ = shutdown => Ok(ReadinessOutcome::Shutdown),
    }
}

async fn read_readiness(
    child_id: ChildId,
    reader: ReadinessReader,
) -> Result<SocketAddr, ProcessSupervisorError> {
    let mut limited = BufReader::new(reader).take((READINESS_LIMIT_BYTES + 1) as u64);
    let mut frame = Vec::new();
    tokio::time::timeout(CHILD_TIMEOUT, limited.read_until(b'\n', &mut frame))
        .await
        .map_err(|_| readiness_error(child_id, "timed out after five seconds"))?
        .map_err(|error| readiness_error(child_id, format!("read frame: {error}")))?;
    if frame.len() > READINESS_LIMIT_BYTES {
        return Err(readiness_error(
            child_id,
            format!("frame exceeds {READINESS_LIMIT_BYTES} bytes"),
        ));
    }
    if !frame.ends_with(b"\n") {
        return Err(readiness_error(
            child_id,
            "stdout closed before readiness newline",
        ));
    }
    let frame = std::str::from_utf8(&frame)
        .map_err(|error| readiness_error(child_id, format!("frame is not UTF-8: {error}")))?;
    let address = frame
        .strip_prefix("BOUND addr=")
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(|| readiness_error(child_id, "expected `BOUND addr=<loopback>\\n`"))?;
    let bound = address
        .parse::<SocketAddr>()
        .map_err(|error| readiness_error(child_id, format!("invalid bound address: {error}")))?;
    if !bound.ip().is_loopback() {
        return Err(readiness_error(child_id, "bound address is not loopback"));
    }
    Ok(bound)
}

async fn finish_failed_readiness(
    child_id: ChildId,
    mut child: Child,
    mut stdin: Option<ChildStdin>,
    spawn_reply: SpawnReply,
    readiness_failure: ProcessSupervisorError,
) -> WatcherCompletion {
    let exit = shutdown_child(child_id, &mut child, &mut stdin).await;
    let reply_error = match &exit {
        Ok(_) => readiness_failure,
        Err(error) => error.clone(),
    };
    WatcherCompletion {
        child_id,
        ready: false,
        exit,
        spawn_failure: Some((spawn_reply, reply_error)),
    }
}

async fn wait_or_shutdown<C: SupervisedChild>(
    child_id: ChildId,
    child: &mut C,
    stdin: &mut Option<ChildStdin>,
    shutdown: &mut oneshot::Receiver<()>,
) -> Result<ChildExit, ProcessSupervisorError> {
    tokio::select! {
        status = child.wait(child_id) => match status {
            Ok(status) => Ok(status),
            Err(error) => {
                drop(stdin.take());
                kill_and_reap(
                    child_id,
                    child,
                    format!("live wait failed: {error}"),
                )
                .await
            }
        },
        _ = shutdown => shutdown_child(child_id, child, stdin).await,
    }
}

async fn shutdown_child<C: SupervisedChild>(
    child_id: ChildId,
    child: &mut C,
    stdin: &mut Option<ChildStdin>,
) -> Result<ChildExit, ProcessSupervisorError> {
    drop(stdin.take());
    match tokio::time::timeout(CHILD_TIMEOUT, child.wait(child_id)).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => {
            kill_and_reap(
                child_id,
                child,
                format!("wait after stdin close failed: {error}"),
            )
            .await
        }
        Err(_) => {
            kill_and_reap(
                child_id,
                child,
                "wait after stdin close timed out after five seconds".to_owned(),
            )
            .await
        }
    }
}

async fn kill_and_reap<C: SupervisedChild>(
    child_id: ChildId,
    child: &mut C,
    preceding_failure: String,
) -> Result<ChildExit, ProcessSupervisorError> {
    let kill_failure = child.start_kill().err().map(|error| error.to_string());
    loop {
        match child.wait(child_id).await {
            Ok(status) => return Ok(status),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                let kill_detail = kill_failure.as_deref().unwrap_or("none");
                return Err(wait_error(
                    child_id,
                    format!(
                        "ownership became unrecoverable: {preceding_failure}; \
                         kill failure: {kill_detail}; final reap failed: {error}"
                    ),
                ));
            }
        }
    }
}

fn child_exit(child_id: ChildId, status: ExitStatus) -> ChildExit {
    ChildExit {
        child_id,
        code: status.code(),
        success: status.success(),
    }
}

fn readiness_error(child_id: ChildId, detail: impl Into<String>) -> ProcessSupervisorError {
    ProcessSupervisorError::Readiness {
        child_id,
        detail: detail.into(),
    }
}

fn wait_error(child_id: ChildId, detail: impl Into<String>) -> ProcessSupervisorError {
    ProcessSupervisorError::Wait {
        child_id,
        detail: detail.into(),
    }
}

#[cfg(test)]
#[path = "process_supervisor_test.rs"]
mod tests;
