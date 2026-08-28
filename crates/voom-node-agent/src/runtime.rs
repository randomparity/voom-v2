use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::rngs::{OsRng, StdRng};
use rand::{Rng, RngCore, SeedableRng, TryRngCore};
use secrecy::SecretString;
use serde_json::{Value as JsonValue, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use voom_core::{
    FailureClass, LeaseId, NodeIncarnationEndReason, NodeIncarnationId, OperationKind, VoomError,
    WorkerId, WorkerReadiness,
};
use voom_worker_protocol::{
    ClientHandle, NdjsonOutcome, OperationRequest, ProgressFrame, WorkerCredentials,
};

use crate::child::{ChildErrorKind, ChildSpec, ChildSupervisor, RunningChild};
use crate::client::{
    AcquireOutcome, AcquireRequest, ActivateOutcome, ActivateRequest, ActivatedWorker,
    CommitApplyingOutcome, CommitApplyingRequest, CommitAuthorizeOutcome, CommitAuthorizeRequest,
    CommitCompleteOutcome, CommitCompleteRequest, CommitOpenOutcome, CommitOpenRequest,
    CommitOutcomeRequest, CommitReceiptOutcome, CompleteOutcome, CompleteRequest,
    ControlPlaneClient, DeactivateOutcome, DeactivateRequest, FailOutcome, FailRequest,
    LeaseDispatch, LeaseHeartbeatOutcome, LeaseHeartbeatRequest, NodeHeartbeatOutcome,
    NodeHeartbeatRequest, RetryRequest, WorkerDeclaration, WorkerReadinessOutcome,
    WorkerReadinessRequest,
};
use crate::commit::{CommitCoordinatorContext, run_commit_coordinator};
use crate::config::{AgentConfig, LoadedAgentConfig, WorkerConfig};
use voom_core::ids::ArtifactCommitIntentId;

const HEARTBEAT_DIVISOR: u32 = 3;
const RESTART_DELAY: Duration = Duration::from_millis(250);
/// Wall-clock budgets for the shutdown tail.
///
/// Threaded as values rather than read from constants at the point of use. Three
/// existing tests gate `deactivate` with a [`tokio::sync::Notify`] that is never
/// notified, so a real 10 s timer read in place would turn each of them into a
/// wall-clock race.
///
/// See `docs/adr/0088-bounded-node-agent-shutdown.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownBudgets {
    /// One control-plane wait in the shutdown tail.
    pub call: Duration,
    /// Collecting a killed child's exit status, after `SIGKILL`.
    pub reap_after_kill: Duration,
    /// Slack the tail backstop adds over the sum of the inner bounds.
    pub backstop_margin: Duration,
}

impl ShutdownBudgets {
    /// Production values.
    ///
    /// 10 s sits under the lifecycle suite's 30 s hang guard, so that guard stops
    /// being the first thing to fire, and under one `REQUEST_TIMEOUT`, so a
    /// shutdown blocked on a wedged control plane abandons rather than spending
    /// another attempt. The 5 s margin covers the tail costs that fall outside
    /// every inner bound — `wait_or_force`'s post-expiry lease drain, the
    /// `JoinSet` spawn before each child's grace starts, and stopping the node
    /// heartbeat — so that every inner bound expires before the backstop does.
    pub const DEFAULT: Self = Self {
        call: Duration::from_secs(10),
        reap_after_kill: Duration::from_secs(1),
        backstop_margin: Duration::from_secs(5),
    };

    /// Upper bound on the whole shutdown tail.
    ///
    /// This is the number `docs/runbooks/operator-node-agent.md` publishes, and
    /// the bound the tail backstop enforces.
    #[must_use]
    pub fn tail(&self, grace: Duration) -> Duration {
        self.call
            .saturating_mul(2)
            .saturating_add(grace)
            .saturating_add(self.reap_after_kill)
            .saturating_add(self.backstop_margin)
    }
}

const VALIDATION_ATTEMPTS: u32 = 3;
const CRASH_LIMIT: u32 = 3;
const CRASH_WINDOW: Duration = Duration::from_mins(1);

/// One live child's dispatch surface, published by its coordinator so the
/// scan-session pump can reach workers it does not own (ADR 0077 plan-review
/// finding b: one coordinator owns exactly one child).
#[derive(Clone, Debug)]
pub(crate) struct ChildEndpoint {
    pub(crate) client: Arc<dyn ClientHandle>,
    pub(crate) credentials: WorkerCredentials,
    pub(crate) operations: Vec<OperationKind>,
}

/// Shared per-incarnation map of logical worker name to that worker's live
/// child endpoint. Coordinators publish on startup and after every restart;
/// consumers observe the live handle or `None` while the child is down.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChildEndpointRegistry {
    entries: Arc<HashMap<String, watch::Sender<Option<ChildEndpoint>>>>,
}

impl ChildEndpointRegistry {
    pub(crate) fn new(workers: &[WorkerConfig]) -> Self {
        let entries = workers
            .iter()
            .map(|worker| (worker.name.clone(), watch::channel(None).0))
            .collect();
        Self {
            entries: Arc::new(entries),
        }
    }

    pub(crate) fn publish(&self, logical_name: &str, endpoint: ChildEndpoint) {
        if let Some(sender) = self.entries.get(logical_name) {
            sender.send_replace(Some(endpoint));
        }
    }

    /// Marks the named child down so consumers wait for the restart instead
    /// of dispatching into a dead handle.
    pub(crate) fn unpublish(&self, logical_name: &str) {
        if let Some(sender) = self.entries.get(logical_name) {
            sender.send_replace(None);
        }
    }

    /// Resolves the live endpoint whose declared operations include
    /// `operation`, or `None` while every matching child is down.
    pub(crate) fn resolve(&self, operation: OperationKind) -> Option<ChildEndpoint> {
        self.entries
            .values()
            .filter_map(|sender| sender.borrow().clone())
            .find(|endpoint| endpoint.operations.contains(&operation))
    }
}

/// Owns one node incarnation from activation through ordered deactivation.
#[derive(Debug)]
pub struct AgentRuntime {
    config: LoadedAgentConfig,
    client: Arc<dyn ControlPlaneApi>,
    /// Concrete HTTP transport for the scan-session pump; absent in tests
    /// that inject a fake [`ControlPlaneApi`], whose leases never route there.
    scan_client: Option<Arc<ControlPlaneClient>>,
    budgets: ShutdownBudgets,
}

impl AgentRuntime {
    /// Build an agent runtime from validated configuration.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the control-plane client cannot be built.
    pub fn new(config: LoadedAgentConfig) -> Result<Self, VoomError> {
        let client = ControlPlaneClient::from_config(&config)?;
        let client = Arc::new(client);
        Ok(Self {
            config,
            scan_client: Some(Arc::clone(&client)),
            client,
            budgets: ShutdownBudgets::DEFAULT,
        })
    }

    #[cfg(test)]
    fn with_client(config: LoadedAgentConfig, client: Arc<dyn ControlPlaneApi>) -> Self {
        Self::with_client_and_budgets(config, client, ShutdownBudgets::DEFAULT)
    }

    #[cfg(test)]
    fn with_client_and_budgets(
        config: LoadedAgentConfig,
        client: Arc<dyn ControlPlaneApi>,
        budgets: ShutdownBudgets,
    ) -> Self {
        Self {
            config,
            scan_client: None,
            client,
            budgets,
        }
    }

    fn budgets(&self) -> ShutdownBudgets {
        self.budgets
    }

    /// Run until SIGINT or SIGTERM requests an orderly shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when activation, child supervision, fencing, or deactivation fails.
    pub async fn run(self) -> Result<(), VoomError> {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let runtime = self.run_with_shutdowns(signal_rx);
        let signals = forward_os_shutdowns(signal_tx);
        tokio::pin!(runtime, signals);
        tokio::select! {
            result = &mut runtime => result,
            () = &mut signals => Err(VoomError::Internal(
                "node-agent signal listener stopped unexpectedly".to_owned(),
            )),
        }
    }

