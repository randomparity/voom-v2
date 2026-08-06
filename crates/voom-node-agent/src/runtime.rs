use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rand::rngs::StdRng;
use rand::{RngCore, SeedableRng};
use secrecy::SecretString;
use serde_json::{Value as JsonValue, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use voom_core::{
    FailureClass, LeaseId, NodeIncarnationEndReason, NodeIncarnationId, OperationKind, VoomError,
    WorkerId,
};
use voom_worker_protocol::{
    ClientHandle, NdjsonOutcome, OperationRequest, ProgressFrame, WorkerCredentials,
};

use crate::child::{ChildErrorKind, ChildSpec, ChildSupervisor, RunningChild};
use crate::client::{
    AcquireOutcome, AcquireRequest, ActivateOutcome, ActivateRequest, ActivatedWorker,
    CompleteOutcome, CompleteRequest, ControlPlaneClient, DeactivateOutcome, DeactivateRequest,
    FailOutcome, FailRequest, LeaseDispatch, LeaseHeartbeatOutcome, LeaseHeartbeatRequest,
    NodeHeartbeatOutcome, NodeHeartbeatRequest, RetryRequest, WorkerDeclaration,
};
use crate::config::{LoadedAgentConfig, WorkerConfig};

const HEARTBEAT_DIVISOR: u32 = 3;
const RESTART_DELAY: Duration = Duration::from_millis(250);

/// Owns one node incarnation from activation through ordered deactivation.
#[derive(Debug)]
pub struct AgentRuntime {
    config: LoadedAgentConfig,
    client: Arc<dyn ControlPlaneApi>,
}

impl AgentRuntime {
    /// Build an agent runtime from validated configuration.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the control-plane client cannot be built.
    pub fn new(config: LoadedAgentConfig) -> Result<Self, VoomError> {
        let client = ControlPlaneClient::from_config(&config)?;
        Ok(Self {
            config,
            client: Arc::new(client),
        })
    }

    #[cfg(test)]
    fn with_client(config: LoadedAgentConfig, client: Arc<dyn ControlPlaneApi>) -> Self {
        Self { config, client }
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
        mut signals: mpsc::UnboundedReceiver<()>,
    ) -> Result<(), VoomError> {
        let incarnation_id = NodeIncarnationId::generate()?;
        let activation = self.activate(incarnation_id).await?;
        let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
        let node_heartbeat = self
            .start_node_heartbeat(
                incarnation_id,
                activation.heartbeat_ttl_seconds,
                fatal_tx.clone(),
            )
            .await?;

        let specs = self.child_specs(&activation)?;
        let startup_supervisor = ChildSupervisor::new(self.shutdown_grace());
        let children = match startup_supervisor.start_all(specs).await {
            Ok(children) => children,
            Err(error) => {
                node_heartbeat.stop();
                self.deactivate(incarnation_id, NodeIncarnationEndReason::ChildStartupFailed)
                    .await?;
                return Err(VoomError::ExternalSystemUnavailable(format!(
                    "start node-agent children: {error}"
                )));
            }
        };

        let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
        let mut coordinators =
            self.spawn_coordinators(children, incarnation_id, &shutdown_rx, &fatal_tx)?;
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
        let _ = shutdown_tx.send(shutdown_kind);
        let forced = wait_for_coordinators(
            &mut coordinators,
            &shutdown_tx,
            &mut signals,
            matches!(&exit, RuntimeExit::Graceful),
        )
        .await?;
        node_heartbeat.stop();

        let reason = match &exit {
            RuntimeExit::Graceful => NodeIncarnationEndReason::GracefulShutdown,
            RuntimeExit::Fatal(_) => return Err(exit.into_error()),
            RuntimeExit::RestartExhausted => NodeIncarnationEndReason::ChildRestartExhausted,
        };
        if forced {
            return Err(forced_shutdown_error());
        }
        if reason == NodeIncarnationEndReason::GracefulShutdown {
            self.deactivate_or_second_signal(incarnation_id, &mut signals)
                .await?;
        } else {
            self.deactivate(incarnation_id, reason).await?;
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

    async fn start_node_heartbeat(
        &self,
        incarnation_id: NodeIncarnationId,
        ttl_seconds: u32,
        fatal_tx: mpsc::UnboundedSender<RuntimeFatal>,
    ) -> Result<NodeHeartbeatHandle, VoomError> {
        send_node_heartbeat(
            self.client.as_ref(),
            self.config.config.node_id,
            incarnation_id,
        )
        .await?;
        let interval = heartbeat_interval(ttl_seconds);
        let client = Arc::clone(&self.client);
        let node_id = self.config.config.node_id;
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let joined = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(interval) => {
                        if let Err(error) = send_node_heartbeat(client.as_ref(), node_id, incarnation_id).await {
                            let _ = fatal_tx.send(RuntimeFatal::ControlPlane(error));
                            return;
                        }
                    }
                }
            }
        });
        Ok(NodeHeartbeatHandle { stop_tx, joined })
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
    ) -> Result<JoinSet<CoordinatorExit>, VoomError> {
        let workers = self
            .config
            .config
            .workers
            .iter()
            .map(|worker| (worker.name.as_str(), worker))
            .collect::<HashMap<_, _>>();
        let mut coordinators = JoinSet::new();
        for child in children {
            let worker = workers.get(child.spec().logical_name()).ok_or_else(|| {
                VoomError::Internal("started child has no configuration".to_owned())
            })?;
            let context = CoordinatorContext {
                client: Arc::clone(&self.client),
                node_id: self.config.config.node_id,
                incarnation_id,
                worker_id: child.spec().credentials().worker_id,
                lease_ttl: Duration::from_secs(u64::from(self.config.config.lease_ttl_seconds)),
                progress_timeout: Duration::from_secs(u64::from(
                    self.config.config.progress_idle_timeout_seconds,
                )),
                poll_interval: Duration::from_millis(self.config.config.poll_interval_ms),
                shutdown_grace: self.shutdown_grace(),
                worker: (*worker).clone(),
                fatal_tx: fatal_tx.clone(),
            };
            coordinators.spawn(run_coordinator(child, context, shutdown.clone()));
        }
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

    async fn deactivate_or_second_signal(
        &self,
        incarnation_id: NodeIncarnationId,
        signals: &mut mpsc::UnboundedReceiver<()>,
    ) -> Result<(), VoomError> {
        tokio::select! {
            result = self.deactivate(
                incarnation_id,
                NodeIncarnationEndReason::GracefulShutdown,
            ) => result,
            _ = signals.recv() => Err(forced_shutdown_error()),
        }
    }

    fn shutdown_grace(&self) -> Duration {
        Duration::from_secs(u64::from(self.config.config.shutdown_grace_seconds))
    }
}

