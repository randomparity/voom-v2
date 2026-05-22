use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use secrecy::SecretString;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, DuplexStream};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ErrorCode, JobId, SystemClock, WorkerId};
use voom_scheduler::SingleWorkerPerKindSelector;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::NewTicket;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, NdjsonReader, OperationKind, OperationRequest,
    OperationResponse, PercentBps, ProgressFrame, ProtocolError, WorkerCredentials,
};

use crate::workflow::executor::{
    WorkflowExecutor, WorkflowExecutorOptions, WorkflowRunSummary, is_synthetic_root_ticket,
};
use crate::workflow::model::{ConcurrencyPolicy, OperationNode, WorkflowNode, WorkflowPlan};
use crate::workflow::runtime::WorkerRuntimeRegistry;
use crate::workflow::ticket_payload::WorkflowTicketPayload;
use crate::workflow::timing::EffectiveTiming;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn executor_never_exceeds_max_in_flight_dispatches() {
    let fixture = ExecutorFixture::with_ready_tickets(6).await;
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 2,
        })
        .await
        .unwrap();

    assert!(summary.peak_active_workflow_leases <= 2);
    assert_eq!(summary.dispatch_count, 6);
    assert_eq!(summary.operation_count(OperationKind::HashFile), 6);
}

#[tokio::test]
async fn local_reservations_prevent_worker_capacity_overrun() {
    let fixture = ExecutorFixture::single_worker_max_parallel(1).await;
    let worker_id = fixture.worker_id();
    let summary = fixture
        .run_with_policy(ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        })
        .await
        .unwrap();

    assert_eq!(summary.max_active_for_worker(worker_id), 1);
    assert_eq!(summary.dispatch_count, 4);
}

#[tokio::test]
async fn no_eligible_worker_is_recorded_before_lease_dispatch() {
    let fixture = ExecutorFixture::without_workers(1).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn ambiguous_worker_selection_is_recorded_before_lease_dispatch() {
    let fixture = ExecutorFixture::ambiguous_workers().await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::AmbiguousWorkerSelection);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn malformed_result_frame_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::MalformedFrame).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::MalformedWorkerResult);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn progress_timeout_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let err = fixture
        .run_with_options(timeout_options())
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn retriable_dispatch_failure_retries_before_terminal_failure() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Crash).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.max_attempts = 2;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 2);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn retriable_pre_lease_failure_retries_before_terminal_failure() {
    let fixture = ExecutorFixture::without_workers(1).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.max_attempts = 2;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn heartbeat_timeout_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_millis(250);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.chaos.disable_heartbeat_ticks = true;
    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn heartbeat_timeout_wins_when_watchdog_deadlines_tie() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Hang).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_millis(20);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.heartbeat_interval = Duration::from_secs(1);
    options.chaos.disable_heartbeat_ticks = true;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    let failed_class = fixture.first_ticket_failed_class().await;
    assert_eq!(failed_class, "worker_timeout");
}

#[tokio::test]
async fn heartbeat_watchdog_is_not_starved_by_progress_frames() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::ProgressFlood).await;
    let mut options = timeout_options();
    options.progress_idle_timeout = Duration::from_secs(1);
    options.heartbeat_timeout = Duration::from_millis(20);
    options.heartbeat_interval = Duration::from_secs(1);
    options.chaos.disable_heartbeat_ticks = true;

    let err = fixture.run_with_options(options).await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerTimeout);
    let failed_class = fixture.first_ticket_failed_class().await;
    assert_eq!(failed_class, "worker_timeout");
}

#[tokio::test]
async fn worker_crash_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Crash).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

#[tokio::test]
async fn missing_runtime_fails_before_lease_acquire() {
    let mut fixture = ExecutorFixture::without_workers(1).await;
    fixture
        .register_worker_without_runtime("hash-worker", OperationKind::HashFile, 1)
        .await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(fixture.lease_count().await, 0);
}

