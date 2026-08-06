use std::collections::VecDeque;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::sync::{Mutex, Notify};
use voom_core::{
    ArtifactAccessMode, LeaseId, NodeId, NodeIncarnationStatus, OperationKind, TicketId, WorkerId,
};
use voom_worker_protocol::{
    DispatchStream, HandshakeResponse, NdjsonReader, OperationResponse, ProtocolError,
    WorkerIdentityResponse,
};

use super::*;
use crate::client::{
    AcquireIdle, ArtifactAccessPlan, CompleteOutcome, DeactivateOutcome, FailOutcome,
    LeaseHeartbeatOutcome, NodeHeartbeatOutcome,
};
use crate::config::{AgentConfig, TokenSource};

#[test]
fn validation_accepts_only_responses_before_half_ttl() {
    let ttl = Duration::from_secs(30);
    assert!(is_validation_fresh(Duration::from_millis(14_999), ttl));
    assert!(!is_validation_fresh(Duration::from_secs(15), ttl));
    assert!(!is_validation_fresh(Duration::from_secs(16), ttl));
}

#[tokio::test(start_paused = true)]
async fn node_heartbeat_continues_during_delayed_child_startup() {
    let control = Arc::new(FakeControlPlane::default());
    let runtime = AgentRuntime::with_client(loaded_config(), control.clone());
    let (fatal_tx, mut fatal_rx) = mpsc::unbounded_channel();
    let heartbeat = runtime
        .start_node_heartbeat(incarnation(), 6, fatal_tx)
        .await
        .unwrap();

    let delayed_startup = tokio::spawn(tokio::time::sleep(Duration::from_secs(20)));
    advance_seconds(7).await;

    assert!(!delayed_startup.is_finished());
    assert!(control.node_heartbeats.load(Ordering::SeqCst) >= 4);
    assert!(fatal_rx.try_recv().is_err());
    delayed_startup.abort();
    heartbeat.stop();
}

#[tokio::test(start_paused = true)]
async fn silent_dispatch_keeps_lease_heartbeat_until_progress_timeout_is_settled() {
    let control = Arc::new(FakeControlPlane::default());
    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(1));
    let held = held_lease(Arc::clone(&permits)).await;
    let (_worker_cancel_tx, worker_cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held,
        worker,
        credentials(),
        context(control.clone()),
        worker_cancel_rx,
        shutdown_rx,
    ));

    tokio::time::advance(Duration::from_secs(6)).await;
    task.await.unwrap();

    assert!(control.lease_heartbeats.load(Ordering::SeqCst) >= 4);
    let failures = control.failures.lock().await;
    assert_eq!(failures.as_slice(), &[FailureClass::ProgressTimeout]);
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test(start_paused = true)]
async fn terminal_retry_retains_heartbeat_and_parallelism_permit() {
    let control = Arc::new(FakeControlPlane::default());
    let gate = Arc::new(Notify::new());
    *control.fail_gate.lock().await = Some(Arc::clone(&gate));
    let worker = Arc::new(FakeWorker::new(WorkerMode::Error));
    let permits = Arc::new(Semaphore::new(1));
    let held = held_lease(Arc::clone(&permits)).await;
    let (_worker_cancel_tx, worker_cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held,
        worker,
        credentials(),
        context(control.clone()),
        worker_cancel_rx,
        shutdown_rx,
    ));

    wait_for_count(&control.fail_started, &control.fail_started_count, 1).await;
    assert!(Arc::clone(&permits).try_acquire_owned().is_err());
    advance_seconds(3).await;
    assert!(control.lease_heartbeats.load(Ordering::SeqCst) >= 2);
    assert!(Arc::clone(&permits).try_acquire_owned().is_err());

    gate.notify_waiters();
    task.await.unwrap();
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test]
async fn rejected_completion_fails_the_held_lease_as_a_malformed_result() {
    let control = Arc::new(FakeControlPlane::default());
    control.reject_complete.store(true, Ordering::SeqCst);

    settle_lease(
        &context(control.clone()),
        &dispatch(json!({})),
        LeaseOutcome::Complete(json!({"invalid": "result"})),
    )
    .await
    .unwrap();

    assert_eq!(control.complete_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::MalformedWorkerResult]
    );
}

