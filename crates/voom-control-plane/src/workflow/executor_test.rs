use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use secrecy::SecretString;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::io::{AsyncWriteExt, DuplexStream};
use voom_core::{ErrorCode, SystemClock, WorkerId};
use voom_scheduler::SingleWorkerPerKindSelector;
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, NdjsonReader, OperationKind, OperationRequest,
    OperationResponse, ProgressFrame, ProtocolError, WorkerCredentials,
};

use super::executor::{WorkflowExecutor, WorkflowExecutorOptions, WorkflowRunSummary};
use super::model::{ConcurrencyPolicy, OperationNode, WorkflowNode, WorkflowPlan};
use super::runtime::WorkerRuntimeRegistry;

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
async fn worker_crash_fails_terminally() {
    let fixture = ExecutorFixture::single_worker_with_behavior(FakeBehavior::Crash).await;
    let err = fixture.run().await.unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::WorkerCrash);
    assert_eq!(err.summary.dispatch_count, 1);
    assert_eq!(err.summary.failure_count, 1);
}

struct ExecutorFixture {
    cp: crate::ControlPlane,
    _tmp: tempfile::NamedTempFile,
    plan: WorkflowPlan,
    registry: WorkerRuntimeRegistry,
    first_worker_id: Option<WorkerId>,
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
        let cp = crate::ControlPlane::open_with_pool(pool, Arc::new(SystemClock))
            .await
            .unwrap();
        Self {
            cp,
            _tmp: tmp,
            plan: independent_hash_plan(ticket_count),
            registry: WorkerRuntimeRegistry::new(),
            first_worker_id: None,
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
        let client = Arc::new(FakeClient::new(worker.id, behavior));
        self.registry.register_in_process_runtime(
            worker.id,
            client,
            WorkerCredentials {
                worker_id: worker.id,
                worker_epoch: worker.epoch,
                secret: SecretString::from("test-secret"),
            },
        );
        worker.id
    }

    fn worker_id(&self) -> WorkerId {
        self.first_worker_id.unwrap()
    }

    async fn run(&self) -> Result<WorkflowRunSummary, super::executor::WorkflowRunError> {
        self.run_with_options(WorkflowExecutorOptions::for_tests())
            .await
    }

    async fn run_with_policy(
        &self,
        concurrency: ConcurrencyPolicy,
    ) -> Result<WorkflowRunSummary, super::executor::WorkflowRunError> {
        let mut plan = self.plan.clone();
        plan.concurrency = concurrency;
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
            .submit_and_run(plan)
            .await
    }

    async fn run_with_options(
        &self,
        options: WorkflowExecutorOptions,
    ) -> Result<WorkflowRunSummary, super::executor::WorkflowRunError> {
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
    Crash,
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
        FakeBehavior::Crash => {}
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
        fan_out: super::model::FanOutPolicy { max_files: 3 },
        concurrency: ConcurrencyPolicy {
            max_in_flight_dispatches: 4,
        },
        timing: super::model::TimingPolicy {
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
