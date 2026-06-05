#![expect(
    clippy::unwrap_used,
    clippy::panic_in_result_fn,
    reason = "integration tests fail fast on unexpected durable state"
)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::ControlPlane;
use crate::workflow::{
    WorkerRuntimeRegistry, WorkflowChaosOptions, WorkflowExecutor, WorkflowExecutorOptions,
    WorkflowPlan, WorkflowRunSummary,
};
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, SqlitePool};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ErrorCode, FailureClass, JobId, SystemClock, TicketOperation, WorkerId};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::http::OperationBody;
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, HttpClient, NdjsonReader, OperationKind,
    OperationRequest, ProtocolError, WorkerCredentials,
};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
static PROCESS_PROVIDER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn process_provider_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    PROCESS_PROVIDER_TEST_LOCK.lock().await
}

#[tokio::test]
async fn default_ci_workflow_runs_all_branches_through_real_scheduler() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_all_fake_providers().await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .map_err(|err| io_error(format!("workflow failed: {:?}", err.source)))?;

        expect_eq("branch_count", &summary.branch_count, &3)?;
        expect_eq("dispatch_count", &summary.dispatch_count, &31)?;
        expect_eq(
            "remux operation count",
            &summary.operation_count(OperationKind::Remux),
            &2,
        )?;
        expect_eq(
            "transcode operation count",
            &summary.operation_count(OperationKind::TranscodeVideo),
            &1,
        )?;
        expect(
            "peak_active_workflow_leases should exceed 1",
            summary.peak_active_workflow_leases > 1,
        )?;
        fixture.assert_job_succeeded(summary.job_id).await?;
        fixture
            .assert_all_workflow_tickets_succeeded(summary.job_id)
            .await
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