#[tokio::test(start_paused = true)]
async fn delayed_validation_rotates_key_and_terminal_replay_never_dispatches() {
    let control = Arc::new(FakeControlPlane::default());
    control.heartbeat_actions.lock().await.extend([
        HeartbeatAction::Delay(Duration::from_secs(4)),
        HeartbeatAction::Success,
        HeartbeatAction::Conflict,
    ]);
    let context = context(control.clone());
    let dispatch = dispatch(json!({}));

    let validation = tokio::spawn({
        let context = context.clone();
        let dispatch = dispatch.clone();
        async move { validate_lease(&context, &dispatch).await }
    });
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        validation.await.unwrap().unwrap(),
        ValidationOutcome::Fresh
    ));
    let keys = control.heartbeat_keys.lock().await;
    assert_eq!(keys.len(), 2);
    assert_ne!(keys[0], keys[1]);
    drop(keys);

    *control.acquire_mode.lock().await = AcquireMode::Lease(dispatch);
    let permits = Arc::new(Semaphore::new(1));
    let worker = FakeWorker::new(WorkerMode::Silent);
    let terminal = match acquire_one(&context, permits).await.unwrap() {
        Acquired::Terminal => true,
        Acquired::Idle | Acquired::Lease(_) => false,
    };
    assert!(
        terminal,
        "expired acquire replay must not become dispatchable"
    );
    assert_eq!(worker.dispatches.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn max_parallel_reserves_before_acquire_and_never_over_acquires() {
    let control = Arc::new(FakeControlPlane::default());
    let gate = Arc::new(Notify::new());
    *control.acquire_mode.lock().await = AcquireMode::GatedIdle(Arc::clone(&gate));
    let permits = Arc::new(Semaphore::new(2));
    let context = context(control.clone());
    let mut acquisitions = JoinSet::new();
    for _ in 0..3 {
        let context = context.clone();
        let permits = Arc::clone(&permits);
        acquisitions.spawn(async move { acquire_one(&context, permits).await });
    }

    wait_for_count(&control.acquire_notify, &control.acquire_started, 2).await;
    tokio::task::yield_now().await;
    assert_eq!(control.acquire_started.load(Ordering::SeqCst), 2);
    assert_eq!(control.max_active_acquires.load(Ordering::SeqCst), 2);
    *control.acquire_mode.lock().await = AcquireMode::Idle;
    gate.notify_waiters();
    while acquisitions.join_next().await.is_some() {}
    assert_eq!(control.max_active_acquires.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn child_crash_settles_all_held_leases_before_restart_can_begin() {
    let control = Arc::new(FakeControlPlane::default());
    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(2));
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    for _ in 0..2 {
        leases.spawn(run_lease(
            held_lease(Arc::clone(&permits)).await,
            worker.clone(),
            credentials(),
            context(control.clone()),
            cancel_rx.clone(),
            shutdown_rx.clone(),
        ));
    }
    wait_for_count(&worker.dispatched, &worker.dispatches, 2).await;

    cancel_and_wait(&cancel_tx, LeaseCancellation::Crash, &mut leases).await;
    control.events.lock().await.push("restart".to_owned());

    let events = control.events.lock().await;
    assert_eq!(events.as_slice(), &["fail-ack", "fail-ack", "restart"]);
    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::WorkerCrash, FailureClass::WorkerCrash]
    );
}

#[tokio::test(start_paused = true)]
async fn incarnation_conflict_fences_inflight_child_without_terminal_mutation() {
    let control = Arc::new(FakeControlPlane::default());
    *control.acquire_mode.lock().await = AcquireMode::Conflict;
    let exit = match acquire_one(&context(control.clone()), Arc::new(Semaphore::new(1))).await {
        Err(fatal) => RuntimeExit::Fatal(fatal),
        Ok(_) => RuntimeExit::Graceful,
    };
    assert_eq!(shutdown_kind_for_exit(&exit), ShutdownKind::Fenced);

    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(1));
    let (_cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let task = tokio::spawn(run_lease(
        held_lease(Arc::clone(&permits)).await,
        worker.clone(),
        credentials(),
        context(control.clone()),
        cancel_rx,
        shutdown_rx,
    ));
    wait_for_count(&worker.dispatched, &worker.dispatches, 1).await;
    shutdown_tx.send(ShutdownKind::Fenced).unwrap();
    task.await.unwrap();
    assert!(control.failures.lock().await.is_empty());
    assert!(permits.try_acquire_owned().is_ok());
}