    /// Run until the supplied shutdown future resolves.
    ///
    /// This seam lets black-box tests request shutdown without sending a process-wide signal.
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::run`].
    pub async fn run_until<F>(self, shutdown: F) -> Result<(), VoomError>
    where
        F: Future<Output = ()> + Send,
    {
        self.run_until_signals(shutdown, std::future::pending())
            .await
    }

    async fn run_until_signals<F, S>(self, first: F, second: S) -> Result<(), VoomError>
    where
        F: Future<Output = ()> + Send,
        S: Future<Output = ()> + Send,
    {
        let (signal_tx, signal_rx) = mpsc::unbounded_channel();
        let runtime = self.run_with_shutdowns(signal_rx);
        let signals = async move {
            forward_shutdowns(first, second, signal_tx).await;
            std::future::pending::<()>().await;
        };
        tokio::pin!(runtime, signals);
        tokio::select! {
            result = &mut runtime => result,
            () = &mut signals => unreachable!("shutdown forwarder remains pending"),
        }
    }

    async fn run_with_shutdowns(
        self,
        signals: mpsc::UnboundedReceiver<()>,
    ) -> Result<(), VoomError> {
        self.run_with_shutdowns_from_rng(signals, &mut OsRng).await
    }

    async fn run_with_shutdowns_from_rng<R>(
        self,
        signals: mpsc::UnboundedReceiver<()>,
        source: &mut R,
    ) -> Result<(), VoomError>
    where
        R: TryRngCore,
    {
        let schedule_rng = schedule_rng_from(source)?;
        self.run_with_seeded_shutdowns(signals, schedule_rng).await
    }

    async fn run_with_seeded_shutdowns(
        self,
        mut signals: mpsc::UnboundedReceiver<()>,
        mut schedule_rng: StdRng,
    ) -> Result<(), VoomError> {
        let incarnation_id = NodeIncarnationId::generate()?;
        let activation = tokio::select! {
            result = self.activate(incarnation_id) => result?,
            _ = signals.recv() => return Ok(()),
        };
        let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
        let node_heartbeat_rng = derive_schedule_rng(&mut schedule_rng);
        let node_heartbeat = self
            .start_node_heartbeat(
                incarnation_id,
                activation.heartbeat_ttl_seconds,
                fatal_tx.clone(),
                node_heartbeat_rng,
            )
            .await?;

        let specs = self.child_specs(&activation)?;
        let startup_supervisor =
            ChildSupervisor::new(self.shutdown_grace(), self.budgets().reap_after_kill);
        let children = match startup_supervisor.start_all(specs).await {
            Ok(children) => children,
            Err(error) => {
                node_heartbeat.stop();
                self.deactivate_after_startup_failure(incarnation_id, &mut signals)
                    .await?;
                return Err(VoomError::ExternalSystemUnavailable(format!(
                    "start node-agent children: {error}"
                )));
            }
        };
        if let Err(error) = self
            .mark_started_children_ready(incarnation_id, &children)
            .await
        {
            node_heartbeat.stop();
            let shutdown = startup_supervisor.shutdown_all(children).await;
            self.deactivate_after_startup_failure(incarnation_id, &mut signals)
                .await?;
            if let Err(shutdown_error) = shutdown {
                return Err(VoomError::ExternalSystemUnavailable(format!(
                    "mark node-agent children ready: {error}; shut down children: {shutdown_error}"
                )));
            }
            return Err(VoomError::ExternalSystemUnavailable(format!(
                "mark node-agent children ready: {error}"
            )));
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
        let mut coordinators = self.spawn_coordinators(
            children,
            incarnation_id,
            &shutdown_rx,
            &fatal_tx,
            &mut schedule_rng,
        )?;
        let exit = tokio::select! {
            signal = signals.recv() => {
                if signal.is_some() {
                    RuntimeExit::Graceful
                } else {
                    RuntimeExit::Fatal(RuntimeFatal::Internal(
                        "node-agent shutdown signal source closed".to_owned(),
                    ))
                }
            },
            Some(fatal) = fatal_rx.recv() => RuntimeExit::Fatal(fatal),
            joined = coordinators.join_next() => coordinator_exit(joined),
        };

        let shutdown_kind = shutdown_kind_for_exit(&exit);
        let signal_phase = signal_phase_for_exit(&exit);
        let _ = shutdown_tx.send(shutdown_kind);
        let bound = self.budgets().tail(self.shutdown_grace());
        let tail = async {
            let settled =
                wait_for_coordinators(&mut coordinators, &shutdown_tx, &mut signals, signal_phase)
                    .await;
            self.finish_shutdown_lifecycle(
                incarnation_id,
                exit,
                settled,
                node_heartbeat,
                &mut signals,
            )
            .await
        };
        run_shutdown_tail_within(bound, tail).await
    }

    /// Deactivate after a child-startup failure, within one call budget.
    ///
    /// Both startup-failure paths run this identically; they precede any shutdown tail,
    /// so each takes its own budget from now rather than sharing the tail's.
    async fn deactivate_after_startup_failure(
        &self,
        incarnation_id: NodeIncarnationId,
        signals: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<(), VoomError> {
        let mut signal_phase = ShutdownSignalPhase::AwaitingFirst;
        self.deactivate_or_second_signal(
            incarnation_id,
            NodeIncarnationEndReason::ChildStartupFailed,
            signals,
            &mut signal_phase,
            Instant::now() + self.budgets().call,
        )
        .await
    }

    async fn finish_shutdown_lifecycle(
        &self,
        incarnation_id: NodeIncarnationId,
        exit: RuntimeExit,
        settled: Result<ShutdownProgress, VoomError>,
        node_heartbeat: NodeHeartbeatHandle,
        signals: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<(), VoomError> {
        // Stop the heartbeat before propagating a coordinator join failure, or the task
        // keeps heartbeating an incarnation this runtime has already abandoned.
        node_heartbeat.stop();
        let progress = settled?;
        let reason = match &exit {
            RuntimeExit::Graceful => NodeIncarnationEndReason::GracefulShutdown,
            RuntimeExit::Fatal(_) => return Err(exit.into_error()),
            RuntimeExit::RestartExhausted => NodeIncarnationEndReason::ChildRestartExhausted,
        };
        // A signal force is the operator saying stop, and skips the write as it always
        // has. A deadline expiring is not that instruction: the write is what this whole
        // bound exists to protect, and attempting it costs one more bounded call.
        if progress.forced == Some(ShutdownForce::Signal) {
            return Err(forced_shutdown_error());
        }
        let mut signal_phase = progress.signal_phase;
        self.deactivate_or_second_signal(
            incarnation_id,
            reason,
            signals,
            &mut signal_phase,
            Instant::now() + self.budgets().call,
        )
        .await?;
        if progress.forced == Some(ShutdownForce::Deadline) {
            // The write may have landed; the settlement that preceded it did not, so the
            // agent still exits unsuccessfully and names the budget rather than a signal.
            return Err(shutdown_deadline_error());
        }
        match exit {
            RuntimeExit::Graceful => Ok(()),
            RuntimeExit::RestartExhausted => Err(VoomError::ExternalSystemUnavailable(
                "node-agent child restart budget exhausted".to_owned(),
            )),
            RuntimeExit::Fatal(_) => unreachable!("fatal exits return before deactivation"),
        }
    }

    async fn activate(
        &self,
        incarnation_id: NodeIncarnationId,
    ) -> Result<ActivateOutcome, VoomError> {
        let workers = self
            .config
            .config
            .workers
            .iter()
            .map(|worker| WorkerDeclaration {
                logical_name: worker.name.clone(),
                operations: worker.operations.clone(),
                artifact_access: worker.artifact_access.clone(),
                accelerator: worker.accelerator.clone(),
                max_parallel: worker.max_parallel,
            })
            .collect();
        let request = RetryRequest::new(
            new_key("activate"),
            &ActivateRequest {
                incarnation_id,
                workers,
            },
        )?;
        let outcome = self
            .client
            .activate(self.config.config.node_id, &request)
            .await?;
        if outcome.node_id != self.config.config.node_id || outcome.incarnation_id != incarnation_id
        {
            return Err(VoomError::ExternalSystemUnavailable(
                "activation response identity does not match the request".to_owned(),
            ));
        }
        validate_activated_workers(&self.config.config.workers, &outcome.workers)?;
        Ok(outcome)
    }

    async fn mark_started_children_ready(
        &self,
        incarnation_id: NodeIncarnationId,
        children: &[RunningChild],
    ) -> Result<(), VoomError> {
        for child in children {
            set_worker_readiness(
                self.client.as_ref(),
                self.config.config.node_id,
                incarnation_id,
                child.spec().credentials().worker_id,
                WorkerReadiness::Ready,
            )
            .await?;
        }
        Ok(())
    }

    async fn start_node_heartbeat(
        &self,
        incarnation_id: NodeIncarnationId,
        ttl_seconds: u32,
        fatal_tx: mpsc::UnboundedSender<RuntimeFatal>,
        schedule_rng: StdRng,
    ) -> Result<NodeHeartbeatHandle, VoomError> {
        send_node_heartbeat(
            self.client.as_ref(),
            self.config.config.node_id,
            incarnation_id,
        )
        .await?;
        let interval = heartbeat_interval(ttl_seconds);
        // Keep the deadline at least one interval beyond the beat. An advertised TTL of 0 or
        // 1 would otherwise be spent by the very first sleep and self-fence every start.
        let ttl = Duration::from_secs(u64::from(ttl_seconds)).max(interval.saturating_mul(2));
        let client = Arc::clone(&self.client);
        let node_id = self.config.config.node_id;
        let (stop_tx, stop_rx) = watch::channel(false);
        let joined = tokio::spawn(async move {
            node_heartbeat_loop(
                client,
                node_id,
                incarnation_id,
                NodeHeartbeatTiming { interval, ttl },
                fatal_tx,
                stop_rx,
                schedule_rng,
            )
            .await;
        });
        Ok(NodeHeartbeatHandle {
            stop_tx,
            joined,
            #[cfg(test)]
            stop_observer: None,
        })
    }

    fn child_specs(&self, activation: &ActivateOutcome) -> Result<Vec<ChildSpec>, VoomError> {
        let activated = activation
            .workers
            .iter()
            .map(|worker| (worker.logical_name.as_str(), worker))
            .collect::<HashMap<_, _>>();
        self.config
            .config
            .workers
            .iter()
            .map(|worker| {
                let descriptor = activated.get(worker.name.as_str()).ok_or_else(|| {
                    VoomError::ExternalSystemUnavailable(format!(
                        "activation omitted worker {:?}",
                        worker.name
                    ))
                })?;
                let credentials = WorkerCredentials {
                    worker_id: descriptor.worker_id,
                    worker_epoch: descriptor.worker_epoch,
                    secret: SecretString::from(random_hex(32)),
                };
                Ok(ChildSpec::from_worker(worker, credentials))
            })
            .collect()
    }

    fn spawn_coordinators(
        &self,
        children: Vec<RunningChild>,
        incarnation_id: NodeIncarnationId,
        shutdown: &watch::Receiver<ShutdownKind>,
        fatal_tx: &mpsc::UnboundedSender<RuntimeFatal>,
        schedule_rng: &mut StdRng,
    ) -> Result<JoinSet<CoordinatorExit>, VoomError> {
        let workers = self
            .config
            .config
            .workers
            .iter()
            .map(|worker| (worker.name.as_str(), worker))
            .collect::<HashMap<_, _>>();
        let endpoints = ChildEndpointRegistry::new(&self.config.config.workers);
        let storage_roots = storage_root_bindings(&self.config.config)?;
        let mut coordinators = JoinSet::new();
        for child in children {
            let worker = workers.get(child.spec().logical_name()).ok_or_else(|| {
                VoomError::Internal("started child has no configuration".to_owned())
            })?;
            let context = CoordinatorContext {
                client: Arc::clone(&self.client),
                scan_client: self.scan_client.clone(),
                endpoints: endpoints.clone(),
                node_id: self.config.config.node_id,
                incarnation_id,
                worker_id: child.spec().credentials().worker_id,
                lease_ttl: Duration::from_secs(u64::from(self.config.config.lease_ttl_seconds)),
                progress_timeout: Duration::from_secs(u64::from(
                    self.config.config.progress_idle_timeout_seconds,
                )),
                poll_interval: Duration::from_millis(self.config.config.poll_interval_ms),
                shutdown_grace: self.shutdown_grace(),
                budgets: self.budgets(),
                worker: (*worker).clone(),
                storage_roots: storage_roots.clone(),
                fatal_tx: fatal_tx.clone(),
            };
            let coordinator_rng = derive_schedule_rng(schedule_rng);
            coordinators.spawn(run_coordinator(
                child,
                context,
                shutdown.clone(),
                coordinator_rng,
            ));
        }

        let commit_rng = derive_schedule_rng(schedule_rng);
        let commit_context = CommitCoordinatorContext {
            api: Arc::clone(&self.client),
            node_id: self.config.config.node_id,
            incarnation_id,
            poll_interval: Duration::from_millis(self.config.config.poll_interval_ms),
            storage_roots,
        };
        coordinators.spawn(run_commit_coordinator(
            commit_context,
            shutdown.clone(),
            commit_rng,
        ));
        Ok(coordinators)
    }

    async fn deactivate(
        &self,
        incarnation_id: NodeIncarnationId,
        reason: NodeIncarnationEndReason,
    ) -> Result<(), VoomError> {
        let request = RetryRequest::new(
            new_key("deactivate"),
            &DeactivateRequest {
                incarnation_id,
                reason,
            },
        )?;
        self.client
            .deactivate(self.config.config.node_id, &request)
            .await?;
        Ok(())
    }

    /// Deactivate, letting a further termination signal abandon a deactivation that the
    /// control plane is not answering. Every deactivation path uses this: the runbook
    /// promises a second signal cancels blocked deactivation, without qualifying which
    /// reason ended the incarnation.
    ///
    /// `deadline` is a parameter rather than a constant read here, because three
    /// existing tests gate `deactivate` with a `Notify` that is never notified: a real
    /// timer read in place would turn each of them into a wall-clock race.
    async fn deactivate_or_second_signal(
        &self,
        incarnation_id: NodeIncarnationId,
        reason: NodeIncarnationEndReason,
        signals: &mut mpsc::UnboundedReceiver<()>,
        signal_phase: &mut ShutdownSignalPhase,
        deadline: Instant,
    ) -> Result<(), VoomError> {
        let deactivation = self.deactivate(incarnation_id, reason);
        tokio::pin!(deactivation);
        loop {
            tokio::select! {
                result = &mut deactivation => return result,
                () = tokio::time::sleep_until(deadline) => {
                    return Err(shutdown_deadline_error());
                }
                signal = signals.recv() => {
                    let Some(()) = signal else {
                        return Err(forced_shutdown_error());
                    };
                    if signal_phase.signal_forces() {
                        return Err(forced_shutdown_error());
                    }
                }
            }
        }
    }

    fn shutdown_grace(&self) -> Duration {
        Duration::from_secs(u64::from(self.config.config.shutdown_grace_seconds))
    }
}

struct NodeHeartbeatTiming {
    interval: Duration,
    ttl: Duration,
}

async fn node_heartbeat_loop(
    client: Arc<dyn ControlPlaneApi>,
    node_id: voom_core::NodeId,
    incarnation_id: NodeIncarnationId,
    timing: NodeHeartbeatTiming,
    fatal_tx: mpsc::UnboundedSender<RuntimeFatal>,
    mut stop: watch::Receiver<bool>,
    mut schedule_rng: StdRng,
) {
    // The control plane expires the incarnation `ttl` after the last heartbeat it observed.
    // Self-fence at that deadline instead of retrying while children keep acquiring against a
    // dead incarnation.
    let mut last_success = Instant::now();
    loop {
        let delay = node_heartbeat_delay(timing.interval, &mut schedule_rng);
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(delay) => {
                let Some(budget) = timing.ttl.checked_sub(last_success.elapsed()) else {
                    let _ = fatal_tx.send(node_heartbeat_expiry());
                    return;
                };
                match tokio::time::timeout(
                    budget,
                    send_node_heartbeat(client.as_ref(), node_id, incarnation_id),
                )
                .await
                {
                    Ok(Ok(())) => last_success = Instant::now(),
                    Ok(Err(error)) => {
                        let _ = fatal_tx.send(RuntimeFatal::ControlPlane(error));
                        return;
                    }
                    Err(_) => {
                        let _ = fatal_tx.send(node_heartbeat_expiry());
                        return;
                    }
                }
            }
        }
    }
}

struct NodeHeartbeatHandle {
    stop_tx: watch::Sender<bool>,
    joined: tokio::task::JoinHandle<()>,
    #[cfg(test)]
    stop_observer: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl NodeHeartbeatHandle {
    fn stop(self) {
        #[cfg(test)]
        if let Some(observer) = &self.stop_observer {
            observer();
        }
        let _ = self.stop_tx.send(true);
        self.joined.abort();
    }
}

#[derive(Clone)]
pub(crate) struct CoordinatorContext {
    pub(crate) client: Arc<dyn ControlPlaneApi>,
    /// Concrete HTTP transport for the scan-session pump; absent in tests.
    pub(crate) scan_client: Option<Arc<ControlPlaneClient>>,
    pub(crate) node_id: voom_core::NodeId,
    pub(crate) incarnation_id: NodeIncarnationId,
    pub(crate) worker_id: WorkerId,
    pub(crate) lease_ttl: Duration,
    pub(crate) progress_timeout: Duration,
    pub(crate) poll_interval: Duration,
    pub(crate) shutdown_grace: Duration,
    pub(crate) budgets: ShutdownBudgets,
    pub(crate) worker: WorkerConfig,
    pub(crate) endpoints: ChildEndpointRegistry,
    /// Configured storage-root provider locators by root id (spawn-time
    /// index shared with the commit coordinator); media dispatch resolves
    /// location handles against it.
    pub(crate) storage_roots: HashMap<u64, PathBuf>,
    pub(crate) fatal_tx: mpsc::UnboundedSender<RuntimeFatal>,
}

#[derive(Debug)]
pub(crate) enum CoordinatorExit {
    Shutdown(LeaseSettlement),
    RestartExhausted,
    Fatal(RuntimeFatal),
}

/// Publishes one coordinator's live child into the shared endpoint registry.
fn publish_child_endpoint(context: &CoordinatorContext, child: &RunningChild) {
    context.endpoints.publish(
        &context.worker.name,
        ChildEndpoint {
            client: child.client(),
            credentials: child.spec().credentials().clone(),
            operations: context.worker.operations.clone(),
        },
    );
}

async fn run_coordinator(
    mut child: RunningChild,
    context: CoordinatorContext,
    mut shutdown: watch::Receiver<ShutdownKind>,
    mut schedule_rng: StdRng,
) -> CoordinatorExit {
    let permits = Arc::new(Semaphore::new(context.worker.max_parallel as usize));
    let mut leases = JoinSet::new();
    let mut crashes = CrashBudget::new();
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let mut poll_before_acquire = false;
    publish_child_endpoint(&context, &child);
    loop {
        drain_finished_leases(&mut leases);
        let event = if poll_before_acquire {
            poll_before_acquire = false;
            acquisition_poll_event(
                context.poll_interval,
                &mut schedule_rng,
                &mut shutdown,
                child.wait_for_exit(),
            )
            .await
        } else {
            let acquire = acquire_one(&context, Arc::clone(&permits));
            tokio::pin!(acquire);
            tokio::select! {
                changed = shutdown.changed() => {
                    let kind = if changed.is_err() {
                        ShutdownKind::Fenced
                    } else {
                        *shutdown.borrow()
                    };
                    CoordinatorEvent::Shutdown(kind)
                }
                status = child.wait_for_exit() => CoordinatorEvent::ChildExit(status.map(|_| ())),
                result = &mut acquire => CoordinatorEvent::Acquire(result),
            }
        };
        match event {
            CoordinatorEvent::Acquire(Ok(Acquired::Idle)) => {
                poll_before_acquire = true;
            }
            CoordinatorEvent::Acquire(Ok(Acquired::Lease(lease))) => {
                let lease_context = context.clone();
                let client = child.client();
                let credentials = child.spec().credentials().clone();
                let lease_rng = derive_schedule_rng(&mut schedule_rng);
                leases.spawn(run_lease(
                    *lease,
                    client,
                    credentials,
                    lease_context,
                    cancel_rx.clone(),
                    shutdown.clone(),
                    lease_rng,
                ));
            }
            CoordinatorEvent::Acquire(Ok(Acquired::Terminal)) => {
                // A lease that validated as terminal yields nothing to run. Without this the
                // coordinator spins acquire/heartbeat pairs at full rate.
                poll_before_acquire = true;
            }
            CoordinatorEvent::Acquire(Err(error)) => return CoordinatorExit::Fatal(error),
            CoordinatorEvent::Shutdown(kind) => {
                let settlement = settle_leases_for_shutdown(
                    &cancel_tx,
                    &mut leases,
                    &mut shutdown,
                    kind,
                    Some(Instant::now() + context.budgets.call),
                )
                .await;
                let supervisor =
                    ChildSupervisor::new(context.shutdown_grace, context.budgets.reap_after_kill);
                let _ = supervisor.shutdown_all(vec![child]).await;
                return CoordinatorExit::Shutdown(settlement);
            }
            CoordinatorEvent::ChildExit(Ok(())) => {
                match restart_after_child_exit(
                    child,
                    &context,
                    &cancel_tx,
                    &mut leases,
                    &mut shutdown,
                    &mut crashes,
                )
                .await
                {
                    Ok(restarted) => child = restarted,
                    Err(exit) => return exit,
                }
            }
            CoordinatorEvent::ChildExit(Err(error)) => {
                return CoordinatorExit::Fatal(RuntimeFatal::Internal(error.to_string()));
            }
            CoordinatorEvent::PollElapsed => {}
        }
    }
}

/// Await `work`, abandoning it once a shutdown is in flight.
///
/// The predicate runs on a **clone** of the receiver. `changed()` on the original
/// would mark the value seen, and that value is sent exactly once; the
/// `cancel_and_wait` wait immediately below is deliberately unbounded and
/// `shutdown.changed()` is its only escape, so consuming the notification here would
/// strand it against the same unresponsive control plane. A clone tracks its own seen
/// version, and `wait_for` checks the current value first.
async fn until_shutdown<F, T>(shutdown: &watch::Receiver<ShutdownKind>, work: F) -> Option<T>
where
    F: Future<Output = T>,
{
    let mut watcher = shutdown.clone();
    tokio::select! {
        value = work => Some(value),
        _ = watcher.wait_for(|kind| *kind != ShutdownKind::Running) => None,
    }
}

/// Reap `child` and report the shutdown that interrupted a restart.
async fn shutdown_during_restart(
    context: &CoordinatorContext,
    child: RunningChild,
) -> CoordinatorExit {
    // Mirrors the shutdown branch above: closed and reaped through the supervisor
    // rather than left to `Drop`, which `SIGKILL`s. That matters most at the `Ready`
    // call site, where the child is a freshly launched replacement that has not run
    // and would otherwise lose its `shutdown_grace_seconds`; at the `restart_child`
    // site the child has already exited and this only collects its status.
    let supervisor = ChildSupervisor::new(context.shutdown_grace, context.budgets.reap_after_kill);
    let _ = supervisor.shutdown_all(vec![child]).await;
    CoordinatorExit::Shutdown(LeaseSettlement::Completed)
}

async fn restart_after_child_exit(
    child: RunningChild,
    context: &CoordinatorContext,
    cancel_tx: &watch::Sender<LeaseCancellation>,
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    crashes: &mut CrashBudget,
) -> Result<RunningChild, CoordinatorExit> {
    context.endpoints.unpublish(&context.worker.name);
    // A readiness update for a worker that is going away is not worth waiting on, and
    // this call reaches the retrying client. Left unraced, a coordinator blocked here
    // holds up the whole shutdown tail for the client's full retry budget.
    let readiness = until_shutdown(
        shutdown,
        set_worker_readiness(
            context.client.as_ref(),
            context.node_id,
            context.incarnation_id,
            context.worker_id,
            WorkerReadiness::NotReady,
        ),
    )
    .await;
    if let Some(Err(error)) = readiness {
        return Err(CoordinatorExit::Fatal(RuntimeFatal::ControlPlane(error)));
    }
    // Settling consumes any shutdown notification, so it must be returned
    // instead of falling through to a restart waiting on an observed change.
    if let Some(settlement) =
        settle_leases_after_child_crash(cancel_tx, leases, shutdown, context.budgets.call).await
    {
        let supervisor =
            ChildSupervisor::new(context.shutdown_grace, context.budgets.reap_after_kill);
        let _ = supervisor.shutdown_all(vec![child]).await;
        return Err(CoordinatorExit::Shutdown(settlement));
    }
    if crashes.record_and_exhausted() {
        return Err(CoordinatorExit::RestartExhausted);
    }
    tokio::time::sleep(RESTART_DELAY).await;
    let spec = child.spec().clone();
    // Restarting a worker the agent is shutting down is wasted work, and an accelerator
    // launch can take three NVIDIA startup timeouts — minutes the tail cannot absorb.
    let Some(restarted) = until_shutdown(shutdown, restart_child(context, spec)).await else {
        return Err(shutdown_during_restart(context, child).await);
    };
    let restarted = restarted.map_err(|_| CoordinatorExit::RestartExhausted)?;
    let readiness = until_shutdown(
        shutdown,
        set_worker_readiness(
            context.client.as_ref(),
            context.node_id,
            context.incarnation_id,
            context.worker_id,
            WorkerReadiness::Ready,
        ),
    )
    .await;
    let Some(readiness) = readiness else {
        return Err(shutdown_during_restart(context, restarted).await);
    };
    readiness.map_err(|error| CoordinatorExit::Fatal(RuntimeFatal::ControlPlane(error)))?;
    publish_child_endpoint(context, &restarted);
    let _ = cancel_tx.send(LeaseCancellation::Running);
    Ok(restarted)
}

/// Bounds children that start cleanly and then crash. [`ChildSupervisor`] only counts
/// consecutive *launch* failures and resets that count on a successful launch, so without
/// this a worker that serves handshake and then dies on every dispatch respawns forever.
struct CrashBudget {
    window_started: Instant,
    crashes: u32,
}

impl CrashBudget {
    fn new() -> Self {
        Self {
            window_started: Instant::now(),
            crashes: 0,
        }
    }

    /// Record one crash episode and report whether this window's budget is spent.
    fn record_and_exhausted(&mut self) -> bool {
        if self.window_started.elapsed() >= CRASH_WINDOW {
            self.window_started = Instant::now();
            self.crashes = 0;
        }
        self.crashes += 1;
        self.crashes > CRASH_LIMIT
    }
}

async fn restart_child(
    context: &CoordinatorContext,
    spec: ChildSpec,
) -> Result<RunningChild, ChildErrorKind> {
    let mut supervisor =
        ChildSupervisor::new(context.shutdown_grace, context.budgets.reap_after_kill);
    loop {
        match supervisor.restart(&spec).await {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == ChildErrorKind::RestartExhausted => {
                return Err(ChildErrorKind::RestartExhausted);
            }
            Err(_) => tokio::time::sleep(RESTART_DELAY).await,
        }
    }
}

enum CoordinatorEvent {
    Acquire(Result<Acquired, RuntimeFatal>),
    ChildExit(Result<(), crate::child::ChildError>),
    PollElapsed,
    Shutdown(ShutdownKind),
}

async fn acquisition_poll_event<F, T>(
    interval: Duration,
    rng: &mut impl Rng,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    child_exit: F,
) -> CoordinatorEvent
where
    F: Future<Output = Result<T, crate::child::ChildError>>,
{
    let delay = acquisition_poll_delay(interval, rng);
    wait_for_acquisition_poll(delay, shutdown, child_exit).await
}

async fn wait_for_acquisition_poll<F, T>(
    delay: Duration,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    child_exit: F,
) -> CoordinatorEvent
where
    F: Future<Output = Result<T, crate::child::ChildError>>,
{
    tokio::select! {
        changed = shutdown.changed() => {
            let kind = if changed.is_err() {
                ShutdownKind::Fenced
            } else {
                *shutdown.borrow()
            };
            CoordinatorEvent::Shutdown(kind)
        }
        status = child_exit => CoordinatorEvent::ChildExit(status.map(|_| ())),
        () = tokio::time::sleep(delay) => CoordinatorEvent::PollElapsed,
    }
}

enum Acquired {
    Idle,
    Lease(Box<HeldLeaseGuard>),
    Terminal,
}

/// Holds one worker permit until the lease reaches acknowledged terminal state.
#[derive(Debug)]
pub struct HeldLeaseGuard {
    dispatch: LeaseDispatch,
    _permit: OwnedSemaphorePermit,
}

async fn acquire_one(
    context: &CoordinatorContext,
    permits: Arc<Semaphore>,
) -> Result<Acquired, RuntimeFatal> {
    let permit = permits
        .acquire_owned()
        .await
        .map_err(|_| RuntimeFatal::Internal("worker permit semaphore closed".to_owned()))?;
    let request = RetryRequest::new(
        new_key("acquire"),
        &AcquireRequest {
            node_id: context.node_id,
            worker_id: context.worker_id(),
            incarnation_id: context.incarnation_id,
            lease_ttl_seconds: duration_seconds_i64(context.lease_ttl),
        },
    )
    .map_err(RuntimeFatal::ControlPlane)?;
    let outcome = context
        .client
        .acquire(&request)
        .await
        .map_err(classify_control_plane_error)?;
    let AcquireOutcome::Leased(dispatch) = outcome else {
        return Ok(Acquired::Idle);
    };
    if dispatch.worker_id != context.worker_id() {
        return Err(RuntimeFatal::Internal(format!(
            "acquire returned worker {:?} for requested {:?}",
            dispatch.worker_id,
            context.worker_id()
        )));
    }
    match validate_lease(context, &dispatch).await? {
        ValidationOutcome::Fresh => Ok(Acquired::Lease(Box::new(HeldLeaseGuard {
            dispatch,
            _permit: permit,
        }))),
        ValidationOutcome::Terminal => Ok(Acquired::Terminal),
    }
}

enum ValidationOutcome {
    Fresh,
    Terminal,
}

async fn validate_lease(
    context: &CoordinatorContext,
    dispatch: &LeaseDispatch,
) -> Result<ValidationOutcome, RuntimeFatal> {
    let granted_ttl = granted_lease_ttl(dispatch.lease_ttl_seconds);
    let freshness = granted_ttl / 2;
    for _ in 0..VALIDATION_ATTEMPTS {
        let started = Instant::now();
        let request = lease_heartbeat_request(context, new_key("validate"), granted_ttl)
            .map_err(RuntimeFatal::ControlPlane)?;
        let result = tokio::time::timeout(
            freshness,
            context.client.lease_heartbeat(dispatch.lease_id, &request),
        )
        .await;
        match result {
            Ok(Ok(_)) if is_validation_fresh(started.elapsed(), granted_ttl) => {
                return Ok(ValidationOutcome::Fresh);
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(VoomError::Conflict(_) | VoomError::NotFound(_))) => {
                return Ok(ValidationOutcome::Terminal);
            }
            Ok(Err(error)) => return Err(classify_control_plane_error(error)),
        }
    }
    // Every attempt either timed out or came back too stale to trust. Give the lease back
    // rather than renewing it forever while holding a worker permit.
    Ok(ValidationOutcome::Terminal)
}

async fn run_lease(
    lease: HeldLeaseGuard,
    child: Arc<dyn ClientHandle>,
    credentials: WorkerCredentials,
    context: CoordinatorContext,
    mut worker_cancel: watch::Receiver<LeaseCancellation>,
    mut shutdown: watch::Receiver<ShutdownKind>,
    schedule_rng: StdRng,
) {
    let lease_id = lease.dispatch.lease_id;
    let (heartbeat_stop_tx, heartbeat_stop_rx) = watch::channel(false);
    let (lease_fence_tx, mut lease_fence_rx) = mpsc::channel(1);
    let heartbeat_context = context.clone();
    let heartbeat = tokio::spawn(async move {
        lease_heartbeat_loop(
            heartbeat_context,
            lease_id,
            lease.dispatch.heartbeat_after_seconds,
            lease.dispatch.lease_ttl_seconds,
            heartbeat_stop_rx,
            lease_fence_tx,
            schedule_rng,
        )
        .await;
    });

    let outcome = tokio::select! {
        outcome = dispatch_outcome(&lease.dispatch, Arc::clone(&child), &credentials, &context) => outcome,
        changed = worker_cancel.changed() => cancellation_outcome(&changed, *worker_cancel.borrow()),
        changed = shutdown.changed() => shutdown_outcome(&changed, *shutdown.borrow()),
        Some(()) = lease_fence_rx.recv() => LeaseOutcome::TerminalElsewhere,
    };
    if let Err(error) = settle_lease(&context, &lease.dispatch, outcome).await {
        let _ = context.fatal_tx.send(error);
    }
    let _ = heartbeat_stop_tx.send(true);
    heartbeat.abort();
}

async fn lease_heartbeat_loop(
    context: CoordinatorContext,
    lease_id: LeaseId,
    heartbeat_after_seconds: i64,
    lease_ttl_seconds: i64,
    mut stop: watch::Receiver<bool>,
    fence: mpsc::Sender<()>,
    mut schedule_rng: StdRng,
) {
    // Both the beat and the deadline come from the grant the control plane returned, not
    // from local config: the server expires the lease on its own granted TTL, so anything
    // longer here reopens the redispatch window this deadline exists to close. A grant whose
    // TTL is not longer than its beat is incoherent and fences almost immediately; that is
    // the honest outcome, since the server would expire such a lease just as fast.
    let seconds = u64::try_from(heartbeat_after_seconds).unwrap_or(1).max(1);
    let interval = Duration::from_secs(seconds);
    let ttl = granted_lease_ttl(lease_ttl_seconds);
    // The control plane expires the lease `ttl` after the last heartbeat it observed. Fence
    // locally at that same deadline so the child stops before the ticket is redispatched.
    let mut last_success = Instant::now();
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(lease_heartbeat_delay(interval, ttl, &mut schedule_rng)) => {
                let Ok(request) = lease_heartbeat_request(
                    &context,
                    new_key("lease-heartbeat"),
                    ttl,
                ) else {
                    let _ = context.fatal_tx.send(RuntimeFatal::Internal(
                        "build lease heartbeat request".to_owned(),
                    ));
                    return;
                };
                let Some(budget) = ttl.checked_sub(last_success.elapsed()) else {
                    let _ = fence.send(()).await;
                    return;
                };
                match tokio::time::timeout(
                    budget,
                    context.client.lease_heartbeat(lease_id, &request),
                )
                .await
                {
                    Ok(Ok(_)) => last_success = Instant::now(),
                    Ok(Err(VoomError::Conflict(_) | VoomError::NotFound(_))) | Err(_) => {
                        let _ = fence.send(()).await;
                        return;
                    }
                    Ok(Err(error)) => {
                        let _ = context.fatal_tx.send(classify_control_plane_error(error));
                        return;
                    }
                }
            }
        }
    }
}

/// Routes one leased operation: `scan_library` drives the durable scan-session
/// pump against the shared child-endpoint registry; byte-touching media
/// operations run through the node-local media executor (ADR 0075 design C3);
/// everything else keeps the plain single-child dispatch.
async fn dispatch_outcome(
    dispatch: &LeaseDispatch,
    child: Arc<dyn ClientHandle>,
    credentials: &WorkerCredentials,
    context: &CoordinatorContext,
) -> LeaseOutcome {
    if dispatch.operation == OperationKind::ScanLibrary.as_str() {
        return scan_library_outcome(dispatch, child, credentials, context).await;
    }
    if crate::media::is_media_dispatch_operation(&dispatch.operation) {
        return crate::media::media_outcome(
            dispatch,
            child,
            credentials,
            context,
            &context.storage_roots,
            &context.endpoints,
        )
        .await;
    }
    dispatch_to_child(dispatch, child.as_ref(), credentials, context).await
}

async fn scan_library_outcome(
    dispatch: &LeaseDispatch,
    child: Arc<dyn ClientHandle>,
    credentials: &WorkerCredentials,
    context: &CoordinatorContext,
) -> LeaseOutcome {
    let Some(scan_client) = context.scan_client.as_deref() else {
        return LeaseOutcome::Failure(
            FailureClass::MalformedWorkerResult,
            "scan_library lease dispatched without an HTTP control-plane transport".to_owned(),
            json!({}),
        );
    };
    crate::scan_session::pump_scan_session(
        dispatch,
        child,
        credentials,
        &context.endpoints,
        scan_client,
        &crate::scan_session::ScanPumpContext {
            node_id: context.node_id.0,
            incarnation_id: context.incarnation_id,
            lease_id: dispatch.lease_id,
            lease_ttl_ms: duration_millis_u32(context.lease_ttl),
            progress_timeout: context.progress_timeout,
        },
    )
    .await
}

async fn dispatch_to_child(
    dispatch: &LeaseDispatch,
    child: &dyn ClientHandle,
    credentials: &WorkerCredentials,
    context: &CoordinatorContext,
) -> LeaseOutcome {
    let Some(operation) = OperationKind::from_wire(&dispatch.operation) else {
        return LeaseOutcome::Failure(
            FailureClass::MalformedWorkerResult,
            format!("unknown operation {:?}", dispatch.operation),
            json!({}),
        );
    };
    let payload = match augment_payload(dispatch, &context.worker) {
        Ok(payload) => payload,
        Err(message) => {
            return LeaseOutcome::Failure(FailureClass::MalformedWorkerResult, message, json!({}));
        }
    };
    let request = OperationRequest {
        operation,
        lease_id: dispatch.lease_id,
        payload,
        heartbeat_deadline_ms: duration_millis_u32(context.lease_ttl),
        progress_idle_deadline_ms: duration_millis_u32(context.progress_timeout),
    };
    let stream = match open_worker_stream(dispatch, child, credentials, context, request).await {
        Ok(stream) => stream,
        Err(outcome) => return outcome,
    };
    consume_progress_stream(stream, context.progress_timeout).await
}

pub(crate) async fn open_worker_stream(
    dispatch: &LeaseDispatch,
    child: &dyn ClientHandle,
    credentials: &WorkerCredentials,
    context: &CoordinatorContext,
    request: OperationRequest,
) -> Result<voom_worker_protocol::NdjsonStream, LeaseOutcome> {
    let dispatch_stream = tokio::time::timeout(
        context.progress_timeout,
        child.dispatch(
            credentials,
            &format!("{}-{}", context.incarnation_id, dispatch.lease_id.0),
            request,
        ),
    )
    .await;
    match dispatch_stream {
        Ok(Ok(stream)) => Ok(stream.frames),
        Ok(Err(error)) => Err(LeaseOutcome::Failure(
            FailureClass::MalformedWorkerResult,
            format!("worker dispatch failed: {error}"),
            json!({}),
        )),
        Err(_) => Err(progress_timeout_outcome(context.progress_timeout)),
    }
}

pub(crate) async fn consume_progress_stream(
    mut stream: voom_worker_protocol::NdjsonStream,
    progress_timeout: Duration,
) -> LeaseOutcome {
    loop {
        let frame = tokio::time::timeout(progress_timeout, stream.next_frame()).await;
        match frame {
            Err(_) => return progress_timeout_outcome(progress_timeout),
            Ok(Err(error)) => {
                return LeaseOutcome::Failure(
                    FailureClass::MalformedWorkerResult,
                    format!("worker progress stream failed: {error}"),
                    json!({}),
                );
            }
            Ok(Ok(NdjsonOutcome::StreamEnd)) => {
                return LeaseOutcome::Failure(
                    FailureClass::MalformedWorkerResult,
                    "worker progress stream ended before a terminal frame".to_owned(),
                    json!({}),
                );
            }
            Ok(Ok(NdjsonOutcome::Frame(ProgressFrame::Progress { .. }))) => {}
            Ok(Ok(NdjsonOutcome::Frame(_))) => {
                return LeaseOutcome::Failure(
                    FailureClass::MalformedWorkerResult,
                    "worker emitted a terminal frame as non-terminal progress".to_owned(),
                    json!({}),
                );
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Result { payload, .. }))) => {
                if payload.is_object() {
                    return LeaseOutcome::Complete(payload);
                }
                return LeaseOutcome::Failure(
                    FailureClass::MalformedWorkerResult,
                    "worker result payload must be an object".to_owned(),
                    json!({"payload": payload}),
                );
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Error {
                class,
                message,
                payload,
                ..
            }))) => {
                return LeaseOutcome::Failure(class, message, payload.unwrap_or_else(|| json!({})));
            }
            Ok(Ok(NdjsonOutcome::Terminated(ProgressFrame::Progress { .. }))) => {
                return LeaseOutcome::Failure(
                    FailureClass::MalformedWorkerResult,
                    "worker terminated with a progress frame".to_owned(),
                    json!({}),
                );
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum LeaseOutcome {
    Complete(JsonValue),
    Failure(FailureClass, String, JsonValue),
    TerminalElsewhere,
    Fenced,
}

async fn settle_lease(
    context: &CoordinatorContext,
    dispatch: &LeaseDispatch,
    outcome: LeaseOutcome,
) -> Result<(), RuntimeFatal> {
    match outcome {
        LeaseOutcome::Complete(result) => {
            let request = RetryRequest::new(
                new_key("complete"),
                &CompleteRequest {
                    node_id: context.node_id,
                    worker_id: context.worker_id(),
                    incarnation_id: context.incarnation_id,
                    result,
                },
            )
            .map_err(RuntimeFatal::ControlPlane)?;
            match context.client.complete(dispatch.lease_id, &request).await {
                Ok(_) | Err(VoomError::NotFound(_)) => Ok(()),
                Err(VoomError::Conflict(message)) => {
                    settle_failure(
                        context,
                        dispatch,
                        FailureClass::MalformedWorkerResult,
                        "control plane rejected the worker completion".to_owned(),
                        json!({"complete_rejection": message}),
                    )
                    .await
                }
                Err(error) => Err(classify_control_plane_error(error)),
            }
        }
        LeaseOutcome::Failure(class, reason, evidence) => {
            settle_failure(context, dispatch, class, reason, evidence).await
        }
        LeaseOutcome::TerminalElsewhere | LeaseOutcome::Fenced => Ok(()),
    }
}

async fn settle_failure(
    context: &CoordinatorContext,
    dispatch: &LeaseDispatch,
    class: FailureClass,
    reason: String,
    evidence: JsonValue,
) -> Result<(), RuntimeFatal> {
    let request = RetryRequest::new(
        new_key("fail"),
        &FailRequest {
            node_id: context.node_id,
            worker_id: context.worker_id(),
            incarnation_id: context.incarnation_id,
            reason,
            class,
            evidence,
        },
    )
    .map_err(RuntimeFatal::ControlPlane)?;
    let result = context
        .client
        .fail(dispatch.lease_id, &request)
        .await
        .map(|_| ());
    match result {
        Ok(()) | Err(VoomError::Conflict(_) | VoomError::NotFound(_)) => Ok(()),
        Err(error) => Err(classify_control_plane_error(error)),
    }
}

fn cancellation_outcome(
    changed: &Result<(), watch::error::RecvError>,
    cancellation: LeaseCancellation,
) -> LeaseOutcome {
    if changed.is_err() || cancellation == LeaseCancellation::Fenced {
        LeaseOutcome::Fenced
    } else {
        match cancellation {
            LeaseCancellation::Crash => LeaseOutcome::Failure(
                FailureClass::WorkerCrash,
                "child process exited while the lease was held".to_owned(),
                json!({}),
            ),
            LeaseCancellation::User => LeaseOutcome::Failure(
                FailureClass::UserCancellation,
                "node-agent shutdown cancelled the lease".to_owned(),
                json!({}),
            ),
            LeaseCancellation::Running | LeaseCancellation::Fenced => LeaseOutcome::Fenced,
        }
    }
}

fn shutdown_outcome(
    changed: &Result<(), watch::error::RecvError>,
    shutdown: ShutdownKind,
) -> LeaseOutcome {
    if changed.is_ok() && shutdown == ShutdownKind::User {
        LeaseOutcome::Failure(
            FailureClass::UserCancellation,
            "node-agent shutdown cancelled the lease".to_owned(),
            json!({}),
        )
    } else {
        LeaseOutcome::Fenced
    }
}

fn progress_timeout_outcome(timeout: Duration) -> LeaseOutcome {
    LeaseOutcome::Failure(
        FailureClass::ProgressTimeout,
        format!("worker emitted no valid progress for {timeout:?}"),
        json!({}),
    )
}

fn augment_payload(dispatch: &LeaseDispatch, worker: &WorkerConfig) -> Result<JsonValue, String> {
    let JsonValue::Object(mut payload) = dispatch.dispatch_payload.clone() else {
        return Err("dispatch payload must be an object".to_owned());
    };
    payload.insert(
        "artifact_access_plan".to_owned(),
        serde_json::to_value(&dispatch.artifact_access_plan)
            .map_err(|error| format!("encode artifact access plan: {error}"))?,
    );
    payload.insert(
        "advertised_artifact_access".to_owned(),
        serde_json::to_value(&worker.artifact_access)
            .map_err(|error| format!("encode advertised artifact access: {error}"))?,
    );
    Ok(JsonValue::Object(payload))
}

fn lease_heartbeat_request(
    context: &CoordinatorContext,
    key: String,
    granted_ttl: Duration,
) -> Result<RetryRequest<LeaseHeartbeatRequest>, VoomError> {
    RetryRequest::new(
        key,
        &LeaseHeartbeatRequest {
            node_id: context.node_id,
            worker_id: context.worker_id(),
            incarnation_id: context.incarnation_id,
            lease_ttl_seconds: duration_seconds_i64(granted_ttl),
        },
    )
}

async fn send_node_heartbeat(
    client: &dyn ControlPlaneApi,
    node_id: voom_core::NodeId,
    incarnation_id: NodeIncarnationId,
) -> Result<(), VoomError> {
    let request = RetryRequest::new(
        new_key("node-heartbeat"),
        &NodeHeartbeatRequest { incarnation_id },
    )?;
    client.node_heartbeat(node_id, &request).await?;
    Ok(())
}

async fn set_worker_readiness(
    client: &dyn ControlPlaneApi,
    node_id: voom_core::NodeId,
    incarnation_id: NodeIncarnationId,
    worker_id: WorkerId,
    readiness: WorkerReadiness,
) -> Result<(), VoomError> {
    let request = RetryRequest::new(
        new_key("worker-readiness"),
        &WorkerReadinessRequest {
            incarnation_id,
            readiness,
        },
    )?;
    let outcome = client
        .worker_readiness(node_id, worker_id, &request)
        .await?;
    if outcome.node_id != node_id
        || outcome.incarnation_id != incarnation_id
        || outcome.worker_id != worker_id
        || outcome.readiness != readiness
    {
        return Err(VoomError::ExternalSystemUnavailable(
            "worker readiness response identity does not match the request".to_owned(),
        ));
    }
    Ok(())
}

#[async_trait]
pub(crate) trait ControlPlaneApi: std::fmt::Debug + Send + Sync {
    async fn activate(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError>;

    async fn worker_readiness(
        &self,
        node_id: voom_core::NodeId,
        worker_id: WorkerId,
        request: &RetryRequest<WorkerReadinessRequest>,
    ) -> Result<WorkerReadinessOutcome, VoomError>;

    async fn deactivate(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError>;

    async fn node_heartbeat(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError>;

    async fn acquire(
        &self,
        request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError>;

    async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError>;

    async fn complete(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError>;

    async fn fail(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError>;

    async fn commit_open(
        &self,
        request: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError>;

    async fn authorize_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError>;

    async fn report_commit_applying(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError>;

    async fn report_commit_outcome(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<CommitReceiptOutcome, VoomError>;

    async fn complete_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError>;
}

#[async_trait]
impl ControlPlaneApi for ControlPlaneClient {
    async fn activate(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        Self::activate(self, node_id, request).await
    }

    async fn worker_readiness(
        &self,
        node_id: voom_core::NodeId,
        worker_id: WorkerId,
        request: &RetryRequest<WorkerReadinessRequest>,
    ) -> Result<WorkerReadinessOutcome, VoomError> {
        Self::worker_readiness(self, node_id, worker_id, request).await
    }

    async fn deactivate(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        Self::deactivate(self, node_id, request).await
    }

    async fn node_heartbeat(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        Self::node_heartbeat(self, node_id, request).await
    }

    async fn acquire(
        &self,
        request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError> {
        Self::acquire(self, request).await
    }

    async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        Self::lease_heartbeat(self, lease_id, request).await
    }

    async fn complete(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        Self::complete(self, lease_id, request).await
    }

    async fn fail(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        Self::fail(self, lease_id, request).await
    }

    async fn commit_open(
        &self,
        request: &RetryRequest<CommitOpenRequest>,
    ) -> Result<CommitOpenOutcome, VoomError> {
        Self::commit_open(self, request).await
    }

    async fn authorize_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitAuthorizeRequest>,
    ) -> Result<CommitAuthorizeOutcome, VoomError> {
        Self::authorize_commit_intent(self, intent_id, request).await
    }

    async fn report_commit_applying(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitApplyingRequest>,
    ) -> Result<CommitApplyingOutcome, VoomError> {
        Self::report_commit_applying(self, intent_id, request).await
    }

    async fn report_commit_outcome(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitOutcomeRequest>,
    ) -> Result<CommitReceiptOutcome, VoomError> {
        Self::report_commit_outcome(self, intent_id, request).await
    }

    async fn complete_commit_intent(
        &self,
        intent_id: ArtifactCommitIntentId,
        request: &RetryRequest<CommitCompleteRequest>,
    ) -> Result<CommitCompleteOutcome, VoomError> {
        Self::complete_commit_intent(self, intent_id, request).await
    }
}

fn validate_activated_workers(
    configured: &[WorkerConfig],
    activated: &[ActivatedWorker],
) -> Result<(), VoomError> {
    if configured.len() != activated.len() {
        return Err(VoomError::ExternalSystemUnavailable(format!(
            "activation returned {} workers for {} declarations",
            activated.len(),
            configured.len()
        )));
    }
    let mut names = activated
        .iter()
        .map(|worker| worker.logical_name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    let mut expected = configured
        .iter()
        .map(|worker| worker.name.as_str())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if names != expected {
        return Err(VoomError::ExternalSystemUnavailable(
            "activation worker names do not match the declarations".to_owned(),
        ));
    }
    Ok(())
}

/// Reap finished lease tasks. A cancelled task still releases its permit on drop, so a
/// join error needs no handling beyond being collected here.
fn drain_finished_leases(leases: &mut JoinSet<()>) {
    while leases.try_join_next().is_some() {}
}

async fn wait_for_leases(leases: &mut JoinSet<()>) {
    while leases.join_next().await.is_some() {}
}

async fn cancel_and_wait(
    cancellation: &watch::Sender<LeaseCancellation>,
    reason: LeaseCancellation,
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
) -> Option<(ShutdownKind, Option<ShutdownForce>)> {
    let _ = cancellation.send(reason);
    // Deliberately unarmed: this is the one wait that runs in steady state.
    wait_or_force(leases, shutdown, true, None).await
}

/// Settle a crashed child's leases, returning the settlement once a shutdown is seen.
///
/// Takes `budget` as a `Duration`, not an `Instant`, and stamps the deadline **after**
/// the first wait. That first wait is `cancel_and_wait`, which is deliberately
/// unbounded and can run for the client's whole retry budget; a deadline computed by
/// the caller would reach the second wait already spent, so that wait's budget would be
/// zero rather than `budget`.
async fn settle_leases_after_child_crash(
    cancellation: &watch::Sender<LeaseCancellation>,
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    budget: Duration,
) -> Option<LeaseSettlement> {
    let observed = cancel_and_wait(cancellation, LeaseCancellation::Crash, leases, shutdown).await;
    let (observed, observed_force) = observed?;
    // Reached only with a shutdown already in flight, so this second wait is a
    // shutdown-tail wait and takes the budget.
    let deadline = Some(Instant::now() + budget);
    // Finish settling with the force escape, but do not re-send a cancellation: these
    // leases died with the child, and overwriting `Crash` would record a crashed worker's
    // ticket as user-cancelled.
    let final_observed = wait_or_force(leases, shutdown, false, deadline).await;
    Some(child_crash_lease_settlement(
        observed,
        observed_force,
        final_observed,
    ))
}

fn child_crash_lease_settlement(
    observed: ShutdownKind,
    observed_force: Option<ShutdownForce>,
    final_observed: Option<(ShutdownKind, Option<ShutdownForce>)>,
) -> LeaseSettlement {
    if observed == ShutdownKind::Forced {
        return LeaseSettlement::Forced(observed_force.unwrap_or(ShutdownForce::Signal));
    }
    match final_observed {
        Some((ShutdownKind::Forced, cause)) => {
            LeaseSettlement::Forced(cause.unwrap_or(ShutdownForce::Signal))
        }
        _ => LeaseSettlement::Completed,
    }
}

/// Wait for every lease task to settle, abandoning settlement when a forced shutdown
/// arrives. Settlement calls the control plane, which retries an unreachable peer, so
/// every wait needs this escape.
///
/// Returns the shutdown observed while waiting, if any. Waiting here consumes the watch
/// notification, so the caller — not a later `changed()` — owns acting on it.
///
/// `report_any` separates the two callers. Shutdown settlement already knows its kind and
/// only escalates on `Forced`, so it ignores anything else and keeps waiting. Crash
/// settlement has observed no shutdown at all, so it must report the first one and let the
/// caller run the real settlement rather than restarting the child.
///
/// `deadline` bounds the wait. Only the two callers that run inside a shutdown supply
/// one; `cancel_and_wait`'s steady-state call passes `None`, because arming it would
/// terminate a healthy running agent after one slow worker-crash settlement. Expiry
/// does locally what observing [`ShutdownKind::Forced`] does, and deliberately does
/// **not** publish on the watch: a coordinator that is not overrunning is not forced.
///
/// The second tuple element is itself optional: with `report_any` a kind is reported
/// without anything having been forced, and there is no cause to name for that.
async fn wait_or_force(
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    report_any: bool,
    deadline: Option<Instant>,
) -> Option<(ShutdownKind, Option<ShutdownForce>)> {
    let observed = {
        let settlement = wait_for_leases(leases);
        tokio::pin!(settlement);
        loop {
            tokio::select! {
                () = &mut settlement => break None,
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        // No deadline: never ready, so this arm never fires.
                        None => std::future::pending().await,
                    }
                } => break Some((ShutdownKind::Forced, Some(ShutdownForce::Deadline))),
                changed = shutdown.changed() => {
                    // A closed channel means the runtime is gone: treat it as forced rather
                    // than looping, since `changed()` then returns Err on every poll.
                    let Ok(()) = changed else {
                        break Some((ShutdownKind::Forced, Some(ShutdownForce::Signal)));
                    };
                    let kind = *shutdown.borrow();
                    if kind == ShutdownKind::Forced {
                        break Some((kind, Some(ShutdownForce::Signal)));
                    }
                    if report_any && kind != ShutdownKind::Running {
                        break Some((kind, None));
                    }
                }
            }
        }
    };
    if matches!(observed, Some((ShutdownKind::Forced, _))) {
        leases.abort_all();
        wait_for_leases(leases).await;
    }
    observed
}

/// Settle this coordinator's leases for a shutdown, within `deadline`.
///
/// This is the only hop that carries a [`ShutdownForce::Deadline`] out of a
/// coordinator. Collapsing either arm below to `Signal` would compile, pass, and make
/// the whole bound a no-op: [`ShutdownProgress::forced`] could then never hold
/// `Deadline`, and `finish_shutdown_lifecycle` would skip the deactivation the bound
/// exists to protect.
async fn settle_leases_for_shutdown(
    cancellation: &watch::Sender<LeaseCancellation>,
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    kind: ShutdownKind,
    deadline: Option<Instant>,
) -> LeaseSettlement {
    let reason = if kind == ShutdownKind::User {
        LeaseCancellation::User
    } else {
        LeaseCancellation::Fenced
    };
    let _ = cancellation.send(reason);
    if kind == ShutdownKind::Forced {
        leases.abort_all();
        wait_for_leases(leases).await;
        // A published Forced kind only ever comes from wait_for_coordinators' signal arm.
        return LeaseSettlement::Forced(ShutdownForce::Signal);
    }
    // Settlement calls the control plane on every path, not just the user-requested one, so
    // a fenced or restart-exhausted shutdown needs the same force escape. Without it a lease
    // stuck retrying an unreachable control plane hangs the coordinator with no way out.
    match wait_or_force(leases, shutdown, false, deadline).await {
        Some((ShutdownKind::Forced, cause)) => {
            LeaseSettlement::Forced(cause.unwrap_or(ShutdownForce::Signal))
        }
        _ => LeaseSettlement::Completed,
    }
}

/// Wait for coordinators to finish, letting a termination signal force settlement.
///
/// Forcing is available on every exit, not just the user-requested one. A fenced or
/// restart-exhausted exit settles against the same control plane, so gating the escape on a
/// graceful exit left an unreachable control plane hanging the process with no recovery.
async fn wait_for_coordinators(
    coordinators: &mut JoinSet<CoordinatorExit>,
    shutdown: &watch::Sender<ShutdownKind>,
    signals: &mut mpsc::UnboundedReceiver<()>,
    mut signal_phase: ShutdownSignalPhase,
) -> Result<ShutdownProgress, VoomError> {
    let mut forced: Option<ShutdownForce> = None;
    let mut signals_open = true;
    while !coordinators.is_empty() {
        tokio::select! {
            joined = coordinators.join_next() => {
                match joined {
                    Some(Ok(CoordinatorExit::Shutdown(LeaseSettlement::Forced(cause)))) => {
                        forced.get_or_insert(cause);
                    }
                    Some(Ok(
                        CoordinatorExit::Shutdown(LeaseSettlement::Completed)
                        | CoordinatorExit::RestartExhausted
                        | CoordinatorExit::Fatal(_),
                    ))
                    | None => {}
                    Some(Err(error)) => {
                        return Err(VoomError::Internal(format!(
                            "join node-agent worker coordinator: {error}"
                        )));
                    }
                }
            }
            signal = signals.recv(), if signals_open && forced.is_none() => {
                let Some(()) = signal else {
                    signals_open = false;
                    continue;
                };
                if signal_phase.signal_forces() {
                    forced = Some(ShutdownForce::Signal);
                    let _ = shutdown.send(ShutdownKind::Forced);
                }
            }
        }
    }
    Ok(ShutdownProgress {
        signal_phase,
        forced,
    })
}

fn shutdown_kind_for_exit(exit: &RuntimeExit) -> ShutdownKind {
    match exit {
        RuntimeExit::Graceful => ShutdownKind::User,
        RuntimeExit::Fatal(_) => ShutdownKind::Fenced,
        RuntimeExit::RestartExhausted => ShutdownKind::RestartExhausted,
    }
}

fn signal_phase_for_exit(exit: &RuntimeExit) -> ShutdownSignalPhase {
    match exit {
        RuntimeExit::Graceful => ShutdownSignalPhase::ForceEnabled,
        RuntimeExit::Fatal(_) | RuntimeExit::RestartExhausted => ShutdownSignalPhase::AwaitingFirst,
    }
}

fn coordinator_exit(
    joined: Option<Result<CoordinatorExit, tokio::task::JoinError>>,
) -> RuntimeExit {
    match joined {
        Some(Ok(CoordinatorExit::RestartExhausted)) => RuntimeExit::RestartExhausted,
        Some(Ok(CoordinatorExit::Fatal(fatal))) => RuntimeExit::Fatal(fatal),
        Some(Ok(CoordinatorExit::Shutdown(_))) | None => RuntimeExit::Fatal(
            RuntimeFatal::Internal("worker coordinator stopped before shutdown".to_owned()),
        ),
        Some(Err(error)) => RuntimeExit::Fatal(RuntimeFatal::Internal(format!(
            "worker coordinator task failed: {error}"
        ))),
    }
}

#[derive(Debug)]
enum RuntimeExit {
    Graceful,
    Fatal(RuntimeFatal),
    RestartExhausted,
}

impl RuntimeExit {
    fn into_error(self) -> VoomError {
        match self {
            Self::Fatal(RuntimeFatal::ControlPlane(error)) => error,
            Self::Fatal(RuntimeFatal::Internal(message)) => VoomError::Internal(message),
            Self::Graceful | Self::RestartExhausted => {
                VoomError::Internal("invalid runtime exit conversion".to_owned())
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum RuntimeFatal {
    ControlPlane(VoomError),
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownSignalPhase {
    AwaitingFirst,
    ForceEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseSettlement {
    Completed,
    Forced(ShutdownForce),
}

/// What abandoned a shutdown wait.
///
/// Reporting a deadline expiry as "interrupted by a termination signal" would send
/// an operator looking for a signal nobody sent, so the two travel separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownForce {
    Signal,
    Deadline,
}

impl ShutdownSignalPhase {
    fn signal_forces(&mut self) -> bool {
        match self {
            Self::AwaitingFirst => {
                *self = Self::ForceEnabled;
                false
            }
            Self::ForceEnabled => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShutdownProgress {
    signal_phase: ShutdownSignalPhase,
    forced: Option<ShutdownForce>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownKind {
    Running,
    User,
    Forced,
    Fenced,
    RestartExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseCancellation {
    Running,
    Crash,
    User,
    Fenced,
}

impl CoordinatorContext {
    fn worker_id(&self) -> WorkerId {
        self.worker_id
    }
}
/// Index the configured storage-root provider locators by root id, rejecting
/// duplicate bindings at spawn time.
pub(crate) fn storage_root_bindings(
    config: &AgentConfig,
) -> Result<HashMap<u64, PathBuf>, VoomError> {
    let mut bindings = HashMap::with_capacity(config.storage_roots.len());
    for root in &config.storage_roots {
        if bindings
            .insert(root.storage_root_id, root.provider_locator.clone())
            .is_some()
        {
            return Err(VoomError::Config(format!(
                "storage root {} is bound more than once",
                root.storage_root_id
            )));
        }
    }
    Ok(bindings)
}

pub(crate) fn classify_control_plane_error(error: VoomError) -> RuntimeFatal {
    RuntimeFatal::ControlPlane(error)
}

fn node_heartbeat_expiry() -> RuntimeFatal {
    RuntimeFatal::ControlPlane(VoomError::ExternalSystemUnavailable(
        "node heartbeat did not reach the control plane within the incarnation ttl".to_owned(),
    ))
}

fn heartbeat_interval(ttl_seconds: u32) -> Duration {
    Duration::from_secs(u64::from((ttl_seconds / HEARTBEAT_DIVISOR).max(1)))
}

fn schedule_rng_from<R>(source: &mut R) -> Result<StdRng, VoomError>
where
    R: TryRngCore,
{
    StdRng::try_from_rng(source).map_err(|error| {
        VoomError::Internal(format!(
            "seed node-agent schedule RNG from OS entropy: {error}"
        ))
    })
}

fn derive_schedule_rng(parent: &mut StdRng) -> StdRng {
    StdRng::from_rng(parent)
}

fn acquisition_poll_delay(interval: Duration, rng: &mut impl Rng) -> Duration {
    centered_jitter(interval, rng)
}

fn node_heartbeat_delay(interval: Duration, rng: &mut impl Rng) -> Duration {
    centered_jitter(interval, rng)
}

fn lease_heartbeat_delay(interval: Duration, ttl: Duration, rng: &mut impl Rng) -> Duration {
    let upper = interval.saturating_add(interval / 2);
    if upper < ttl {
        centered_jitter(interval, rng)
    } else {
        interval
    }
}

pub(crate) fn centered_jitter(interval: Duration, rng: &mut impl Rng) -> Duration {
    let half = interval / 2;
    let lower = half.as_nanos();
    let upper = interval.saturating_add(half).as_nanos();
    duration_from_nanos(rng.random_range(lower..=upper))
}

fn duration_from_nanos(nanos: u128) -> Duration {
    const NANOS_PER_SECOND: u128 = 1_000_000_000;
    let seconds = u64::try_from(nanos / NANOS_PER_SECOND).unwrap_or(u64::MAX);
    let subsecond = u32::try_from(nanos % NANOS_PER_SECOND).unwrap_or(999_999_999);
    Duration::new(seconds, subsecond)
}

fn is_validation_fresh(elapsed: Duration, ttl: Duration) -> bool {
    elapsed < ttl / 2
}

fn granted_lease_ttl(lease_ttl_seconds: i64) -> Duration {
    Duration::from_secs(u64::try_from(lease_ttl_seconds).unwrap_or(1).max(1))
}

fn duration_seconds_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

pub(crate) fn duration_millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

pub(crate) fn new_key(prefix: &str) -> String {
    format!("{prefix}-{}", random_hex(16))
}

fn random_hex(byte_count: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = vec![0_u8; byte_count];
    StdRng::from_os_rng().fill_bytes(&mut bytes);
    let mut encoded = String::with_capacity(byte_count * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Run the shutdown tail, giving up after `bound`.
///
/// The inner budgets bound the waits this design enumerated; this bounds the ones it
/// did not. Dropping `tail` drops the coordinator [`JoinSet`], aborting its tasks — a
/// child mid-reap is still `SIGKILL`ed, by `.kill_on_drop(true)` in `launch`, not by
/// `RunningChild`'s `Drop`, which early-returns once `shutdown` has moved the handle
/// out. Sized above the sum of the inner bounds, so each of those expires first and an
/// ordinary overrun is still attributed to the wait that caused it.
async fn run_shutdown_tail_within<F>(bound: Duration, tail: F) -> Result<(), VoomError>
where
    F: Future<Output = Result<(), VoomError>>,
{
    match tokio::time::timeout(bound, tail).await {
        Ok(result) => result,
        Err(_) => Err(shutdown_backstop_error(bound)),
    }
}

/// The whole shutdown tail overran its published bound.
///
/// Deliberately distinct from [`shutdown_deadline_error`]: that one is an ordinary,
/// documented abandon at a named budget, and this one means a wait nobody bounded ran
/// past every inner budget — a defect, and the operator's only channel says so.
fn shutdown_backstop_error(bound: Duration) -> VoomError {
    VoomError::ExternalSystemUnavailable(format!(
        "node-agent shutdown exceeded its {bound:?} tail bound; a wait outside the \
         shutdown budgets did not complete — please report this"
    ))
}

/// A shutdown-tail control-plane wait was abandoned at its own budget.
///
/// Distinct from [`forced_shutdown_error`] because no signal arrived: an operator
/// told to look for one would be looking for something nobody sent.
fn shutdown_deadline_error() -> VoomError {
    VoomError::ExternalSystemUnavailable(
        "node-agent shutdown abandoned a control-plane call at its shutdown budget; \
         the control plane did not answer"
            .to_owned(),
    )
}

fn forced_shutdown_error() -> VoomError {
    VoomError::ExternalSystemUnavailable(
        "node-agent shutdown interrupted by a termination signal".to_owned(),
    )
}

async fn forward_shutdowns<F, S>(first: F, second: S, signals: mpsc::UnboundedSender<()>)
where
    F: Future<Output = ()>,
    S: Future<Output = ()>,
{
    first.await;
    if signals.send(()).is_err() {
        return;
    }
    second.await;
    let _ = signals.send(());
}

#[cfg(unix)]
async fn forward_os_shutdowns(signals: mpsc::UnboundedSender<()>) {
    let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
    let Ok(mut terminate) = terminate else {
        forward_ctrl_c(signals).await;
        return;
    };
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if result.is_err() || signals.send(()).is_err() {
                    return;
                }
            }
            signal = terminate.recv() => {
                if signal.is_none() || signals.send(()).is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(not(unix))]
async fn forward_os_shutdowns(signals: mpsc::UnboundedSender<()>) {
    forward_ctrl_c(signals).await;
}

async fn forward_ctrl_c(signals: mpsc::UnboundedSender<()>) {
    loop {
        if tokio::signal::ctrl_c().await.is_err() || signals.send(()).is_err() {
            return;
        }
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;