// Chaos / fault-injection coverage. Five tests pin the failure-class mapping the
// executor applies to a misbehaving worker: WorkerCrash, WorkerTimeout (dispatch
// timeout), MalformedWorkerResult, ProgressTimeout, and the missed-heartbeat
// watchdog. WorkerCrash / MalformedResult / ProgressTimeout / missed-heartbeat run
// the in-house `chaos-worker` fake out-of-process so the crash and stall modes have
// real-process fidelity. The dispatch-timeout case is driven deterministically by
// an in-process runtime whose dispatch never returns (see
// `start_with_unreachable_runtime_override`); it previously relied on a real
// timeout elapsing under a 120ms watchdog with out-of-process prerequisites, which
// flaked on loaded runners.
//
// `third_party/chaos-librarian` is unrelated: it is a media-library *fixture*
// generator (synthetic files/scenarios for scanner/probe tests), with no worker,
// lease, or failure-class concept, so it does not replace the `chaos-worker` fake.
#[tokio::test]
async fn chaos_worker_crash_maps_to_worker_crash() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override(
        OperationKind::ProbeFile,
        ChaosWorkerMode::Crash,
    )
    .await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::WorkerCrash,
            )
            .await?;
        fixture
            .assert_no_success_for_operation(summary.job_id, OperationKind::ProbeFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::ProbeFile,
            FailureClass::WorkerCrash,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_dispatch_timeout_maps_to_worker_timeout() -> TestResult<()> {
    let mut fixture =
        DurableWorkflowFixture::start_with_unreachable_runtime_override(OperationKind::ProbeFile)
            .await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::WorkerTimeout,
            )
            .await?;
        fixture
            .assert_no_terminal_frame_accepted(summary.job_id, OperationKind::ProbeFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::ProbeFile,
            FailureClass::WorkerTimeout,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_malformed_result_maps_to_malformed_worker_result() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override(
        OperationKind::ProbeFile,
        ChaosWorkerMode::MalformedResult,
    )
    .await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::MalformedWorkerResult,
            )
            .await?;
        fixture
            .assert_no_failure_class(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::WorkerCrash,
            )
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::ProbeFile,
            FailureClass::MalformedWorkerResult,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_progress_timeout_maps_to_progress_timeout() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override(
        OperationKind::ProbeFile,
        ChaosWorkerMode::DeadlineExceeded,
    )
    .await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::ProgressTimeout,
            )
            .await?;
        fixture
            .assert_heartbeat_events_exist(summary.job_id, OperationKind::ProbeFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::ProbeFile,
            FailureClass::ProgressTimeout,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_missed_heartbeat_uses_executor_watchdog() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let chaos = WorkflowChaosOptions::suppress_heartbeats_for_operation(OperationKind::ProbeFile);
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override_and_options(
        OperationKind::ProbeFile,
        ChaosWorkerMode::Stall,
        chaos,
        DeadlineFixture {
            heartbeat_deadline_ms: 100,
            progress_idle_deadline_ms: 1_000,
        },
    )
    .await?;
    fixture.assert_heartbeat_deadline_precedes_progress_timeout()?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::ProbeFile,
                FailureClass::WorkerTimeout,
            )
            .await?;
        fixture
            .assert_no_expire_due_path(summary.job_id, OperationKind::ProbeFile)
            .await?;
        fixture
            .assert_no_progress_triggered_heartbeat(summary.job_id, OperationKind::ProbeFile)
            .await?;
        fixture
            .assert_no_terminal_frame_accepted(summary.job_id, OperationKind::ProbeFile)
            .await?;
        fixture
            .assert_no_malformed_frame(summary.job_id, OperationKind::ProbeFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::ProbeFile,
            FailureClass::WorkerTimeout,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn benchmark_durable_workflow_reports_non_zero_throughput() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_all_fake_providers().await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci())
            .await
            .map_err(|err| io_error(format!("workflow failed: {:?}", err.source)))?;

        expect(
            "durable workflow throughput should be non-zero",
            summary.throughput_per_second > 0.0,
        )?;
        let scan = summary
            .per_operation
            .get(&OperationKind::ScanLibrary)
            .ok_or_else(|| io_error("scan operation summary missing"))?;
        expect(
            "scan dispatch count should be populated",
            scan.dispatch_count > 0,
        )?;
        expect(
            "scan success count should be populated",
            scan.success_count > 0,
        )?;
        expect("scan elapsed should be populated", !scan.elapsed.is_zero())?;
        expect(
            "scan throughput should be non-zero",
            scan.throughput_per_second > 0.0,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn stress_durable_workflow_respects_dispatch_and_worker_parallel_limits() -> TestResult<()> {
    let mut fixture = DurableWorkflowFixture::start_all_in_process_fake_providers(1).await?;
    let result = async {
        let mut plan = WorkflowPlan::default_ci();
        plan.concurrency.max_in_flight_dispatches = 3;
        plan.timing.base_duration_ms = 80;
        plan.timing.jitter_ms = 0;
        let summary = fixture
            .executor()
            .submit_and_run(plan)
            .await
            .map_err(|err| io_error(format!("workflow failed: {:?}", err.source)))?;

        expect(
            "stress peak active leases should exceed one",
            summary.peak_active_workflow_leases > 1,
        )?;
        expect(
            "max_in_flight_dispatches should be respected",
            summary.peak_active_workflow_leases <= 3,
        )?;
        fixture.assert_worker_parallel_limits(&summary)?;
        expect(
            "stress throughput should be non-zero",
            summary.throughput_per_second > 0.0,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn pre_lease_no_worker_retries_then_terminal_fails_without_dispatch() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.max_attempts = 2;

    let err = fixture
        .executor_with_options(options)
        .submit_and_run(WorkflowPlan::default_ci())
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::NoEligibleWorker);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.retry_count, 1);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    fixture.assert_job_failed(err.summary.job_id).await?;
    fixture
        .assert_ticket_state_counts(err.summary.job_id, 0, 0, 1)
        .await?;
    fixture.assert_lease_count(0).await?;
    Ok(())
}

#[tokio::test]
async fn pre_lease_ambiguous_worker_terminal_fails_without_dispatch() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    fixture
        .register_worker_without_runtime(
            "scanner-a",
            &[OperationKind::ScanLibrary],
            1,
            "ambiguous-a-secret",
        )
        .await?;
    fixture
        .register_worker_without_runtime(
            "scanner-b",
            &[OperationKind::ScanLibrary],
            1,
            "ambiguous-b-secret",
        )
        .await?;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.queue.max_attempts = 2;

    let err = fixture
        .executor_with_options(options)
        .submit_and_run(WorkflowPlan::default_ci())
        .await
        .unwrap_err();

    assert_eq!(err.source.error_code(), ErrorCode::AmbiguousWorkerSelection);
    assert_eq!(err.summary.dispatch_count, 0);
    assert_eq!(err.summary.retry_count, 0);
    assert_eq!(err.summary.failure_count, 1);
    assert_eq!(err.summary.peak_active_workflow_leases, 0);
    fixture.assert_job_failed(err.summary.job_id).await?;
    fixture
        .assert_ticket_state_counts(err.summary.job_id, 0, 0, 1)
        .await?;
    fixture.assert_lease_count(0).await?;
    Ok(())
}

struct DurableWorkflowFixture {
    cp: ControlPlane,
    pool: SqlitePool,
    _tmp: tempfile::NamedTempFile,
    registry: WorkerRuntimeRegistry,
    launches: Vec<ProviderLaunch>,
    registered_workers: Vec<(WorkerId, u32)>,
    executor_options: WorkflowExecutorOptions,
    deadline_fixture: Option<DeadlineFixture>,
}