#[tokio::test]
async fn dispatch_setup_error_fails_acquired_lease() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::DispatchError).await;

    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(fixture.held_lease_count().await, 0);
}

#[tokio::test]
async fn ready_lookup_is_scoped_to_active_workflow_job() {
    let mut fixture = ExecutorFixture::with_ready_tickets(1).await;
    fixture.seed_other_job_ready_ticket(100).await;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.ready_batch_size = 1;

    let summary = fixture.run_with_options(options).await.unwrap();

    assert_eq!(summary.dispatch_count, 1);
    assert_eq!(fixture.other_job_ready_count().await, 1);
}

#[test]
fn summary_branch_count_only_excludes_synthetic_root_ticket() {
    let synthetic_root = WorkflowTicketPayload::new_for_test(
        "workflow",
        "plan",
        "scan",
        "root",
        OperationKind::ScanLibrary,
        json!({"path": "/library"}),
    );
    let mut real_root_branch = WorkflowTicketPayload::new_for_test(
        "workflow",
        "plan",
        "probe",
        "root",
        OperationKind::ProbeFile,
        json!({"path": "/library/root.mkv"}),
    );
    real_root_branch.source_file = Some(json!({"path": "/library/root.mkv"}));

    assert!(is_synthetic_root_ticket(&synthetic_root));
    assert!(!is_synthetic_root_ticket(&real_root_branch));
}

struct ExecutorFixture {
    cp: crate::ControlPlane,
    _tmp: tempfile::NamedTempFile,
    plan: WorkflowPlan,
    registry: WorkerRuntimeRegistry,
    first_worker_id: Option<WorkerId>,
    other_job_id: Option<JobId>,
}

