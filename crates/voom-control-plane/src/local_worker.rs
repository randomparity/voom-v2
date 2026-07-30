//! Production control-plane supervisor for a locally-launched mutation worker.
//!
//! [`ControlPlane::start_local_worker`] productizes the test-only
//! `voom-test-support` launch helper: it self-heals unreachable same-kind
//! workers, preserves reachable siblings, registers a node-less worker row,
//! spawns the bundled mutation-worker binary (`voom-ffmpeg-worker` /
//! `voom-mkvtoolnix-worker`) resolved as a sibling of the running executable,
//! reads its bound endpoint from stdout, then records a capability carrying
//! `{endpoint, secret}` and an execute grant so `compliance execute` can
//! discover and dispatch to it. The child's stdin is kept piped; closing it
//! triggers the worker's watchdog shutdown.

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;
use voom_core::{TicketOperation, VoomError, WorkerId, WorkerKind, WorkerStatus};
use voom_store::repo::accelerator_claims::{NewAcceleratorClaim, SqliteAcceleratorClaimRepo};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker};
use voom_worker_protocol::{
    LocalWorkerBound, NvidiaVideoAcceleratorDescriptor, VideoAcceleratorDescriptor,
};

use crate::ControlPlane;
use crate::worker_process::{WorkerCommand, bundled_worker_command_from, random_hex_128};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const NVIDIA_STARTUP_TIMEOUT: Duration = Duration::from_mins(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SELF_HEAL_SCAN_LIMIT: u32 = 1000;

/// The kind of bundled mutation worker to launch locally.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LocalWorkerKind {
    Ffmpeg,
    Mkvtoolnix,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NvidiaLocalWorkerConfig {
    pub device_uuid: String,
    pub max_sessions: u32,
}

impl LocalWorkerKind {
    const fn binary(self) -> &'static str {
        match self {
            Self::Ffmpeg => "voom-ffmpeg-worker",
            Self::Mkvtoolnix => "voom-mkvtoolnix-worker",
        }
    }

    /// Stable label shared by every launch of this kind. The durable worker
    /// row name is this base plus a unique suffix (the `workers.name` column is
    /// globally `UNIQUE`, so a retired row would otherwise block re-registering
    /// the same name); self-heal matches prior workers by this base.
    const fn base_name(self) -> &'static str {
        match self {
            Self::Ffmpeg => "local-ffmpeg",
            Self::Mkvtoolnix => "local-mkvtoolnix",
        }
    }

    const fn operations(self) -> &'static [&'static str] {
        match self {
            Self::Ffmpeg => &["transcode_video", "transcode_audio", "extract_audio"],
            Self::Mkvtoolnix => &["remux"],
        }
    }
}

/// Identifying facts about a launched local worker.
#[derive(Clone, Debug)]
pub struct LocalWorkerHandle {
    pub worker_id: WorkerId,
    pub kind: LocalWorkerKind,
    pub endpoint: SocketAddr,
}

/// A live local worker: the running child process plus the control-plane row it
/// is registered against. Dropping it kills the child (via `kill_on_drop`) but
/// leaves the durable worker row; call [`Self::shutdown_and_retire`] for a clean
/// teardown that also retires the row.
pub struct RunningLocalWorker {
    child: Child,
    stdin: Option<ChildStdin>,
    handle: LocalWorkerHandle,
}

impl std::fmt::Debug for RunningLocalWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningLocalWorker")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

impl RunningLocalWorker {
    #[must_use]
    pub fn handle(&self) -> &LocalWorkerHandle {
        &self.handle
    }

    /// Close the child's stdin (triggering the worker watchdog), await the
    /// child's exit, then retire the worker row.
    ///
    /// # Errors
    /// Returns `VoomError` if waiting on the child fails or if retiring the
    /// worker row fails.
    pub async fn shutdown_and_retire(mut self, cp: &ControlPlane) -> Result<(), VoomError> {
        drop(self.stdin.take());
        if timeout(SHUTDOWN_TIMEOUT, self.child.wait()).await.is_err() {
            self.child
                .kill()
                .await
                .map_err(|err| VoomError::WorkerCrash(format!("killing local worker: {err}")))?;
            self.child.wait().await.map_err(|err| {
                VoomError::WorkerCrash(format!("awaiting killed local worker: {err}"))
            })?;
        }
        let now = cp.clock().now();
        let epoch = current_epoch(cp, self.handle.worker_id).await?;
        cp.retire_worker(self.handle.worker_id, epoch, now).await?;
        Ok(())
    }
}