impl DurableWorkflowFixture {
    async fn start_all_fake_providers() -> TestResult<Self> {
        Self::start_all_fake_providers_with_max_parallel(1).await
    }

    async fn start_all_fake_providers_with_max_parallel(max_parallel: u32) -> TestResult<Self> {
        let mut fixture = Self::without_fake_providers().await?;
        fixture.executor_options.timing.heartbeat_timeout = Duration::from_secs(2);
        fixture.executor_options.timing.progress_idle_timeout = Duration::from_secs(2);
        for provider in provider_specs() {
            if let Err(err) = fixture
                .register_process_provider(provider, max_parallel)
                .await
            {
                return combine_result_and_cleanup(Err(err), fixture.shutdown().await);
            }
        }
        Ok(fixture)
    }

    async fn start_all_in_process_fake_providers(max_parallel: u32) -> TestResult<Self> {
        let mut fixture = Self::without_fake_providers().await?;
        fixture.executor_options.timing.heartbeat_timeout = Duration::from_secs(2);
        fixture.executor_options.timing.progress_idle_timeout = Duration::from_secs(2);
        for provider in provider_specs() {
            fixture
                .register_in_process_provider(provider, max_parallel)
                .await?;
        }
        Ok(fixture)
    }

    async fn start_with_chaos_override(
        operation: OperationKind,
        mode: ChaosWorkerMode,
    ) -> TestResult<Self> {
        let mut options = WorkflowExecutorOptions::for_tests();
        options.timing.heartbeat_interval = Duration::from_millis(20);
        options.timing.heartbeat_timeout = Duration::from_millis(500);
        options.timing.progress_idle_timeout = Duration::from_millis(150);
        Self::start_with_chaos_override_and_executor_options(operation, mode, options, None).await
    }

    async fn start_with_chaos_override_and_options(
        operation: OperationKind,
        mode: ChaosWorkerMode,
        mut chaos: WorkflowChaosOptions,
        deadlines: DeadlineFixture,
    ) -> TestResult<Self> {
        let mut options = WorkflowExecutorOptions::for_tests();
        options.timing.heartbeat_interval = Duration::from_millis(20);
        options.timing.heartbeat_timeout =
            Duration::from_millis(u64::from(deadlines.heartbeat_deadline_ms));
        options.timing.progress_idle_timeout =
            Duration::from_millis(u64::from(deadlines.progress_idle_deadline_ms));
        chaos.set_payload_mode_for_operation(operation, mode.payload_mode());
        options.chaos = chaos;
        Self::start_with_chaos_override_and_executor_options(
            operation,
            mode,
            options,
            Some(deadlines),
        )
        .await
    }

    async fn start_with_chaos_override_and_executor_options(
        operation: OperationKind,
        mode: ChaosWorkerMode,
        mut options: WorkflowExecutorOptions,
        deadline_fixture: Option<DeadlineFixture>,
    ) -> TestResult<Self> {
        options
            .chaos
            .set_payload_mode_for_operation(operation, mode.payload_mode());
        let mut fixture = Self::without_fake_providers().await?;
        fixture.executor_options = options;
        fixture.deadline_fixture = deadline_fixture;
        let setup = async {
            fixture
                .register_process_providers_except(operation, 4)
                .await?;
            fixture.register_chaos_provider(operation, mode).await
        }
        .await;
        if let Err(err) = setup {
            return combine_result_and_cleanup(Err(err), fixture.shutdown().await);
        }
        Ok(fixture)
    }

    async fn start_with_unreachable_runtime_override(operation: OperationKind) -> TestResult<Self> {
        // Deterministic dispatch-timeout fixture. The healthy branches run on
        // in-process fake providers that answer in microseconds, and the watchdog
        // budget is generous (2s), so a CPU-loaded runner never trips a healthy
        // branch. The branch under test runs on an in-process runtime whose
        // dispatch never returns, so the executor's dispatch timeout always maps
        // it to WorkerTimeout regardless of wall-clock latency. The earlier
        // version used out-of-process workers under a 120ms watchdog, which let a
        // loaded runner time out a prerequisite branch and flake the assertion.
        let mut fixture = Self::without_fake_providers().await?;
        fixture.executor_options.timing.heartbeat_timeout = Duration::from_secs(2);
        fixture.executor_options.timing.progress_idle_timeout = Duration::from_secs(2);
        let setup = async {
            fixture
                .register_in_process_providers_except(operation, 4)
                .await?;
            fixture.register_pending_dispatch_runtime(operation).await
        }
        .await;
        if let Err(err) = setup {
            return combine_result_and_cleanup(Err(err), fixture.shutdown().await);
        }
        Ok(fixture)
    }