impl ExecutorFixture {
    async fn with_ready_tickets(ticket_count: usize) -> Self {
        let mut fixture = Self::without_workers(ticket_count).await;
        let worker_id = fixture
            .register_worker(
                "hash-worker",
                OperationKind::HashFile,
                8,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn single_worker_max_parallel(max_parallel: u32) -> Self {
        let mut fixture = Self::without_workers(4).await;
        let worker_id = fixture
            .register_worker(
                "hash-worker",
                OperationKind::HashFile,
                max_parallel,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn single_worker_with_behavior(behavior: FakeBehavior) -> Self {
        let mut fixture = Self::without_workers(1).await;
        let worker_id = fixture
            .register_worker("hash-worker", OperationKind::HashFile, 1, behavior)
            .await;
        fixture.first_worker_id = Some(worker_id);
        fixture
    }

    async fn ambiguous_workers() -> Self {
        let mut fixture = Self::without_workers(1).await;
        let first = fixture
            .register_worker(
                "hash-worker-a",
                OperationKind::HashFile,
                1,
                FakeBehavior::Success,
            )
            .await;
        let _second = fixture
            .register_worker(
                "hash-worker-b",
                OperationKind::HashFile,
                1,
                FakeBehavior::Success,
            )
            .await;
        fixture.first_worker_id = Some(first);
        fixture
    }

    async fn without_workers(ticket_count: usize) -> Self {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite://{}", tmp.path().display());
        let _ = voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let cp = crate::ControlPlane::open_with_pool_and_rng(
            pool,
            Arc::new(SystemClock),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await
        .unwrap();
        Self {
            cp,
            _tmp: tmp,
            plan: independent_hash_plan(ticket_count),
            registry: WorkerRuntimeRegistry::new(),
            first_worker_id: None,
            other_job_id: None,
        }
    }

    async fn register_worker(
        &mut self,
        name: &str,
        operation: OperationKind,
        max_parallel: u32,
        behavior: FakeBehavior,
    ) -> WorkerId {
        let worker = self
            .register_worker_without_runtime(name, operation, max_parallel)
            .await;
        let client = Arc::new(FakeClient::new(worker, behavior));
        self.registry.register_in_process_runtime(
            worker,
            client,
            WorkerCredentials {
                worker_id: worker,
                worker_epoch: 0,
                secret: SecretString::from("test-secret"),
            },
        );
        worker
    }

    async fn register_worker_without_runtime(
        &mut self,
        name: &str,
        operation: OperationKind,
        max_parallel: u32,
    ) -> WorkerId {
        let worker = self
            .cp
            .register_worker(NewWorker {
                name: name.to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: T0,
            })
            .await
            .unwrap();
        let operation_name = operation_name(operation);
        self.cp
            .record_capability(NewCapability {
                worker_id: worker.id,
                operation: operation_name.clone(),
                codecs: Vec::new(),
                hardware: Vec::new(),
                artifact_access: Vec::new(),
                extra: json!({}),
            })
            .await
            .unwrap();
        self.cp
            .record_grant(NewGrant {
                worker_id: worker.id,
                can_execute: vec![operation_name.clone()],
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: json!({ operation_name: max_parallel }),
            })
            .await
            .unwrap();
        worker.id
    }

    fn worker_id(&self) -> WorkerId {
        self.first_worker_id.unwrap()
    }

    async fn run(&self) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        self.run_with_options(WorkflowExecutorOptions::for_tests())
            .await
    }

    async fn run_with_policy(
        &self,
        concurrency: ConcurrencyPolicy,
    ) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        let mut plan = self.plan.clone();
        plan.concurrency = concurrency;
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
            .submit_and_run(plan)
            .await
    }

    async fn run_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> Result<WorkflowRunSummary, crate::workflow::executor::WorkflowRunError> {
        self.executor_with_options(options)
            .submit_and_run(self.plan.clone())
            .await
    }

    fn executor_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> WorkflowExecutor<SingleWorkerPerKindSelector> {
        WorkflowExecutor::with_options(
            self.cp.clone(),
            SingleWorkerPerKindSelector,
            self.registry.clone(),
            options,
        )
    }

    async fn lease_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn held_lease_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE state = 'held'")
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn seed_other_job_ready_ticket(&mut self, priority: i64) {
        let job = self
            .cp
            .open_job(NewJob {
                kind: "other.workflow".to_owned(),
                priority,
                created_at: T0,
            })
            .await
            .unwrap();
        let operation = OperationKind::HashFile;
        let rendered_payload = json!({
            "operation": operation_name(operation),
            "branch_id": "other",
            "path": "/library/other.mkv",
            "duration_ms": 10_u64,
            "progress_interval_ms": 1_u64,
        });
        let payload = WorkflowTicketPayload {
            workflow_id: "other-workflow".to_owned(),
            plan_id: "other-plan".to_owned(),
            node_id: "hash-other".to_owned(),
            branch_id: "other".to_owned(),
            operation,
            rendered_payload,
            timing: EffectiveTiming::for_test(10, 1),
            source_file: None,
        }
        .to_ticket_payload()
        .unwrap();
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(job.id),
                kind: format!("synthetic.workflow.operation.{}", operation_name(operation)),
                priority,
                payload,
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, T0)
            .await
            .unwrap();
        self.other_job_id = Some(job.id);
    }

    async fn other_job_ready_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE job_id = ? AND state = 'ready'")
            .bind(i64::try_from(self.other_job_id.unwrap().0).unwrap())
            .fetch_one(&self.cp.pool)
            .await
            .unwrap()
    }

    async fn first_ticket_failed_class(&self) -> String {
        let payload: String = sqlx::query_scalar(
            "SELECT payload FROM events \
             WHERE kind = 'ticket.failed_terminal' \
             ORDER BY event_id ASC LIMIT 1",
        )
        .fetch_one(&self.cp.pool)
        .await
        .unwrap();
        serde_json::from_str::<Value>(&payload).unwrap()["class"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

#[derive(Debug)]
struct FakeClient {
    worker_id: WorkerId,
    behavior: FakeBehavior,
    active: Arc<AtomicU32>,
    max_active: AtomicU32,
}

impl FakeClient {
    fn new(worker_id: WorkerId, behavior: FakeBehavior) -> Self {
        Self {
            worker_id,
            behavior,
            active: Arc::new(AtomicU32::new(0)),
            max_active: AtomicU32::new(0),
        }
    }

    fn enter_active(&self) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, Copy)]
enum FakeBehavior {
    Success,
    MalformedFrame,
    Hang,
    ProgressFlood,
    Crash,
    DispatchError,
}

#[async_trait]
impl ClientHandle for FakeClient {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        assert_eq!(_creds.worker_id, self.worker_id);
        if matches!(self.behavior, FakeBehavior::DispatchError) {
            return Err(ProtocolError::InternalServerError);
        }
        self.enter_active();
        let (reader, writer) = tokio::io::duplex(16 * 1024);
        let behavior = self.behavior;
        let lease_id = request.lease_id;
        let active = self.active.clone();
        tokio::spawn(async move {
            write_behavior(writer, request, behavior).await;
            active.fetch_sub(1, Ordering::SeqCst);
        });
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id,
                accepted_at: Utc::now(),
            },
            frames: NdjsonReader::new(
                Box::pin(reader) as Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>,
                lease_id,
            ),
        })
    }
}