impl ControlPlane {
    /// Launch a bundled mutation worker locally and register it for discovery.
    ///
    /// Self-heals unreachable same-kind workers left by a previous hard kill,
    /// preserves reachable siblings, registers a node-less worker row, spawns
    /// the bundled binary, reads its bound endpoint, then records the
    /// endpoint+secret capability and an execute grant. On spawn or bind failure
    /// the just-registered worker row is retired so no dangling worker is left
    /// behind.
    ///
    /// # Errors
    /// Returns `VoomError` if the bundled binary cannot be spawned, exits before
    /// printing its bound address (e.g. an ffmpeg/mkvtoolnix preflight failure),
    /// or any registry write fails.
    pub async fn start_local_worker(
        &self,
        kind: LocalWorkerKind,
    ) -> Result<RunningLocalWorker, VoomError> {
        self.start_local_worker_configured(kind, None).await
    }

    /// Launch a local worker with an optional exact NVIDIA device binding.
    ///
    /// # Errors
    /// Returns `CONFIG_INVALID` for an invalid kind/configuration pairing or
    /// session declaration, and otherwise propagates startup and registry
    /// failures.
    pub async fn start_local_worker_configured(
        &self,
        kind: LocalWorkerKind,
        nvidia: Option<NvidiaLocalWorkerConfig>,
    ) -> Result<RunningLocalWorker, VoomError> {
        validate_local_worker_config(kind, nvidia.as_ref())?;
        if let Some(nvidia) = &nvidia {
            self.recover_nvidia_claim(nvidia).await?;
        } else {
            self.self_heal_stale_workers(kind).await?;
        }

        let secret = random_hex_128();
        let (worker, child) = self
            .register_spawn_and_claim(kind, &secret, nvidia.as_ref())
            .await?;

        match self
            .finish_startup(kind, worker.id, &secret, child, nvidia.as_ref())
            .await
        {
            Ok(running) => Ok(running),
            Err(error) => Err(self
                .retire_failed_worker(&worker, nvidia.as_ref(), error)
                .await),
        }
    }

    async fn retire_failed_worker(
        &self,
        worker: &voom_store::repo::workers::Worker,
        nvidia: Option<&NvidiaLocalWorkerConfig>,
        source: VoomError,
    ) -> VoomError {
        if let Some(config) = nvidia {
            let token = format!("nvidia:{}", config.device_uuid);
            let claims = SqliteAcceleratorClaimRepo::new(self.pool.clone());
            let claim = match claims.get(&token).await {
                Ok(claim) => claim,
                Err(error) => {
                    return cleanup_failure_with_retained_claim(&token, &source, &error);
                }
            };
            if let Some(claim) = claim.filter(|claim| claim.worker_id == worker.id) {
                match process_group_has_members(claim.process_group_id) {
                    Ok(false) => {}
                    Ok(true) => {
                        let error = VoomError::WorkerCrash(format!(
                            "process group {} still has live members",
                            claim.process_group_id
                        ));
                        return cleanup_failure_with_retained_claim(&token, &source, &error);
                    }
                    Err(error) => {
                        return cleanup_failure_with_retained_claim(&token, &source, &error);
                    }
                }
            }
        }
        if let Err(error) = self
            .retire_worker(worker.id, worker.epoch, self.clock().now())
            .await
        {
            return VoomError::WorkerCrash(format!(
                "{source}; retiring failed local worker {} also failed: {error}",
                worker.id.0
            ));
        }
        source
    }