    async fn without_fake_providers() -> TestResult<Self> {
        let tmp = tempfile::NamedTempFile::new()?;
        let url = format!("sqlite://{}", tmp.path().display());
        voom_store::init(&url).await?;
        let pool = connect_single_connection_pool(&url).await?;
        let cp = ControlPlane::open_with_pool_and_rng(
            pool.clone(),
            Arc::new(SystemClock),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await?;

        Ok(Self {
            cp,
            pool,
            _tmp: tmp,
            registry: WorkerRuntimeRegistry::new(),
            launches: Vec::new(),
            registered_workers: Vec::new(),
            executor_options: WorkflowExecutorOptions::for_tests(),
            deadline_fixture: None,
        })
    }

    fn executor(&self) -> WorkflowExecutor {
        self.executor_with_options(self.executor_options.clone())
    }

    fn executor_with_options(&self, options: WorkflowExecutorOptions) -> WorkflowExecutor {
        WorkflowExecutor::with_options(self.cp.clone(), self.registry.clone(), options)
    }

    async fn register_process_provider(
        &mut self,
        provider: ProviderSpec,
        max_parallel: u32,
    ) -> TestResult<()> {
        self.register_process_provider_operations(provider.name, provider.operations, max_parallel)
            .await
    }

    async fn register_in_process_provider(
        &mut self,
        provider: ProviderSpec,
        max_parallel: u32,
    ) -> TestResult<()> {
        self.register_in_process_provider_operations(
            provider.name,
            provider.operations,
            max_parallel,
        )
        .await
    }

    async fn register_in_process_provider_operations(
        &mut self,
        name: &'static str,
        operations: &[OperationKind],
        max_parallel: u32,
    ) -> TestResult<()> {
        let secret = format!("durable-workflow-{name}-secret");
        let worker = self
            .register_worker_without_runtime(name, operations, max_parallel, &secret)
            .await?;
        self.registered_workers.push((worker, max_parallel));
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(InProcessFakeProvider::new(name)?),
            WorkerCredentials {
                worker_id: worker,
                worker_epoch: 0,
                secret: SecretString::from(secret),
            },
        );
        Ok(())
    }

    async fn register_in_process_providers_except(
        &mut self,
        skipped: OperationKind,
        max_parallel: u32,
    ) -> TestResult<()> {
        for provider in provider_specs() {
            let operations = provider
                .operations
                .iter()
                .copied()
                .filter(|operation| *operation != skipped)
                .collect::<Vec<_>>();
            if operations.is_empty() {
                continue;
            }
            self.register_in_process_provider_operations(provider.name, &operations, max_parallel)
                .await?;
        }
        Ok(())
    }

    async fn register_process_providers_except(
        &mut self,
        skipped: OperationKind,
        max_parallel: u32,
    ) -> TestResult<()> {
        for provider in provider_specs() {
            let operations = provider
                .operations
                .iter()
                .copied()
                .filter(|operation| *operation != skipped)
                .collect::<Vec<_>>();
            if operations.is_empty() {
                continue;
            }
            self.register_process_provider_operations(provider.name, &operations, max_parallel)
                .await?;
        }
        Ok(())
    }