async fn write_behavior(
    mut writer: DuplexStream,
    request: OperationRequest,
    behavior: FakeBehavior,
) {
    match behavior {
        FakeBehavior::Success => {
            tokio::time::sleep(Duration::from_millis(25)).await;
            write_frame(&mut writer, result_frame(&request, json!({"ok": true}))).await;
        }
        FakeBehavior::MalformedFrame => {
            let _ = writer.write_all(b"{not-json}\n").await;
        }
        FakeBehavior::Hang => {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        FakeBehavior::ProgressFlood => {
            for seq in 0..128 {
                tokio::time::sleep(Duration::from_millis(1)).await;
                write_frame(&mut writer, progress_frame(&request, seq)).await;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        FakeBehavior::Crash | FakeBehavior::DispatchError => {}
    }
}

async fn write_frame(writer: &mut DuplexStream, frame: ProgressFrame) {
    let bytes = serde_json::to_vec(&frame).unwrap();
    writer.write_all(&bytes).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
}

fn result_frame(request: &OperationRequest, payload: Value) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id: request.lease_id,
        seq: 0,
        emitted_at: Utc::now(),
        payload,
    }
}

fn progress_frame(request: &OperationRequest, seq: u64) -> ProgressFrame {
    ProgressFrame::Progress {
        lease_id: request.lease_id,
        seq,
        emitted_at: Utc::now(),
        percent: Some(PercentBps::try_from(100).unwrap()),
        message: None,
        payload: None,
    }
}

fn independent_hash_plan(ticket_count: usize) -> WorkflowPlan {
    WorkflowPlan {
        id: format!("executor-test-{ticket_count}"),
        seed: 2,
        nodes: (0..ticket_count)
            .map(|index| {
                WorkflowNode::Operation(OperationNode {
                    id: format!("hash-{index}"),
                    operation: OperationKind::HashFile,
                    depends_on: Vec::new(),
                    depends_on_selected: Vec::new(),
                    provides_selected: None,
                })
            })
            .collect(),
        fan_out: crate::workflow::model::FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: crate::workflow::model::TimingPolicy {
            base_duration_ms: 10,
            jitter_ms: 0,
        },
    }
}

fn timeout_options() -> WorkflowExecutorOptions {
    let mut options = WorkflowExecutorOptions::for_tests();
    options.progress_idle_timeout = Duration::from_millis(20);
    options.heartbeat_timeout = Duration::from_millis(250);
    options.heartbeat_interval = Duration::from_millis(10);
    options
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}