    async fn recover_nvidia_claim(
        &self,
        config: &NvidiaLocalWorkerConfig,
    ) -> Result<(), VoomError> {
        let token = format!("nvidia:{}", config.device_uuid);
        let claims = SqliteAcceleratorClaimRepo::new(self.pool.clone());
        let Some(claim) = claims.get(&token).await? else {
            return Ok(());
        };
        let boot_id = linux_boot_id()?;
        if claim.boot_id != boot_id {
            return self.retire_claim_owner(&claim).await;
        }
        match linux_process_start_ticks_if_present(claim.supervisor_pid)? {
            Some(start_ticks) if start_ticks == claim.supervisor_start_ticks => {
                return Err(VoomError::Conflict(format!(
                    "accelerator `{token}` is owned by live supervisor PID {}",
                    claim.supervisor_pid
                )));
            }
            Some(_) => {
                return Err(VoomError::Conflict(format!(
                    "accelerator `{token}` owner PID {} was reused; refusing to signal or steal",
                    claim.supervisor_pid
                )));
            }
            None => {}
        }
        if linux_process_start_ticks_if_present(claim.process_group_id)?.is_some() {
            return Err(VoomError::Conflict(format!(
                "accelerator `{token}` process-group leader {} was reused; refusing to signal",
                claim.process_group_id
            )));
        }
        terminate_process_group(claim.process_group_id).await?;
        self.retire_claim_owner(&claim).await
    }

    async fn retire_claim_owner(
        &self,
        claim: &voom_store::repo::accelerator_claims::AcceleratorClaim,
    ) -> Result<(), VoomError> {
        let inspection = self
            .get_worker_inspection(claim.worker_id)
            .await?
            .ok_or_else(|| {
                VoomError::Internal(format!(
                    "accelerator claim owner worker {} is missing",
                    claim.worker_id.0
                ))
            })?;
        self.retire_worker(claim.worker_id, inspection.worker.epoch, self.clock().now())
            .await?;
        Ok(())
    }

    async fn self_heal_stale_workers(&self, kind: LocalWorkerKind) -> Result<(), VoomError> {
        let live_runtimes = self.live_policy_runtime_registry().await?;
        let inspections = self
            .list_worker_inspections(None, SELF_HEAL_SCAN_LIMIT)
            .await?;
        let now = self.clock().now();
        let prefix = format!("{}-", kind.base_name());
        for inspection in inspections {
            let worker = inspection.worker;
            if worker.name != kind.base_name() && !worker.name.starts_with(&prefix) {
                continue;
            }
            if !matches!(
                worker.status,
                WorkerStatus::Registered | WorkerStatus::Active
            ) {
                continue;
            }
            if live_runtimes.contains(worker.id) {
                continue;
            }
            self.retire_worker(worker.id, worker.epoch, now).await?;
        }
        Ok(())
    }

    async fn register_spawn_and_claim(
        &self,
        kind: LocalWorkerKind,
        secret: &str,
        nvidia: Option<&NvidiaLocalWorkerConfig>,
    ) -> Result<(voom_store::repo::workers::Worker, Child), VoomError> {
        let mut tx = self.pool.begin().await.map_err(|error| {
            VoomError::database_context("begin local worker registration", error)
        })?;
        let worker = self
            .register_supervisor_worker_in_tx(
                &mut tx,
                NewWorker {
                    name: format!("{}-{}", kind.base_name(), random_hex_128()),
                    kind: WorkerKind::Local,
                    registered_at: self.clock().now(),
                    node_id: None,
                },
            )
            .await?;
        let command = bundled_worker_command_from(
            None,
            std::env::current_exe(),
            kind.binary(),
            |command, _worker_dir| command,
        );
        let mut child = spawn_worker(command, worker.id, secret, nvidia)?;
        if let Some(nvidia) = nvidia {
            let claim = (|| {
                let process_id = child.id().ok_or_else(|| {
                    VoomError::WorkerCrash("NVIDIA worker process has no process id".to_owned())
                })?;
                Ok::<_, VoomError>(NewAcceleratorClaim {
                    hardware_token: format!("nvidia:{}", nvidia.device_uuid),
                    worker_id: worker.id,
                    boot_id: linux_boot_id()?,
                    supervisor_pid: process_id,
                    supervisor_start_ticks: linux_process_start_ticks(process_id)?,
                    process_group_id: process_id,
                    capacity: nvidia.max_sessions,
                    claimed_at: self.clock().now(),
                })
            })();
            let claim = match claim {
                Ok(claim) => claim,
                Err(error) => {
                    return Err(kill_and_wait_on_error(&mut child, error).await);
                }
            };
            let claim_result = SqliteAcceleratorClaimRepo::new(self.pool.clone())
                .claim_in_tx(&mut tx, claim)
                .await;
            if let Err(error) = claim_result {
                return Err(kill_and_wait_on_error(&mut child, error).await);
            }
        }
        if let Err(error) = tx.commit().await {
            let error = VoomError::database_context("commit local worker registration", error);
            return Err(kill_and_wait_on_error(&mut child, error).await);
        }
        Ok((worker, child))
    }