    async fn register_process_provider_operations(
        &mut self,
        name: &'static str,
        operations: &[OperationKind],
        max_parallel: u32,
    ) -> TestResult<()> {
        let secret = format!("durable-workflow-{name}-secret");
        let worker = self
            .register_worker_without_runtime(name, operations, max_parallel, &secret)
            .await?;
        self.registered_workers.push((worker, max_parallel));
        let launch = ProviderLaunch::spawn(name, worker, &secret, false).await?;
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(HttpClient::new(launch.bound)),
            launch.credentials.clone(),
        );
        self.launches.push(launch);
        Ok(())
    }

    async fn register_chaos_provider(
        &mut self,
        operation: OperationKind,
        mode: ChaosWorkerMode,
    ) -> TestResult<()> {
        expect_eq(
            "chaos worker operation",
            &operation,
            &OperationKind::ProbeFile,
        )?;
        let secret = "durable-workflow-chaos-secret";
        let worker = self
            .register_worker_without_runtime("chaos-probe", &[operation], 1, secret)
            .await?;
        self.registered_workers.push((worker, 1));
        let launch = ProviderLaunch::spawn(
            "chaos-worker",
            worker,
            secret,
            mode == ChaosWorkerMode::Crash,
        )
        .await?;
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(HttpClient::new(launch.bound)),
            launch.credentials.clone(),
        );
        self.launches.push(launch);
        Ok(())
    }

    async fn register_pending_dispatch_runtime(
        &mut self,
        operation: OperationKind,
    ) -> TestResult<()> {
        let secret = "durable-workflow-pending-secret";
        let worker = self
            .register_worker_without_runtime("pending-probe", &[operation], 1, secret)
            .await?;
        self.registered_workers.push((worker, 1));
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(UnreachableInProcessProvider),
            WorkerCredentials {
                worker_id: worker,
                worker_epoch: 0,
                secret: SecretString::from(secret.to_owned()),
            },
        );
        Ok(())
    }

    async fn register_worker_without_runtime(
        &self,
        name: &str,
        operations: &[OperationKind],
        max_parallel: u32,
        secret: &str,
    ) -> TestResult<WorkerId> {
        let worker = self
            .cp
            .register_worker(NewWorker {
                name: name.to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: T0,
                node_id: None,
            })
            .await?;
        let operation_names: Vec<String> = operations.iter().copied().map(operation_name).collect();
        for operation in &operation_names {
            self.cp
                .record_capability(NewCapability {
                    worker_id: worker.id,
                    operation: TicketOperation::new(operation.clone())?,
                    codecs: Vec::new(),
                    hardware: Vec::new(),
                    artifact_access: Vec::new(),
                    extra: json!({ "secret_label": secret }),
                })
                .await?;
        }
        let max_parallel_by_operation = operation_names
            .iter()
            .map(|operation| (operation.clone(), json!(max_parallel)))
            .collect::<serde_json::Map<_, _>>();
        self.cp
            .record_grant(NewGrant {
                worker_id: worker.id,
                can_execute: operation_names
                    .iter()
                    .cloned()
                    .map(TicketOperation::new)
                    .collect::<Result<Vec<_>, _>>()?,
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: Value::Object(max_parallel_by_operation),
            })
            .await?;
        Ok(worker.id)
    }

    async fn assert_job_succeeded(&self, job_id: JobId) -> TestResult<()> {
        self.assert_job_state(job_id, "succeeded").await
    }

    async fn assert_job_failed(&self, job_id: JobId) -> TestResult<()> {
        self.assert_job_state(job_id, "failed").await
    }

    async fn assert_job_state(&self, job_id: JobId, expected: &str) -> TestResult<()> {
        let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = ?")
            .bind(i64::try_from(job_id.0)?)
            .fetch_one(&self.pool)
            .await?;
        expect_eq("job state", &state.as_str(), &expected)
    }

    async fn assert_all_workflow_tickets_succeeded(&self, job_id: JobId) -> TestResult<()> {
        let unfinished: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? \
               AND json_extract(payload, '$.workflow_id') IS NOT NULL \
               AND state != 'succeeded'",
        )
        .bind(i64::try_from(job_id.0)?)
        .fetch_one(&self.pool)
        .await?;
        expect_eq("unfinished workflow ticket count", &unfinished, &0)
    }

    async fn assert_ticket_state_counts(
        &self,
        job_id: JobId,
        ready: i64,
        succeeded: i64,
        failed: i64,
    ) -> TestResult<()> {
        let counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT \
                SUM(CASE WHEN state = 'ready' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN state = 'succeeded' THEN 1 ELSE 0 END), \
                SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END) \
             FROM tickets WHERE job_id = ?",
        )
        .bind(i64::try_from(job_id.0)?)
        .fetch_one(&self.pool)
        .await?;
        assert_eq!(counts, (ready, succeeded, failed));
        Ok(())
    }

    async fn assert_lease_count(&self, expected: i64) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
            .fetch_one(&self.pool)
            .await?;
        assert_eq!(count, expected);
        Ok(())
    }

    async fn assert_ticket_failed_with(
        &self,
        job_id: JobId,
        operation: OperationKind,
        class: FailureClass,
    ) -> TestResult<()> {
        let count = self.failure_class_count(job_id, operation, class).await?;
        expect(
            &format!(
                "expected failed {operation:?} ticket with class {}",
                failure_class_name(class)
            ),
            count > 0,
        )?;
        let durable_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM tickets t \
             JOIN leases l ON l.ticket_id = t.id \
             JOIN events lease_event \
               ON lease_event.subject_type = 'lease' \
              AND lease_event.subject_id = l.id \
              AND lease_event.kind = 'lease.released' \
             JOIN events ticket_event \
               ON ticket_event.subject_type = 'ticket' \
              AND ticket_event.subject_id = t.id \
              AND ticket_event.kind = 'ticket.failed_terminal' \
             WHERE t.job_id = ? \
               AND t.state = 'failed' \
               AND json_extract(t.payload, '$.operation') = ? \
               AND l.state = 'released' \
               AND l.release_reason = 'failed_terminal' \
               AND json_extract(lease_event.payload, '$.release_reason') = 'failed_terminal' \
               AND json_extract(ticket_event.payload, '$.class') = ?",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .bind(failure_class_name(class))
        .fetch_one(&self.pool)
        .await?;
        expect(
            &format!(
                "expected durable failed ticket and lease state for {operation:?} class {}",
                failure_class_name(class)
            ),
            durable_count > 0,
        )
    }

    async fn assert_no_failure_class(
        &self,
        job_id: JobId,
        operation: OperationKind,
        class: FailureClass,
    ) -> TestResult<()> {
        let count = self.failure_class_count(job_id, operation, class).await?;
        expect_eq(
            &format!(
                "unexpected {operation:?} failure class {}",
                failure_class_name(class)
            ),
            &count,
            &0,
        )
    }

    async fn failure_class_count(
        &self,
        job_id: JobId,
        operation: OperationKind,
        class: FailureClass,
    ) -> TestResult<i64> {
        Ok(sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM tickets t \
             JOIN events e ON e.subject_type = 'ticket' AND e.subject_id = t.id \
             WHERE t.job_id = ? \
               AND json_extract(t.payload, '$.operation') = ? \
               AND e.kind IN ('ticket.failed_terminal', 'ticket.failed_retriable') \
               AND json_extract(e.payload, '$.class') = ?",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .bind(failure_class_name(class))
        .fetch_one(&self.pool)
        .await?)
    }

    async fn assert_no_success_for_operation(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM tickets \
             WHERE job_id = ? \
               AND state = 'succeeded' \
               AND json_extract(payload, '$.operation') = ?",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .fetch_one(&self.pool)
        .await?;
        expect_eq("operation success count", &count, &0)
    }

    async fn assert_no_terminal_frame_accepted(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        self.assert_no_success_for_operation(job_id, operation)
            .await
    }

    async fn assert_heartbeat_events_exist(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM leases l \
             JOIN tickets t ON t.id = l.ticket_id \
             WHERE t.job_id = ? \
               AND json_extract(t.payload, '$.operation') = ? \
               AND l.last_heartbeat_at > l.acquired_at",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .fetch_one(&self.pool)
        .await?;
        expect("expected heartbeat-updated lease row", count > 0)
    }

    async fn assert_no_expire_due_path(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM leases l \
             JOIN tickets t ON t.id = l.ticket_id \
             JOIN events e ON e.subject_type = 'lease' AND e.subject_id = l.id \
             WHERE t.job_id = ? \
               AND json_extract(t.payload, '$.operation') = ? \
               AND e.kind = 'lease.expired'",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .fetch_one(&self.pool)
        .await?;
        expect_eq("lease.expired event count", &count, &0)
    }

    async fn assert_no_progress_triggered_heartbeat(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) \
             FROM leases l \
             JOIN tickets t ON t.id = l.ticket_id \
             WHERE t.job_id = ? \
               AND json_extract(t.payload, '$.operation') = ? \
               AND l.last_heartbeat_at != l.acquired_at",
        )
        .bind(i64::try_from(job_id.0)?)
        .bind(operation_name(operation))
        .fetch_one(&self.pool)
        .await?;
        expect_eq("heartbeat mutation count", &count, &0)
    }

    async fn assert_no_malformed_frame(
        &self,
        job_id: JobId,
        operation: OperationKind,
    ) -> TestResult<()> {
        self.assert_no_failure_class(job_id, operation, FailureClass::MalformedWorkerResult)
            .await
    }

    fn assert_failure_summary(
        summary: &WorkflowRunSummary,
        operation: OperationKind,
        class: FailureClass,
    ) -> TestResult<()> {
        let operation_summary = summary
            .per_operation
            .get(&operation)
            .ok_or_else(|| io_error(format!("{operation:?} summary missing")))?;
        expect(
            &format!("{operation:?} summary failure count"),
            operation_summary.failure_count > 0,
        )?;
        expect_eq(
            &format!("{operation:?} summary failure class"),
            &operation_summary.last_failure_class,
            &Some(class),
        )
    }

    fn assert_worker_parallel_limits(&self, summary: &WorkflowRunSummary) -> TestResult<()> {
        for (worker_id, max_parallel) in &self.registered_workers {
            expect(
                &format!("worker {worker_id} exceeded max_parallel {max_parallel}"),
                summary.max_active_for_worker(*worker_id) <= *max_parallel,
            )?;
        }
        Ok(())
    }

    fn assert_heartbeat_deadline_precedes_progress_timeout(&self) -> TestResult<()> {
        let fixture = self
            .deadline_fixture
            .ok_or_else(|| io_error("deadline fixture missing"))?;
        expect(
            "heartbeat deadline should precede progress timeout",
            fixture.heartbeat_deadline_ms < fixture.progress_idle_deadline_ms,
        )
    }

    async fn shutdown(&mut self) -> TestResult<()> {
        let mut cleanup_error: Option<String> = None;
        while let Some(mut launch) = self.launches.pop() {
            if let Err(err) = launch.shutdown().await {
                match &mut cleanup_error {
                    Some(existing) => {
                        existing.push_str("; ");
                        existing.push_str(&err.to_string());
                    }
                    None => cleanup_error = Some(err.to_string()),
                }
            }
        }
        if let Some(cleanup_error) = cleanup_error {
            Err(io_error(cleanup_error))
        } else {
            Ok(())
        }
    }
}

