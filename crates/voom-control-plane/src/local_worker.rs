//! Production control-plane supervisor for a locally-launched mutation worker.
//!
//! [`ControlPlane::start_local_worker`] productizes the test-only
//! `voom-test-support` launch helper: it self-heals any stale same-name worker,
//! registers a node-less worker row, spawns the bundled mutation-worker binary
//! (`voom-ffmpeg-worker` / `voom-mkvtoolnix-worker`) resolved as a sibling of
//! the running executable, reads its bound endpoint from stdout, then records a
//! capability carrying `{endpoint, secret}` and an execute grant so
//! `compliance execute` can discover and dispatch to it. The child's stdin is
//! kept piped; closing it triggers the worker's watchdog shutdown.

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::time::timeout;
use voom_core::{TicketOperation, VoomError, WorkerId, WorkerKind, WorkerStatus};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker};

use crate::ControlPlane;
use crate::worker_process::{WorkerCommand, bundled_worker_command_from, random_hex_128};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const SELF_HEAL_SCAN_LIMIT: u32 = 1000;

/// The kind of bundled mutation worker to launch locally.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LocalWorkerKind {
    Ffmpeg,
    Mkvtoolnix,
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
    /// Self-heals any live same-name worker (a previous hard-kill that left a
    /// stale endpoint), registers a node-less worker row, spawns the bundled
    /// binary, reads its bound endpoint, then records the endpoint+secret
    /// capability and an execute grant. On spawn or bind failure the
    /// just-registered worker row is retired so no dangling worker is left
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
        self.self_heal_stale_workers(kind).await?;

        let secret = random_hex_128();
        let worker = self
            .register_worker(NewWorker {
                name: format!("{}-{}", kind.base_name(), random_hex_128()),
                kind: WorkerKind::Local,
                registered_at: self.clock().now(),
                node_id: None,
            })
            .await?;

        match self.spawn_and_record(kind, worker.id, &secret).await {
            Ok(running) => Ok(running),
            Err(err) => {
                let now = self.clock().now();
                let _ = self.retire_worker(worker.id, worker.epoch, now).await;
                Err(err)
            }
        }
    }

    async fn self_heal_stale_workers(&self, kind: LocalWorkerKind) -> Result<(), VoomError> {
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
            self.retire_worker(worker.id, worker.epoch, now).await?;
        }
        Ok(())
    }

    async fn spawn_and_record(
        &self,
        kind: LocalWorkerKind,
        worker_id: WorkerId,
        secret: &str,
    ) -> Result<RunningLocalWorker, VoomError> {
        let command = bundled_worker_command_from(
            None,
            std::env::current_exe(),
            kind.binary(),
            |command, _worker_dir| command,
        );
        let mut child = spawn_worker(command, worker_id, secret)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| VoomError::WorkerCrash("local worker missing stdin pipe".to_owned()))?;
        let endpoint = match read_bound_endpoint(&mut child).await {
            Ok(endpoint) => endpoint,
            Err(err) => {
                kill_and_wait(&mut child).await;
                return Err(err);
            }
        };

        self.record_local_worker_registry(kind, worker_id, secret, endpoint)
            .await?;

        Ok(RunningLocalWorker {
            child,
            stdin: Some(stdin),
            handle: LocalWorkerHandle {
                worker_id,
                kind,
                endpoint,
            },
        })
    }

    async fn record_local_worker_registry(
        &self,
        kind: LocalWorkerKind,
        worker_id: WorkerId,
        secret: &str,
        endpoint: SocketAddr,
    ) -> Result<(), VoomError> {
        let mut grant_ops = Vec::with_capacity(kind.operations().len());
        let mut max_parallel = serde_json::Map::new();
        for op in kind.operations() {
            let operation = TicketOperation::new(*op)?;
            self.record_capability(NewCapability {
                worker_id,
                operation: operation.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: serde_json::json!({
                    "endpoint": endpoint.to_string(),
                    "secret": secret,
                }),
            })
            .await?;
            max_parallel.insert((*op).to_owned(), serde_json::json!(1));
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
    for (key, value) in command.env {
        spawn.env(key, value);
    }
    spawn
        .spawn()
        .map_err(|err| VoomError::WorkerCrash(format!("spawning local worker: {err}")))
}

async fn read_bound_endpoint(child: &mut Child) -> Result<SocketAddr, VoomError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| VoomError::WorkerCrash("local worker missing stdout pipe".to_owned()))?;
    let mut lines = BufReader::new(stdout).lines();
    let line = match timeout(STARTUP_TIMEOUT, lines.next_line()).await {
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
                "timed out after {STARTUP_TIMEOUT:?} waiting for local worker bound address"
            )));
        }
    };
    let addr = line.strip_prefix("BOUND addr=").ok_or_else(|| {
        VoomError::WorkerCrash(format!("unexpected local worker stdout line: {line}"))
    })?;
    addr.trim().parse::<SocketAddr>().map_err(|err| {
        VoomError::WorkerCrash(format!("local worker printed invalid bound address: {err}"))
    })
}

async fn kill_and_wait(child: &mut Child) {
    let _kill = child.kill().await;
    let _status = child.wait().await;
}

#[cfg(test)]
#[path = "local_worker_test.rs"]
mod tests;