    async fn finish_startup(
        &self,
        kind: LocalWorkerKind,
        worker_id: WorkerId,
        secret: &str,
        mut child: Child,
        nvidia: Option<&NvidiaLocalWorkerConfig>,
    ) -> Result<RunningLocalWorker, VoomError> {
        let startup = async {
            let stdin = child.stdin.take().ok_or_else(|| {
                VoomError::WorkerCrash("local worker missing stdin pipe".to_owned())
            })?;
            let startup_timeout = if nvidia.is_some() {
                NVIDIA_STARTUP_TIMEOUT
            } else {
                STARTUP_TIMEOUT
            };
            let bound = read_bound(&mut child, startup_timeout).await?;
            let accelerator = bound_nvidia_accelerator(bound.accelerator.as_ref())?;
            validate_bound_accelerator(accelerator, nvidia)?;
            self.record_local_worker_registry(kind, worker_id, secret, bound.addr, accelerator)
                .await?;
            Ok::<_, VoomError>((stdin, bound))
        }
        .await;
        let (stdin, bound) = match startup {
            Ok(startup) => startup,
            Err(error) => {
                return Err(kill_and_wait_on_error(&mut child, error).await);
            }
        };

        Ok(RunningLocalWorker {
            child,
            stdin: Some(stdin),
            handle: LocalWorkerHandle {
                worker_id,
                kind,
                endpoint: bound.addr,
            },
        })
    }

    async fn record_local_worker_registry(
        &self,
        kind: LocalWorkerKind,
        worker_id: WorkerId,
        secret: &str,
        endpoint: SocketAddr,
        accelerator: Option<&NvidiaVideoAcceleratorDescriptor>,
    ) -> Result<(), VoomError> {
        let mut grant_ops = Vec::with_capacity(kind.operations().len());
        let mut max_parallel = serde_json::Map::new();
        for op in kind.operations() {
            let operation = TicketOperation::new(*op)?;
            let mut extra = serde_json::json!({
                "endpoint": endpoint.to_string(),
                "secret": secret,
            });
            if let Some(accelerator) = accelerator {
                extra["accelerator"] = serde_json::to_value(accelerator).map_err(|error| {
                    VoomError::Internal(format!("serialize local accelerator descriptor: {error}"))
                })?;
            }
            self.record_capability(NewCapability {
                worker_id,
                operation: operation.clone(),
                codecs: Vec::new(),
                hardware: accelerator
                    .map(|descriptor| vec![descriptor.hardware_token.clone()])
                    .unwrap_or_default(),
                artifact_access: Vec::new(),
                extra,
            })
            .await?;
            let operation_capacity = if *op == "transcode_video" {
                accelerator.map_or(1, |descriptor| descriptor.max_sessions)
            } else {
                1
            };
            max_parallel.insert((*op).to_owned(), serde_json::json!(operation_capacity));
            grant_ops.push(operation);
        }
        self.record_grant(NewGrant {
            worker_id,
            can_execute: grant_ops,
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: serde_json::Value::Object(max_parallel),
        })
        .await?;
        Ok(())
    }
}

async fn current_epoch(cp: &ControlPlane, worker_id: WorkerId) -> Result<u64, VoomError> {
    let inspection = cp
        .get_worker_inspection(worker_id)
        .await?
        .ok_or_else(|| VoomError::NotFound(format!("local worker {} not found", worker_id.0)))?;
    Ok(inspection.worker.epoch)
}