struct NodeHeartbeatHandle {
    stop_tx: watch::Sender<bool>,
    joined: tokio::task::JoinHandle<()>,
}

impl NodeHeartbeatHandle {
    fn stop(self) {
        let _ = self.stop_tx.send(true);
        self.joined.abort();
    }
}

#[derive(Clone)]
struct CoordinatorContext {
    client: Arc<dyn ControlPlaneApi>,
    node_id: voom_core::NodeId,
    incarnation_id: NodeIncarnationId,
    worker_id: WorkerId,
    lease_ttl: Duration,
    progress_timeout: Duration,
    poll_interval: Duration,
    shutdown_grace: Duration,
    worker: WorkerConfig,
    fatal_tx: mpsc::UnboundedSender<RuntimeFatal>,
}

#[derive(Debug)]
enum CoordinatorExit {
    Shutdown,
    RestartExhausted,
    Fatal(RuntimeFatal),
}

async fn run_coordinator(
    mut child: RunningChild,
    context: CoordinatorContext,
    mut shutdown: watch::Receiver<ShutdownKind>,
) -> CoordinatorExit {
    let permits = Arc::new(Semaphore::new(context.worker.max_parallel as usize));
    let mut leases = JoinSet::new();
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    loop {
        drain_finished_leases(&mut leases);
        let acquire = acquire_one(&context, Arc::clone(&permits));
        tokio::pin!(acquire);
        let event = tokio::select! {
            changed = shutdown.changed() => {
                let kind = if changed.is_err() { ShutdownKind::Fenced } else { *shutdown.borrow() };
                CoordinatorEvent::Shutdown(kind)
            }
            status = child.wait_for_exit() => CoordinatorEvent::ChildExit(status.map(|_| ())),
            result = &mut acquire => CoordinatorEvent::Acquire(result),
        };
        match event {
            CoordinatorEvent::Acquire(Ok(Acquired::Idle)) => {
                tokio::time::sleep(context.poll_interval).await;
            }
            CoordinatorEvent::Acquire(Ok(Acquired::Lease(lease))) => {
                let lease_context = context.clone();
                let client = child.client();
                let credentials = child.spec().credentials().clone();
                leases.spawn(run_lease(
                    *lease,
                    client,
                    credentials,
                    lease_context,
                    cancel_rx.clone(),
                    shutdown.clone(),
                ));
            }
            CoordinatorEvent::Acquire(Ok(Acquired::Terminal)) => {}
            CoordinatorEvent::Acquire(Err(error)) => return CoordinatorExit::Fatal(error),
            CoordinatorEvent::Shutdown(kind) => {
                settle_leases_for_shutdown(&cancel_tx, &mut leases, &mut shutdown, kind).await;
                let supervisor = ChildSupervisor::new(context.shutdown_grace);
                let _ = supervisor.shutdown_all(vec![child]).await;
                return CoordinatorExit::Shutdown;
            }
            CoordinatorEvent::ChildExit(Ok(())) => {
                cancel_and_wait(&cancel_tx, LeaseCancellation::Crash, &mut leases).await;
                match restart_child(&context, child.spec().clone()).await {
                    Ok(restarted) => {
                        child = restarted;
                        let _ = cancel_tx.send(LeaseCancellation::Running);
                    }
                    Err(ChildErrorKind::RestartExhausted) => {
                        return CoordinatorExit::RestartExhausted;
                    }
                    Err(_) => return CoordinatorExit::RestartExhausted,
                }
            }
            CoordinatorEvent::ChildExit(Err(error)) => {
                return CoordinatorExit::Fatal(RuntimeFatal::Internal(error.to_string()));
            }
        }
    }
}