#[tokio::test(start_paused = true)]
async fn shutdown_orders_settlement_before_reap_and_deactivation() {
    let control = Arc::new(FakeControlPlane::default());
    let worker = Arc::new(FakeWorker::new(WorkerMode::Silent));
    let permits = Arc::new(Semaphore::new(1));
    let (cancel_tx, cancel_rx) = watch::channel(LeaseCancellation::Running);
    let (_shutdown_tx, shutdown_rx) = watch::channel(ShutdownKind::Running);
    let mut leases = JoinSet::new();
    leases.spawn(run_lease(
        held_lease(permits).await,
        worker.clone(),
        credentials(),
        context(control.clone()),
        cancel_rx,
        shutdown_rx,
    ));
    wait_for_count(&worker.dispatched, &worker.dispatches, 1).await;

    cancel_and_wait(&cancel_tx, LeaseCancellation::User, &mut leases).await;
    control.events.lock().await.push("reap".to_owned());
    let request = RetryRequest::new(
        "deactivate".to_owned(),
        &DeactivateRequest {
            incarnation_id: incarnation(),
            reason: NodeIncarnationEndReason::GracefulShutdown,
        },
    )
    .unwrap();
    control.deactivate(NodeId(7), &request).await.unwrap();

    assert_eq!(
        control.events.lock().await.as_slice(),
        &["fail-ack", "reap", "deactivate"]
    );
    assert_eq!(
        control.failures.lock().await.as_slice(),
        &[FailureClass::UserCancellation]
    );
}

#[derive(Debug, Clone)]
enum HeartbeatAction {
    Success,
    Delay(Duration),
    Conflict,
}

#[derive(Debug, Clone)]
enum AcquireMode {
    Idle,
    Lease(LeaseDispatch),
    GatedIdle(Arc<Notify>),
    Conflict,
}

#[derive(Debug)]
struct FakeControlPlane {
    node_heartbeats: AtomicUsize,
    lease_heartbeats: AtomicUsize,
    heartbeat_actions: Mutex<VecDeque<HeartbeatAction>>,
    heartbeat_keys: Mutex<Vec<String>>,
    acquire_mode: Mutex<AcquireMode>,
    acquire_started: AtomicUsize,
    active_acquires: AtomicUsize,
    max_active_acquires: AtomicUsize,
    acquire_notify: Notify,
    failures: Mutex<Vec<FailureClass>>,
    fail_gate: Mutex<Option<Arc<Notify>>>,
    fail_started: Notify,
    fail_started_count: AtomicUsize,
    complete_calls: AtomicUsize,
    reject_complete: AtomicBool,
    events: Mutex<Vec<String>>,
}