fn spawn_worker(
    command: WorkerCommand,
    worker_id: WorkerId,
    secret: &str,
    nvidia: Option<&NvidiaLocalWorkerConfig>,
) -> Result<Child, VoomError> {
    let mut spawn = Command::new(command.program);
    spawn
        .args(command.args)
        .env("VOOM_WORKER_SECRET", secret)
        .env("VOOM_WORKER_ID", worker_id.0.to_string())
        .env("VOOM_WORKER_EPOCH", "0")
        .env("VOOM_WORKER_BIND", "127.0.0.1:0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    #[cfg(unix)]
    spawn.process_group(0);
    if let Some(nvidia) = nvidia {
        spawn
            .env("VOOM_NVIDIA_DEVICE", &nvidia.device_uuid)
            .env("VOOM_NVIDIA_MAX_SESSIONS", nvidia.max_sessions.to_string())
            .env("CUDA_VISIBLE_DEVICES", &nvidia.device_uuid);
    }
    for (key, value) in command.env {
        spawn.env(key, value);
    }
    spawn
        .spawn()
        .map_err(|err| VoomError::WorkerCrash(format!("spawning local worker: {err}")))
}

async fn read_bound(child: &mut Child, deadline: Duration) -> Result<LocalWorkerBound, VoomError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VoomError::WorkerCrash("local worker missing stdout pipe".to_owned()))?;
    let mut lines = BufReader::new(stdout).lines();
    let line = match timeout(deadline, lines.next_line()).await {
        Ok(Ok(Some(line))) => line,
        Ok(Ok(None)) => {
            return Err(VoomError::WorkerCrash(
                "local worker exited before printing bound address".to_owned(),
            ));
        }
        Ok(Err(err)) => {
            return Err(VoomError::WorkerCrash(format!(
                "reading local worker bound address: {err}"
            )));
        }
        Err(_) => {
            return Err(VoomError::WorkerTimeout(format!(
                "timed out after {deadline:?} waiting for local worker bound address"
            )));
        }
    };
    let payload = line.strip_prefix("BOUND ").ok_or_else(|| {
        VoomError::WorkerCrash(format!("unexpected local worker stdout line: {line}"))
    })?;
    if let Some(addr) = payload.strip_prefix("addr=") {
        return addr
            .trim()
            .parse::<SocketAddr>()
            .map(|addr| LocalWorkerBound {
                addr,
                accelerator: None,
            })
            .map_err(|err| {
                VoomError::WorkerCrash(format!("local worker printed invalid bound address: {err}"))
            });
    }
    serde_json::from_str(payload).map_err(|error| {
        VoomError::WorkerCrash(format!(
            "local worker printed invalid bound metadata: {error}"
        ))
    })
}

fn validate_local_worker_config(
    kind: LocalWorkerKind,
    nvidia: Option<&NvidiaLocalWorkerConfig>,
) -> Result<(), VoomError> {
    let Some(nvidia) = nvidia else {
        return Ok(());
    };
    if kind != LocalWorkerKind::Ffmpeg {
        return Err(VoomError::Config(
            "NVIDIA device configuration is valid only for an ffmpeg worker".to_owned(),
        ));
    }
    if !(1..=16).contains(&nvidia.max_sessions) {
        return Err(VoomError::Config(
            "NVIDIA max sessions must be in 1..=16".to_owned(),
        ));
    }
    if !is_full_nvidia_uuid(&nvidia.device_uuid) {
        return Err(VoomError::Config(
            "NVIDIA device must be a full GPU- UUID".to_owned(),
        ));
    }
    Ok(())
}

fn is_full_nvidia_uuid(device_uuid: &str) -> bool {
    let Some(uuid) = device_uuid.strip_prefix("GPU-") else {
        return false;
    };
    uuid.len() == 36
        && uuid.char_indices().all(|(index, ch)| match index {
            8 | 13 | 18 | 23 => ch == '-',
            _ => ch.is_ascii_hexdigit(),
        })
}