async fn restart_child(
    context: &CoordinatorContext,
    spec: ChildSpec,
) -> Result<RunningChild, ChildErrorKind> {
    let mut supervisor = ChildSupervisor::new(context.shutdown_grace);
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
    Shutdown(ShutdownKind),
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
    let freshness = context.lease_ttl / 2;
    loop {
        let started = Instant::now();
        let request = lease_heartbeat_request(context, new_key("validate"))
            .map_err(RuntimeFatal::ControlPlane)?;
        let result = tokio::time::timeout(
            freshness,
            context.client.lease_heartbeat(dispatch.lease_id, &request),
        )
        .await;
        match result {
            Ok(Ok(_)) if is_validation_fresh(started.elapsed(), context.lease_ttl) => {
                return Ok(ValidationOutcome::Fresh);
            }
            Ok(Ok(_)) | Err(_) => {}
            Ok(Err(VoomError::Conflict(_) | VoomError::NotFound(_))) => {
                return Ok(ValidationOutcome::Terminal);
            }
            Ok(Err(error)) => return Err(classify_control_plane_error(error)),
        }
    }
}

async fn run_lease(
    lease: HeldLeaseGuard,
    child: Arc<dyn ClientHandle>,
    credentials: WorkerCredentials,
    context: CoordinatorContext,
    mut worker_cancel: watch::Receiver<LeaseCancellation>,
    mut shutdown: watch::Receiver<ShutdownKind>,
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
            heartbeat_stop_rx,
            lease_fence_tx,
        )
        .await;
    });

    let outcome = tokio::select! {
        outcome = dispatch_to_child(&lease.dispatch, child.as_ref(), &credentials, &context) => outcome,
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
    mut stop: watch::Receiver<bool>,
    fence: mpsc::Sender<()>,
) {
    let seconds = u64::try_from(heartbeat_after_seconds).unwrap_or(1).max(1);
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_secs(seconds)) => {
                let Ok(request) = lease_heartbeat_request(&context, new_key("lease-heartbeat")) else {
                    let _ = context.fatal_tx.send(RuntimeFatal::Internal(
                        "build lease heartbeat request".to_owned(),
                    ));
                    return;
                };
                match context.client.lease_heartbeat(lease_id, &request).await {
                    Ok(_) => {}
                    Err(VoomError::Conflict(_) | VoomError::NotFound(_)) => {
                        let _ = fence.send(()).await;
                        return;
                    }
                    Err(error) => {
                        let _ = context.fatal_tx.send(classify_control_plane_error(error));
                        return;
                    }
                }
            }
        }
    }
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