impl Default for FakeControlPlane {
    fn default() -> Self {
        Self {
            node_heartbeats: AtomicUsize::new(0),
            lease_heartbeats: AtomicUsize::new(0),
            heartbeat_actions: Mutex::new(VecDeque::new()),
            heartbeat_keys: Mutex::new(Vec::new()),
            acquire_mode: Mutex::new(AcquireMode::Idle),
            acquire_started: AtomicUsize::new(0),
            active_acquires: AtomicUsize::new(0),
            max_active_acquires: AtomicUsize::new(0),
            acquire_notify: Notify::new(),
            failures: Mutex::new(Vec::new()),
            fail_gate: Mutex::new(None),
            fail_started: Notify::new(),
            fail_started_count: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
            reject_complete: AtomicBool::new(false),
            events: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ControlPlaneApi for FakeControlPlane {
    async fn activate(
        &self,
        node_id: NodeId,
        _request: &RetryRequest<ActivateRequest>,
    ) -> Result<ActivateOutcome, VoomError> {
        Ok(ActivateOutcome {
            node_id,
            node_epoch: 1,
            incarnation_id: incarnation(),
            heartbeat_ttl_seconds: 6,
            workers: vec![ActivatedWorker {
                logical_name: "echo".to_owned(),
                worker_id: WorkerId(14),
                worker_epoch: 1,
            }],
        })
    }

    async fn deactivate(
        &self,
        node_id: NodeId,
        _request: &RetryRequest<DeactivateRequest>,
    ) -> Result<DeactivateOutcome, VoomError> {
        self.events.lock().await.push("deactivate".to_owned());
        Ok(DeactivateOutcome {
            node_id,
            node_epoch: 1,
            incarnation_id: incarnation(),
            status: NodeIncarnationStatus::Retired,
            reason: NodeIncarnationEndReason::GracefulShutdown,
            retired_worker_ids: vec![WorkerId(14)],
        })
    }

    async fn node_heartbeat(
        &self,
        node_id: NodeId,
        _request: &RetryRequest<NodeHeartbeatRequest>,
    ) -> Result<NodeHeartbeatOutcome, VoomError> {
        self.node_heartbeats.fetch_add(1, Ordering::SeqCst);
        Ok(NodeHeartbeatOutcome {
            node_id,
            status: "active".to_owned(),
        })
    }

    async fn acquire(
        &self,
        _request: &RetryRequest<AcquireRequest>,
    ) -> Result<AcquireOutcome, VoomError> {
        let mode = self.acquire_mode.lock().await.clone();
        match mode {
            AcquireMode::Idle => Ok(idle()),
            AcquireMode::Lease(dispatch) => Ok(AcquireOutcome::Leased(dispatch)),
            AcquireMode::Conflict => Err(VoomError::Conflict("incarnation fenced".to_owned())),
            AcquireMode::GatedIdle(gate) => {
                self.acquire_started.fetch_add(1, Ordering::SeqCst);
                let active = self.active_acquires.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_active_acquires.fetch_max(active, Ordering::SeqCst);
                self.acquire_notify.notify_waiters();
                gate.notified().await;
                self.active_acquires.fetch_sub(1, Ordering::SeqCst);
                Ok(idle())
            }
        }
    }

    async fn lease_heartbeat(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<LeaseHeartbeatRequest>,
    ) -> Result<LeaseHeartbeatOutcome, VoomError> {
        self.lease_heartbeats.fetch_add(1, Ordering::SeqCst);
        self.heartbeat_keys
            .lock()
            .await
            .push(request.idempotency_key().to_owned());
        let action = self
            .heartbeat_actions
            .lock()
            .await
            .pop_front()
            .unwrap_or(HeartbeatAction::Success);
        match action {
            HeartbeatAction::Success => Ok(lease_heartbeat_outcome(lease_id)),
            HeartbeatAction::Delay(delay) => {
                tokio::time::sleep(delay).await;
                Ok(lease_heartbeat_outcome(lease_id))
            }
            HeartbeatAction::Conflict => Err(VoomError::Conflict("lease expired".to_owned())),
        }
    }

    async fn complete(
        &self,
        lease_id: LeaseId,
        _request: &RetryRequest<CompleteRequest>,
    ) -> Result<CompleteOutcome, VoomError> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        if self.reject_complete.load(Ordering::SeqCst) {
            return Err(VoomError::Conflict(
                "remote complete rejected: artifact access validation missing".to_owned(),
            ));
        }
        Ok(CompleteOutcome {
            lease_id,
            ticket_id: TicketId(13),
            worker_id: WorkerId(14),
            artifact_access_plan: access_plan(),
        })
    }

    async fn fail(
        &self,
        lease_id: LeaseId,
        request: &RetryRequest<FailRequest>,
    ) -> Result<FailOutcome, VoomError> {
        let body: JsonValue = serde_json::from_slice(request.body()).unwrap();
        let class = body["class"].as_str().and_then(FailureClass::from_wire_str);
        if let Some(class) = class {
            self.failures.lock().await.push(class);
        }
        self.fail_started_count.fetch_add(1, Ordering::SeqCst);
        self.fail_started.notify_waiters();
        if let Some(gate) = self.fail_gate.lock().await.clone() {
            gate.notified().await;
        }
        self.events.lock().await.push("fail-ack".to_owned());
        Ok(FailOutcome {
            lease_id,
            ticket_id: TicketId(13),
            worker_id: WorkerId(14),
            artifact_access_plan: access_plan(),
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkerMode {
    Silent,
    Error,
}

#[derive(Debug)]
struct FakeWorker {
    mode: WorkerMode,
    dispatches: AtomicUsize,
    dispatched: Notify,
}

impl FakeWorker {
    fn new(mode: WorkerMode) -> Self {
        Self {
            mode,
            dispatches: AtomicUsize::new(0),
            dispatched: Notify::new(),
        }
    }
}

#[async_trait]
impl ClientHandle for FakeWorker {
    async fn handshake(&self, offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Ok(HandshakeResponse { agreed: offered })
    }

    async fn identity(
        &self,
        credentials: &WorkerCredentials,
    ) -> Result<WorkerIdentityResponse, ProtocolError> {
        Ok(WorkerIdentityResponse {
            worker_id: credentials.worker_id,
            worker_epoch: credentials.worker_epoch,
            protocol_version: voom_core::PROTOCOL_VERSION,
            proof: String::new(),
        })
    }

    async fn dispatch(
        &self,
        _credentials: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.dispatched.notify_waiters();
        let (mut writer, reader) = tokio::io::duplex(4096);
        match self.mode {
            WorkerMode::Silent => {
                tokio::spawn(async move {
                    std::future::pending::<()>().await;
                    drop(writer);
                });
            }
            WorkerMode::Error => {
                tokio::spawn(async move {
                    let frame = ProgressFrame::Error {
                        lease_id: request.lease_id,
                        seq: 0,
                        emitted_at: OffsetDateTime::UNIX_EPOCH,
                        class: FailureClass::WorkerCrash,
                        code: voom_core::ErrorCode::Internal,
                        message: "scripted worker error".to_owned(),
                        payload: None,
                    };
                    let mut bytes = serde_json::to_vec(&frame).unwrap();
                    bytes.push(b'\n');
                    writer.write_all(&bytes).await.unwrap();
                });
            }
        }
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = Box::pin(reader);
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id: request.lease_id,
                accepted_at: OffsetDateTime::UNIX_EPOCH,
            },
            frames: NdjsonReader::new(reader, request.lease_id),
        })
    }
}

fn loaded_config() -> LoadedAgentConfig {
    LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: "http://127.0.0.1:1".to_owned(),
            ca_cert: None,
            node_id: NodeId(7),
            poll_interval_ms: 50,
            lease_ttl_seconds: 6,
            progress_idle_timeout_seconds: 5,
            shutdown_grace_seconds: 1,
            node_token: TokenSource::Env {
                name: "VOOM_NODE_TOKEN".to_owned(),
            },
            workers: vec![worker()],
        },
        node_token: SecretString::from("node-secret"),
    }
}

fn context(control: Arc<FakeControlPlane>) -> CoordinatorContext {
    CoordinatorContext {
        client: control,
        node_id: NodeId(7),
        incarnation_id: incarnation(),
        worker_id: WorkerId(14),
        lease_ttl: Duration::from_secs(6),
        progress_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(50),
        shutdown_grace: Duration::from_secs(1),
        worker: worker(),
        fatal_tx: mpsc::unbounded_channel().0,
    }
}

fn worker() -> WorkerConfig {
    WorkerConfig {
        name: "echo".to_owned(),
        program: PathBuf::from("/bin/echo"),
        args: Vec::new(),
        operations: vec![OperationKind::ProbeFile],
        artifact_access: vec![ArtifactAccessMode::SharedMount],
        max_parallel: 2,
    }
}

fn credentials() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(14),
        worker_epoch: 1,
        secret: SecretString::from("worker-secret"),
    }
}