/// Narrows a worker's advertised accelerator to the NVIDIA descriptor.
///
/// `start_local_worker` only ever configures an NVIDIA device, so a worker that
/// advertised a VAAPI device bound itself to hardware the control plane never
/// asked for. That is a startup failure, not something to record silently.
fn bound_nvidia_accelerator(
    accelerator: Option<&VideoAcceleratorDescriptor>,
) -> Result<Option<&NvidiaVideoAcceleratorDescriptor>, VoomError> {
    match accelerator {
        None => Ok(None),
        Some(VideoAcceleratorDescriptor::Nvidia(nvidia)) => Ok(Some(nvidia)),
        Some(VideoAcceleratorDescriptor::Vaapi(vaapi)) => Err(VoomError::WorkerCrash(format!(
            "local worker advertised an unconfigured VAAPI device at PCI {}",
            vaapi.pci_address
        ))),
    }
}

fn validate_bound_accelerator(
    actual: Option<&NvidiaVideoAcceleratorDescriptor>,
    configured: Option<&NvidiaLocalWorkerConfig>,
) -> Result<(), VoomError> {
    match (actual, configured) {
        (None, None) => Ok(()),
        (Some(actual), Some(configured))
            if actual.device_uuid == configured.device_uuid
                && actual.hardware_token == format!("nvidia:{}", configured.device_uuid)
                && actual.max_sessions == configured.max_sessions =>
        {
            Ok(())
        }
        (Some(actual), Some(configured)) => Err(VoomError::WorkerCrash(format!(
            "NVIDIA readiness metadata did not match configuration: expected {} sessions={} \
             but worker reported {} sessions={}",
            configured.device_uuid,
            configured.max_sessions,
            actual.device_uuid,
            actual.max_sessions
        ))),
        (Some(_), None) => Err(VoomError::WorkerCrash(
            "software worker unexpectedly advertised an accelerator".to_owned(),
        )),
        (None, Some(configured)) => Err(VoomError::WorkerCrash(format!(
            "worker omitted NVIDIA readiness metadata for {}",
            configured.device_uuid
        ))),
    }
}

fn linux_boot_id() -> Result<String, VoomError> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(|error| VoomError::WorkerCrash(format!("reading Linux boot id: {error}")))?;
    let boot_id = boot_id.trim().to_owned();
    if boot_id.is_empty() {
        return Err(VoomError::WorkerCrash("Linux boot id was empty".to_owned()));
    }
    Ok(boot_id)
}

fn linux_process_start_ticks(pid: u32) -> Result<u64, VoomError> {
    linux_process_start_ticks_if_present(pid)?.ok_or_else(|| {
        VoomError::WorkerCrash(format!(
            "process PID {pid} exited before identity was recorded"
        ))
    })
}

fn linux_process_start_ticks_if_present(pid: u32) -> Result<Option<u64>, VoomError> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(VoomError::WorkerCrash(format!(
                "reading process identity for PID {pid}: {error}"
            )));
        }
    };
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(|| {
        VoomError::WorkerCrash(format!("malformed /proc/{pid}/stat process identity"))
    })?;
    let start_ticks = fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| {
            VoomError::WorkerCrash(format!("/proc/{pid}/stat omitted process start time"))
        })?
        .parse::<u64>()
        .map_err(|error| {
            VoomError::WorkerCrash(format!(
                "invalid /proc/{pid}/stat process start time: {error}"
            ))
        })?;
    Ok(Some(start_ticks))
}

async fn terminate_process_group(process_group_id: u32) -> Result<(), VoomError> {
    if !process_group_has_members(process_group_id)? {
        return Ok(());
    }
    signal_process_group(process_group_id, "TERM").await?;
    if wait_for_empty_process_group(process_group_id).await? {
        return Ok(());
    }
    signal_process_group(process_group_id, "KILL").await?;
    if wait_for_empty_process_group(process_group_id).await? {
        return Ok(());
    }
    Err(VoomError::WorkerCrash(format!(
        "process group {process_group_id} remained populated after TERM and KILL"
    )))
}