struct ProviderSpec {
    name: &'static str,
    operations: &'static [OperationKind],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChaosWorkerMode {
    Crash,
    MalformedResult,
    DeadlineExceeded,
    Stall,
}

impl ChaosWorkerMode {
    fn payload_mode(self) -> &'static str {
        match self {
            Self::Crash => "crash",
            Self::MalformedResult => "malformed_result",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::Stall => "stall",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DeadlineFixture {
    heartbeat_deadline_ms: u32,
    progress_idle_deadline_ms: u32,
}

fn provider_specs() -> Vec<ProviderSpec> {
    vec![
        ProviderSpec {
            name: "fake-scanner",
            operations: &[OperationKind::ScanLibrary],
        },
        ProviderSpec {
            name: "fake-prober",
            operations: &[OperationKind::ProbeFile, OperationKind::HashFile],
        },
        ProviderSpec {
            name: "fake-transcoder",
            operations: &[
                OperationKind::TranscodeVideo,
                OperationKind::ExtractAudio,
                OperationKind::TranscribeAudio,
            ],
        },
        ProviderSpec {
            name: "fake-remuxer",
            operations: &[OperationKind::Remux],
        },
        ProviderSpec {
            name: "fake-backup-store",
            operations: &[OperationKind::BackUpFile, OperationKind::DeleteArtifact],
        },
        ProviderSpec {
            name: "fake-health-checker",
            operations: &[OperationKind::VerifyArtifact],
        },
        ProviderSpec {
            name: "fake-identity-provider",
            operations: &[OperationKind::IdentifyMedia],
        },
        ProviderSpec {
            name: "fake-external-system",
            operations: &[OperationKind::SyncExternalSystem],
        },
        ProviderSpec {
            name: "fake-quality-scorer",
            operations: &[OperationKind::ScoreQuality],
        },
        ProviderSpec {
            name: "fake-issue-provider",
            operations: &[OperationKind::CommitArtifact],
        },
        ProviderSpec {
            name: "fake-use-lease-provider",
            operations: &[OperationKind::EditTracks],
        },
    ]
}

struct ProviderLaunch {
    child: Child,
    stdin: Option<ChildStdin>,
    bound: std::net::SocketAddr,
    credentials: WorkerCredentials,
    name: &'static str,
    allow_nonzero_exit: bool,
}

impl ProviderLaunch {
    async fn spawn(
        name: &'static str,
        worker_id: WorkerId,
        secret: &str,
        allow_nonzero_exit: bool,
    ) -> TestResult<Self> {
        let bin = provider_binary(name)?;
        let mut child = tokio::process::Command::new(&bin)
            .env("VOOM_WORKER_SECRET", secret)
            .env("VOOM_WORKER_ID", worker_id.0.to_string())
            .env("VOOM_WORKER_EPOCH", "0")
            .env("VOOM_WORKER_BIND", "127.0.0.1:0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take();
        let credentials = WorkerCredentials {
            worker_id,
            worker_epoch: 0,
            secret: SecretString::from(secret.to_owned()),
        };
        let bound = match read_bound_addr(&mut child, name).await {
            Ok(bound) => bound,
            Err(err) => {
                let mut launch = Self {
                    child,
                    stdin,
                    bound: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    credentials,
                    name,
                    allow_nonzero_exit,
                };
                return combine_result_and_cleanup(Err(err), launch.terminate().await);
            }
        };
        Ok(Self {
            child,
            stdin,
            bound,
            credentials,
            name,
            allow_nonzero_exit,
        })
    }

    async fn shutdown(&mut self) -> TestResult<()> {
        drop(self.stdin.take());
        let status = if let Ok(status) =
            tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await
        {
            status?
        } else {
            self.terminate().await?;
            return Err(io_error(format!("{} cleanup timed out", self.name)));
        };
        if !status.success() && !self.allow_nonzero_exit {
            return Err(io_error(format!("{} exited with {status}", self.name)));
        }
        Ok(())
    }

    async fn terminate(&mut self) -> TestResult<()> {
        drop(self.stdin.take());
        let _ = self.child.start_kill();
        tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await??;
        Ok(())
    }
}

#[derive(Debug)]
struct InProcessFakeProvider {
    definition: voom_fake_support::ProviderDefinition,
}

impl InProcessFakeProvider {
    fn new(name: &'static str) -> TestResult<Self> {
        let definition = voom_fake_support::provider_definition(name)
            .ok_or_else(|| io_error(format!("unknown fake provider {name}")))?;
        Ok(Self { definition })
    }
}

#[async_trait::async_trait]
impl ClientHandle for InProcessFakeProvider {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        let duration_ms = request
            .payload
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut immediate_request = request;
        if let Some(payload) = immediate_request.payload.as_object_mut() {
            payload.insert("duration_ms".to_owned(), json!(0));
            payload.insert("progress_interval_ms".to_owned(), json!(0));
        }
        let dispatch = voom_fake_support::dispatch_provider(&self.definition, &immediate_request)?;
        let OperationBody::Buffered(body) = dispatch.body else {
            return Err(ProtocolError::InternalServerError);
        };
        let expected_lease_id = dispatch.response.lease_id;
        let (mut writer, reader) = tokio::io::duplex(16 * 1024);
        tokio::spawn(async move {
            if duration_ms > 0 {
                tokio::time::sleep(Duration::from_millis(duration_ms)).await;
            }
            let _ = writer.write_all(&body).await;
        });
        Ok(DispatchStream {
            response: dispatch.response,
            frames: NdjsonReader::new(Box::pin(reader), expected_lease_id),
        })
    }
}

/// In-process runtime whose dispatch never returns, modelling a worker that
/// accepted the lease but produced no response. The executor's dispatch timeout
/// fires and maps it to `WorkerTimeout` without depending on a real socket or
/// wall-clock latency.
#[derive(Debug)]
struct UnreachableInProcessProvider;

#[async_trait::async_trait]
impl ClientHandle for UnreachableInProcessProvider {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        _request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        std::future::pending().await
    }
}

impl Drop for ProviderLaunch {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn read_bound_addr(child: &mut Child, name: &str) -> TestResult<std::net::SocketAddr> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io_error(format!("{name} stdout missing")))?;
    let mut lines = BufReader::new(stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await??
        .ok_or_else(|| io_error(format!("{name} exited before bind line")))?;
    Ok(line
        .strip_prefix("BOUND addr=")
        .ok_or_else(|| io_error(format!("malformed {name} bind line: {line}")))?
        .parse::<std::net::SocketAddr>()?)
}

fn provider_binary(name: &str) -> TestResult<PathBuf> {
    let env_name = format!("CARGO_BIN_EXE_{name}");
    if let Some(path) = std::env::var_os(env_name) {
        return Ok(PathBuf::from(path));
    }
    ensure_fake_provider_bins_built()?;
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(default_target_dir, target_dir_from_env);
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    Ok(debug_dir(&target_dir).join(format!("{name}{suffix}")))
}

fn ensure_fake_provider_bins_built() -> TestResult<()> {
    static BUILD: OnceLock<Result<(), String>> = OnceLock::new();
    BUILD
        .get_or_init(|| {
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "voom-fakes", "--bins"])
                .current_dir(workspace_root())
                .status()
                .map_err(|e| format!("fake provider build failed to start: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("fake provider build exited with {status}"))
            }
        })
        .clone()
        .map_err(io_error)
}