async fn held_lease(permits: Arc<Semaphore>) -> HeldLeaseGuard {
    HeldLeaseGuard {
        dispatch: dispatch(json!({"path": "/media/a.mkv"})),
        _permit: permits.acquire_owned().await.unwrap(),
    }
}

fn dispatch(payload: JsonValue) -> LeaseDispatch {
    LeaseDispatch {
        lease_id: LeaseId(11),
        scheduler_decision_id: 12,
        ticket_id: TicketId(13),
        worker_id: WorkerId(14),
        operation: "probe_file".to_owned(),
        dispatch_payload: payload,
        lease_ttl_seconds: 6,
        heartbeat_after_seconds: 1,
        artifact_access_plan: access_plan(),
    }
}

fn access_plan() -> ArtifactAccessPlan {
    ArtifactAccessPlan {
        id: 15,
        input_handles: vec!["input".to_owned()],
        output_handles: vec!["output".to_owned()],
        selected_access_mode: ArtifactAccessMode::SharedMount,
    }
}

fn idle() -> AcquireOutcome {
    AcquireOutcome::Idle(AcquireIdle {
        worker_id: WorkerId(14),
        scheduler_decision_id: 12,
    })
}

fn lease_heartbeat_outcome(lease_id: LeaseId) -> LeaseHeartbeatOutcome {
    LeaseHeartbeatOutcome {
        lease_id,
        worker_id: WorkerId(14),
        ttl_seconds: 6,
    }
}

fn incarnation() -> NodeIncarnationId {
    "0123456789abcdef0123456789abcdef".parse().unwrap()
}

async fn wait_for_count(notify: &Notify, count: &AtomicUsize, expected: usize) {
    loop {
        let notified = notify.notified();
        if count.load(Ordering::SeqCst) >= expected {
            return;
        }
        notified.await;
    }
}

async fn advance_seconds(seconds: u64) {
    for _ in 0..seconds {
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
}