async fn signal_process_group(process_group_id: u32, signal: &str) -> Result<(), VoomError> {
    let status = Command::new("kill")
        .args([
            format!("-{signal}"),
            "--".to_owned(),
            format!("-{process_group_id}"),
        ])
        .status()
        .await
        .map_err(|error| {
            VoomError::WorkerCrash(format!(
                "starting kill for process group {process_group_id}: {error}"
            ))
        })?;
    if status.success() {
        return Ok(());
    }
    Err(VoomError::WorkerCrash(format!(
        "kill -{signal} for process group {process_group_id} exited {status}"
    )))
}

async fn wait_for_empty_process_group(process_group_id: u32) -> Result<bool, VoomError> {
    for _attempt in 0..50 {
        if !process_group_has_members(process_group_id)? {
            return Ok(true);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(false)
}

fn process_group_has_members(process_group_id: u32) -> Result<bool, VoomError> {
    let entries = std::fs::read_dir("/proc")
        .map_err(|error| VoomError::WorkerCrash(format!("scanning /proc: {error}")))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| VoomError::WorkerCrash(format!("reading /proc entry: {error}")))?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if process_group_for_pid(pid)? == Some(process_group_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn process_group_for_pid(pid: u32) -> Result<Option<u32>, VoomError> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(error) => {
            return Err(VoomError::WorkerCrash(format!(
                "reading process group for PID {pid}: {error}"
            )));
        }
    };
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(|| {
        VoomError::WorkerCrash(format!("malformed /proc/{pid}/stat process group"))
    })?;
    let mut fields = fields.split_whitespace();
    let state = fields
        .next()
        .ok_or_else(|| VoomError::WorkerCrash(format!("/proc/{pid}/stat omitted process state")))?;
    if state == "Z" {
        return Ok(None);
    }
    fields
        .nth(1)
        .ok_or_else(|| VoomError::WorkerCrash(format!("/proc/{pid}/stat omitted process group")))?
        .parse::<u32>()
        .map(Some)
        .map_err(|error| {
            VoomError::WorkerCrash(format!("invalid /proc/{pid}/stat process group: {error}"))
        })
}

async fn kill_and_wait_on_error(child: &mut Child, source: VoomError) -> VoomError {
    match kill_and_wait(child).await {
        Ok(()) => source,
        Err(cleanup) => VoomError::WorkerCrash(format!(
            "{source}; local worker process-group cleanup also failed: {cleanup}"
        )),
    }
}

async fn kill_and_wait(child: &mut Child) -> Result<(), VoomError> {
    let process_group_id = child.id();
    if let Some(process_group_id) = process_group_id {
        let _ = signal_process_group(process_group_id, "TERM").await;
    }
    match timeout(SHUTDOWN_TIMEOUT, child.wait()).await {
        Ok(Ok(_status)) => {}
        Ok(Err(error)) => {
            return Err(VoomError::WorkerCrash(format!(
                "reaping terminated local worker: {error}"
            )));
        }
        Err(_) => {
            if let Some(process_group_id) = process_group_id {
                if signal_process_group(process_group_id, "KILL")
                    .await
                    .is_err()
                {
                    child.kill().await.map_err(|error| {
                        VoomError::WorkerCrash(format!("killing local worker: {error}"))
                    })?;
                }
            } else {
                child.kill().await.map_err(|error| {
                    VoomError::WorkerCrash(format!("killing local worker: {error}"))
                })?;
            }
            child.wait().await.map_err(|error| {
                VoomError::WorkerCrash(format!("reaping killed local worker: {error}"))
            })?;
        }
    }
    let Some(process_group_id) = process_group_id else {
        return Ok(());
    };
    if process_group_has_members(process_group_id)? {
        signal_process_group(process_group_id, "KILL").await?;
        if !wait_for_empty_process_group(process_group_id).await? {
            return Err(VoomError::WorkerCrash(format!(
                "process group {process_group_id} remained populated after worker cleanup"
            )));
        }
    }
    Ok(())
}

fn cleanup_failure_with_retained_claim(
    token: &str,
    source: &VoomError,
    cleanup: &VoomError,
) -> VoomError {
    VoomError::WorkerCrash(format!(
        "{source}; cleanup could not prove the prior process group dead: {cleanup}; \
         accelerator claim `{token}` was retained"
    ))
}

#[cfg(test)]
#[path = "local_worker_test.rs"]
mod tests;