async fn open_worker_stream(
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

async fn consume_progress_stream(
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

enum LeaseOutcome {
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
) -> Result<RetryRequest<LeaseHeartbeatRequest>, VoomError> {
    RetryRequest::new(
        key,
        &LeaseHeartbeatRequest {
            node_id: context.node_id,
            worker_id: context.worker_id(),
            incarnation_id: context.incarnation_id,
            lease_ttl_seconds: duration_seconds_i64(context.lease_ttl),
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

#[async_trait]
trait ControlPlaneApi: std::fmt::Debug + Send + Sync {
    async fn activate(
        &self,
        node_id: voom_core::NodeId,
        request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError>;

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

fn drain_finished_leases(leases: &mut JoinSet<()>) {
    while let Some(result) = leases.try_join_next() {
        if result.is_err() {
            // A cancelled lease task releases its permit; the coordinator remains responsible
            // for the child and subsequent work.
        }
    }
}

async fn wait_for_leases(leases: &mut JoinSet<()>) {
    while leases.join_next().await.is_some() {}
}

async fn cancel_and_wait(
    cancellation: &watch::Sender<LeaseCancellation>,
    reason: LeaseCancellation,
    leases: &mut JoinSet<()>,
) {
    let _ = cancellation.send(reason);
    wait_for_leases(leases).await;
}

async fn settle_leases_for_shutdown(
    cancellation: &watch::Sender<LeaseCancellation>,
    leases: &mut JoinSet<()>,
    shutdown: &mut watch::Receiver<ShutdownKind>,
    kind: ShutdownKind,
) -> bool {
    let reason = if kind == ShutdownKind::User {
        LeaseCancellation::User
    } else {
        LeaseCancellation::Fenced
    };
    let _ = cancellation.send(reason);
    if kind != ShutdownKind::User {
        wait_for_leases(leases).await;
        return false;
    }

    let forced = {
        let settlement = wait_for_leases(leases);
        tokio::pin!(settlement);
        loop {
            tokio::select! {
                () = &mut settlement => break false,
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() == ShutdownKind::Forced {
                        break true;
                    }
                }
            }
        }
    };
    if forced {
        leases.abort_all();
        wait_for_leases(leases).await;
    }
    forced
}

async fn wait_for_coordinators(
    coordinators: &mut JoinSet<CoordinatorExit>,
    shutdown: &watch::Sender<ShutdownKind>,
    signals: &mut mpsc::UnboundedReceiver<()>,
    allow_force: bool,
) -> Result<bool, VoomError> {
    let mut forced = false;
    let mut force_enabled = allow_force;
    while !coordinators.is_empty() {
        tokio::select! {
            joined = coordinators.join_next() => {
                if let Some(Err(error)) = joined {
                    return Err(VoomError::Internal(format!(
                        "join node-agent worker coordinator: {error}"
                    )));
                }
            }
            signal = signals.recv(), if force_enabled && !forced => {
                if signal.is_none() {
                    force_enabled = false;
                    continue;
                }
                forced = true;
                let _ = shutdown.send(ShutdownKind::Forced);
            }
        }
    }
    Ok(forced)
}

fn shutdown_kind_for_exit(exit: &RuntimeExit) -> ShutdownKind {
    match exit {
        RuntimeExit::Graceful => ShutdownKind::User,
        RuntimeExit::Fatal(_) => ShutdownKind::Fenced,
        RuntimeExit::RestartExhausted => ShutdownKind::RestartExhausted,
    }
}

fn coordinator_exit(
    joined: Option<Result<CoordinatorExit, tokio::task::JoinError>>,
) -> RuntimeExit {
    match joined {
        Some(Ok(CoordinatorExit::RestartExhausted)) => RuntimeExit::RestartExhausted,
        Some(Ok(CoordinatorExit::Fatal(fatal))) => RuntimeExit::Fatal(fatal),
        Some(Ok(CoordinatorExit::Shutdown)) | None => RuntimeExit::Fatal(RuntimeFatal::Internal(
            "worker coordinator stopped before shutdown".to_owned(),
        )),
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
enum RuntimeFatal {
    ControlPlane(VoomError),
    Internal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownKind {
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

fn classify_control_plane_error(error: VoomError) -> RuntimeFatal {
    RuntimeFatal::ControlPlane(error)
}

fn heartbeat_interval(ttl_seconds: u32) -> Duration {
    Duration::from_secs(u64::from((ttl_seconds / HEARTBEAT_DIVISOR).max(1)))
}

fn is_validation_fresh(elapsed: Duration, ttl: Duration) -> bool {
    elapsed < ttl / 2
}

fn duration_seconds_i64(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn duration_millis_u32(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn new_key(prefix: &str) -> String {
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

fn forced_shutdown_error() -> VoomError {
    VoomError::ExternalSystemUnavailable(
        "node-agent shutdown interrupted by a second termination signal".to_owned(),
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