fn target_dir_from_env(path: std::ffi::OsString) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        workspace_root().join(path)
    }
}

fn debug_dir(target_dir: &Path) -> PathBuf {
    if let Some(target) = std::env::var_os("CARGO_BUILD_TARGET").filter(|target| !target.is_empty())
    {
        target_dir.join(target).join("debug")
    } else {
        target_dir.join("debug")
    }
}

fn default_target_dir() -> PathBuf {
    workspace_root().join("target")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

async fn connect_single_connection_pool(url: &str) -> TestResult<SqlitePool> {
    let mut options: SqliteConnectOptions = url.parse()?;
    options = options
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .disable_statement_logging();
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn failure_class_name(class: FailureClass) -> String {
    serde_json::to_value(class)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn io_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message.into()))
}

fn expect(label: &str, condition: bool) -> TestResult<()> {
    if condition {
        Ok(())
    } else {
        Err(io_error(label.to_owned()))
    }
}

fn expect_eq<T>(label: &str, actual: &T, expected: &T) -> TestResult<()>
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        Ok(())
    } else {
        Err(io_error(format!(
            "{label}: expected {expected:?}, got {actual:?}"
        )))
    }
}

fn combine_result_and_cleanup<T>(result: TestResult<T>, cleanup: TestResult<()>) -> TestResult<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(err), Ok(())) | (Ok(_), Err(err)) => Err(err),
        (Err(err), Err(cleanup_err)) => Err(io_error(format!(
            "{err}; provider cleanup failed: {cleanup_err}"
        ))),
    }
}
