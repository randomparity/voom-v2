#![expect(
    clippy::unwrap_used,
    clippy::panic_in_result_fn,
    reason = "integration tests fail fast on unexpected durable state"
)]

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
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
use voom_core::{
    ErrorCode, FailureClass, FileLocationId, JobId, SystemClock, TicketOperation, WorkerId,
    WorkerKind,
};
use voom_store::repo::execution::workers::{NewCapability, NewGrant};
use voom_worker_protocol::http::OperationBody;
use voom_worker_protocol::{
    ClientHandle, DispatchStream, HandshakeResponse, HttpClient, NdjsonReader, OperationKind,
    OperationRequest, OperationResponse, ProgressFrame, ProtocolError, WorkerCredentials,
};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
static PROCESS_PROVIDER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

async fn process_provider_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    PROCESS_PROVIDER_TEST_LOCK.lock().await
}

#[test]
fn provider_binary_uses_active_test_profile() -> TestResult<()> {
    let actual = provider_binary("fake-scanner")?;
    let expected = std::env::var_os("CARGO_BIN_EXE_fake-scanner").map_or_else(
        || voom_test_support::worker::target_debug_binary("fake-scanner"),
        PathBuf::from,
    );

    expect_eq("provider binary path", &actual, &expected)
}

#[tokio::test]
async fn default_ci_workflow_runs_all_branches_through_real_scheduler() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_all_fake_providers().await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .map_err(|err| io_error(format!("workflow failed: {:?}", err.source)))?;

        expect_eq("branch_count", &summary.branch_count, &3)?;
        // Only generic operations mint leases now: the 12 envelope-family
        // tickets (3 probes, 2 remux + 1 transcode, 3 backups, 3 verifies)
        // are executed by their storage owner's agent without a lease.
        expect_eq("dispatch_count", &summary.dispatch_count, &19)?;
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
// watchdog. WorkerCrash / MalformedResult / missed-heartbeat run the in-house
// `chaos-worker` fake out-of-process so the crash and stall modes have real-process
// fidelity. Dispatch timeout and progress timeout use deterministic in-process
// fault boundaries: one never returns from dispatch, and one emits a progress
// frame followed by a typed ProgressTimeout terminal signal. The latter preserves
// lease-heartbeat evidence without depending on a short wall-clock deadline.
//
// The fault target is HashFile, not ProbeFile: probe is an envelope-family
// operation that routes to its storage owner's agent and never leases a
// worker, so its failure classes come from agent settlement, not from the
// worker-dispatch machinery these tests pin. HashFile still traverses that
// machinery. The emulated media chain runs alongside so the workflow fails
// only on the injected fault.
#[tokio::test]
async fn chaos_worker_crash_maps_to_worker_crash() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override(
        OperationKind::HashFile,
        ChaosWorkerMode::Crash,
    )
    .await?;
    fixture.assert_watchdog_budget_is_generous()?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::WorkerCrash,
            )
            .await?;
        fixture
            .assert_no_success_for_operation(summary.job_id, OperationKind::HashFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::HashFile,
            FailureClass::WorkerCrash,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_dispatch_timeout_maps_to_worker_timeout() -> TestResult<()> {
    let mut fixture =
        DurableWorkflowFixture::start_with_unreachable_runtime_override(OperationKind::HashFile)
            .await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::WorkerTimeout,
            )
            .await?;
        fixture
            .assert_no_terminal_frame_accepted(summary.job_id, OperationKind::HashFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::HashFile,
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
        OperationKind::HashFile,
        ChaosWorkerMode::MalformedResult,
    )
    .await?;
    fixture.assert_watchdog_budget_is_generous()?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::MalformedWorkerResult,
            )
            .await?;
        fixture
            .assert_no_failure_class(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::WorkerCrash,
            )
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::HashFile,
            FailureClass::MalformedWorkerResult,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_progress_timeout_maps_to_progress_timeout() -> TestResult<()> {
    let mut fixture =
        DurableWorkflowFixture::start_with_progress_timeout_signal(OperationKind::HashFile).await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::ProgressTimeout,
            )
            .await?;
        fixture
            .assert_heartbeat_events_exist(summary.job_id, OperationKind::HashFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::HashFile,
            FailureClass::ProgressTimeout,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn chaos_missed_heartbeat_uses_executor_watchdog() -> TestResult<()> {
    let _process_provider_guard = process_provider_test_guard().await;
    let chaos = WorkflowChaosOptions::suppress_heartbeats_for_operation(OperationKind::HashFile);
    let mut fixture = DurableWorkflowFixture::start_with_chaos_override_and_options(
        OperationKind::HashFile,
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
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
            .await
            .unwrap_err()
            .summary;

        fixture
            .assert_ticket_failed_with(
                summary.job_id,
                OperationKind::HashFile,
                FailureClass::WorkerTimeout,
            )
            .await?;
        fixture
            .assert_no_expire_due_path(summary.job_id, OperationKind::HashFile)
            .await?;
        fixture
            .assert_no_progress_triggered_heartbeat(summary.job_id, OperationKind::HashFile)
            .await?;
        fixture
            .assert_no_terminal_frame_accepted(summary.job_id, OperationKind::HashFile)
            .await?;
        fixture
            .assert_no_malformed_frame(summary.job_id, OperationKind::HashFile)
            .await?;
        DurableWorkflowFixture::assert_failure_summary(
            &summary,
            OperationKind::HashFile,
            FailureClass::WorkerTimeout,
        )
    }
    .await;

    combine_result_and_cleanup(result, fixture.shutdown().await)
}

#[tokio::test]
async fn benchmark_durable_workflow_reports_non_zero_throughput() -> TestResult<()> {
    // In-process fake providers, not process ones: the benchmark verifies the
    // summary math (non-zero throughput over populated dispatch/success
    // counts), not process dispatch overhead. Out-of-process workers let a
    // loaded runner corrupt a frame or lose a spawn and fail the whole
    // workflow with MalformedWorkerResult — the same flake the unreachable
    // runtime fixture below documents and avoids this way.
    let mut fixture = DurableWorkflowFixture::start_all_in_process_fake_providers(1).await?;
    let result = async {
        let summary = fixture
            .executor()
            .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
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
        let mut plan = WorkflowPlan::default_ci(fixture.source_location_id);
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
        .submit_and_run(WorkflowPlan::default_ci(fixture.source_location_id))
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

struct DurableWorkflowFixture {
    cp: ControlPlane,
    pool: SqlitePool,
    source_location_id: FileLocationId,
    _tmp: voom_test_support::TempDatabase,
    registry: WorkerRuntimeRegistry,
    launches: Vec<ProviderLaunch>,
    registered_workers: Vec<(WorkerId, u32)>,
    executor_options: WorkflowExecutorOptions,
    deadline_fixture: Option<DeadlineFixture>,
    owner_node_emulator: Option<tokio::task::JoinHandle<()>>,
}

impl DurableWorkflowFixture {
    async fn start_all_fake_providers() -> TestResult<Self> {
        Self::start_all_fake_providers_with_max_parallel(1).await
    }

    async fn start_all_fake_providers_with_max_parallel(max_parallel: u32) -> TestResult<Self> {
        let mut fixture = Self::without_fake_providers().await?;
        fixture.enable_owner_node_emulation().await?;
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
        fixture.enable_owner_node_emulation().await?;
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
        // Generous watchdog budgets, for the same reason
        // `start_with_unreachable_runtime_override` carries them: the healthy
        // branches here run out-of-process, and the watchdog does not
        // distinguish a worker that is misbehaving from one the runner has not
        // scheduled yet. Both fault modes this fixture serves are immediate and
        // deterministic -- Crash exits the process, MalformedResult answers 200
        // with an unparseable body -- so neither depends on a deadline, and the
        // sub-second budgets that used to be here only raced the healthy
        // branches. Under them a loaded runner reaped `scan_library` first, and
        // the workflow then aborted before the fault branch ever dispatched
        // (issue #541).
        let mut options = WorkflowExecutorOptions::for_tests();
        options.timing.heartbeat_interval = Duration::from_millis(20);
        options.timing.heartbeat_timeout = Duration::from_secs(2);
        options.timing.progress_idle_timeout = Duration::from_secs(2);
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
        fixture.enable_owner_node_emulation().await?;
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
        fixture.enable_owner_node_emulation().await?;
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

    async fn start_with_progress_timeout_signal(operation: OperationKind) -> TestResult<Self> {
        let mut fixture = Self::without_fake_providers().await?;
        fixture.enable_owner_node_emulation().await?;
        let setup = async {
            fixture
                .register_in_process_providers_except(operation, 4)
                .await?;
            fixture.register_progress_timeout_runtime(operation).await
        }
        .await;
        if let Err(err) = setup {
            return combine_result_and_cleanup(Err(err), fixture.shutdown().await);
        }
        Ok(fixture)
    }

    async fn without_fake_providers() -> TestResult<Self> {
        let tmp = voom_test_support::TempDatabase::new()?;
        let url = format!("sqlite://{}", tmp.path().display());
        voom_store::init(&url).await?;
        let pool = connect_single_connection_pool(&url).await?;
        let cp = ControlPlane::open_with_pool_and_rng(
            pool.clone(),
            Arc::new(SystemClock),
            Arc::new(Mutex::new(FrozenRng::new(0))),
        )
        .await?;
        // Byte-touching plan nodes must name a live rooted location.
        let source_location_id = voom_store::test_support::seed_test_rooted_location(&pool).await?;
        Ok(Self {
            cp,
            pool,
            source_location_id,
            _tmp: tmp,
            registry: WorkerRuntimeRegistry::new(),
            launches: Vec::new(),
            registered_workers: Vec::new(),
            executor_options: WorkflowExecutorOptions::for_tests(),
            deadline_fixture: None,
            owner_node_emulator: None,
        })
    }

    /// Seed the durable inputs envelope-bearing media tickets resolve against
    /// (the fake scanner's synthetic location band, per-version snapshots,
    /// staging/backup defaults) and start the owner-node emulator that
    /// settles them.
    ///
    /// ADR 0075: media tickets are never leased by the executor — they wait
    /// ready for their storage owner's agent. These drivers have no real
    /// agent, so a task stands in for one: it polls ready node-local tickets
    /// and completes them with the success results their downstream
    /// expansions consume. Chaos drivers keep this on too: the emulated media
    /// chain must succeed so the workflow's failure comes from the fault the
    /// test injects into a generic operation.
    async fn enable_owner_node_emulation(&mut self) -> TestResult<()> {
        let seeded = voom_store::test_support::seed_synthetic_rooted_locations(
            &self.pool,
            voom_fake_support::FAKE_SCANNER_FIRST_LOCATION_ID,
            3,
        )
        .await?;
        sqlx::query(
            "UPDATE library_roots SET default_staging_root_id = id, \
             default_backup_root_id = id WHERE id = ?",
        )
        .bind(i64::try_from(
            voom_store::test_support::TEST_STORAGE_ROOT_ID.0,
        )?)
        .execute(&self.pool)
        .await?;
        for (version_id, _) in &seeded {
            self.cp
                .record_media_snapshot(*version_id, None, fixture_media_snapshot(), T0)
                .await?;
        }
        self.owner_node_emulator = Some(tokio::spawn(run_owner_node_emulator(self.pool.clone())));
        Ok(())
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
        let secret = "durable-workflow-chaos-secret";
        let worker = self
            .register_worker_without_runtime("chaos-worker", &[operation], 1, secret)
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

    async fn register_progress_timeout_runtime(
        &mut self,
        operation: OperationKind,
    ) -> TestResult<()> {
        let secret = "durable-workflow-progress-timeout-secret";
        let worker = self
            .register_worker_without_runtime("progress-timeout-probe", &[operation], 1, secret)
            .await?;
        self.registered_workers.push((worker, 1));
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(ProgressTimeoutInProcessProvider),
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
            .register_worker(crate::cases::workers::RegisterWorkerInput {
                name: name.to_owned(),
                kind: WorkerKind::Synthetic,
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
        if count == 0 {
            // Report what the job actually recorded. Without this the message
            // names only the expectation, so a CI-only occurrence says nothing
            // about which class won and forces a fresh investigation.
            return Err(io_error(format!(
                "expected failed {operation:?} ticket with class {}; job recorded {}",
                failure_class_name(class),
                self.describe_ticket_outcomes(job_id).await?
            )));
        }
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

    /// Every ticket in the job as `operation=state[class,...]`, for a failed
    /// class assertion to quote. A wrong class and a branch that never reached
    /// the fault produce the same bare count, so the message has to carry both
    /// the states and the classes to tell them apart.
    async fn describe_ticket_outcomes(&self, job_id: JobId) -> TestResult<String> {
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT json_extract(t.payload, '$.operation'), t.state, \
                    group_concat(DISTINCT json_extract(e.payload, '$.class')) \
             FROM tickets t \
             LEFT JOIN events e \
               ON e.subject_type = 'ticket' AND e.subject_id = t.id \
              AND e.kind IN ('ticket.failed_terminal', 'ticket.failed_retriable') \
             WHERE t.job_id = ? \
             GROUP BY t.id \
             ORDER BY t.id",
        )
        .bind(i64::try_from(job_id.0)?)
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok("no tickets".to_owned());
        }
        Ok(rows
            .iter()
            .map(|(operation, state, classes)| {
                classes.as_ref().map_or_else(
                    || format!("{operation}={state}"),
                    |classes| format!("{operation}={state}[{classes}]"),
                )
            })
            .collect::<Vec<_>>()
            .join(", "))
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

    /// Guard for the fixtures whose fault is immediate and whose healthy
    /// branches run out-of-process. Their watchdog exists only as an outer
    /// bound, so re-tightening it buys nothing and reintroduces issue #541.
    /// One second is a floor against that edit, not a measured cliff -- the
    /// observed failure ran under 500ms, and how much slack a shared runner
    /// needs is not a number this suite can pin.
    ///
    /// Deliberately not called from the fixture constructor:
    /// `chaos_missed_heartbeat_uses_executor_watchdog` shares that constructor
    /// and sets a tight heartbeat deadline on purpose, because there the
    /// watchdog is the behaviour under test.
    fn assert_watchdog_budget_is_generous(&self) -> TestResult<()> {
        let floor = Duration::from_secs(1);
        expect(
            "heartbeat timeout should not race the healthy out-of-process branches",
            self.executor_options.timing.heartbeat_timeout >= floor,
        )?;
        expect(
            "progress idle timeout should not race the healthy out-of-process branches",
            self.executor_options.timing.progress_idle_timeout >= floor,
        )
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
        if let Some(handle) = self.owner_node_emulator.take() {
            handle.abort();
        }
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
    Stall,
}

impl ChaosWorkerMode {
    fn payload_mode(self) -> &'static str {
        match self {
            Self::Crash => "crash",
            Self::MalformedResult => "malformed_result",
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
    // Only generic operations register worker runtimes: the envelope family
    // (probe, transcode/remux/audio, backup, verify) routes to its storage
    // owner's agent — emulated here — and never leases a worker.
    vec![
        ProviderSpec {
            name: "fake-scanner",
            operations: &[OperationKind::ScanLibrary],
        },
        ProviderSpec {
            name: "fake-prober",
            operations: &[OperationKind::HashFile],
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

    async fn identity(
        &self,
        _credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, ProtocolError> {
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

    async fn identity(
        &self,
        _credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, ProtocolError> {
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

#[derive(Debug)]
struct ProgressTimeoutInProcessProvider;

#[async_trait::async_trait]
impl ClientHandle for ProgressTimeoutInProcessProvider {
    async fn handshake(&self, _offered: u32) -> Result<HandshakeResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn identity(
        &self,
        _credentials: &WorkerCredentials,
    ) -> Result<voom_worker_protocol::WorkerIdentityResponse, ProtocolError> {
        Err(ProtocolError::InternalServerError)
    }

    async fn dispatch(
        &self,
        _creds: &WorkerCredentials,
        _idempotency_key: &str,
        request: OperationRequest,
    ) -> Result<DispatchStream, ProtocolError> {
        let lease_id = request.lease_id;
        let frames = [
            ProgressFrame::Progress {
                lease_id,
                seq: 0,
                emitted_at: T0,
                percent: None,
                message: Some("accepted".to_owned()),
                payload: None,
            },
            ProgressFrame::Error {
                lease_id,
                seq: 1,
                emitted_at: T0,
                class: FailureClass::ProgressTimeout,
                code: ErrorCode::WorkerTimeout,
                message: "deterministic progress-timeout signal".to_owned(),
                payload: None,
            },
        ];
        let mut body = Vec::new();
        for frame in frames {
            body.extend_from_slice(&serde_json::to_vec(&frame).map_err(|err| {
                ProtocolError::MalformedFrame {
                    detail: format!("encode deterministic timeout frame: {err}"),
                }
            })?);
            body.push(b'\n');
        }
        let (mut writer, reader) = tokio::io::duplex(16 * 1024);
        tokio::spawn(async move {
            let _ = writer.write_all(&body).await;
        });
        Ok(DispatchStream {
            response: OperationResponse {
                lease_id,
                accepted_at: T0,
            },
            frames: NdjsonReader::new(Box::pin(reader), lease_id),
        })
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
    voom_test_support::worker::cargo_bin_or_build("voom-fakes", name)
        .map_err(|err| io_error(format!("fake provider binary {name}: {err}")))
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

/// The media snapshot every synthetic fixture version carries: enough for a
/// remux child's selection to derive from the recorded streams.
fn fixture_media_snapshot() -> Value {
    serde_json::json!({
        "container": "mkv",
        "video_codec": "h264",
        "streams": [
            {
                "id": "stream-0",
                "index": 0,
                "kind": "video",
                "codec_name": "h264",
                "disposition": {"default": true}
            },
            {
                "id": "stream-audio-1",
                "index": 1,
                "kind": "audio",
                "codec_name": "aac",
                "language": "eng",
                "channels": 2,
                "disposition": {"default": false, "forced": false}
            }
        ]
    })
}

/// Emulate the storage owner's agent for envelope-bearing media tickets
/// (ADR 0075): poll ready node-local tickets and settle them with the success
/// results their downstream expansions consume.
///
/// The executor never leases these tickets — they wait `ready` for their
/// storage owner's agent — so without something answering, a workflow holding
/// them waits in the externally-held idle state forever. Settlement mirrors
/// what an owner node reports: probe echoes its codec, transforms name their
/// planned output, backups release the observed output facts their verify
/// child pins as expectations.
async fn run_owner_node_emulator(pool: SqlitePool) {
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    loop {
        tick.tick().await;
        let Ok(outstanding) = sqlx::query_as::<
            _,
            (
                i64,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT id, \
             json_extract(payload, '$.operation'), \
             json_extract(payload, '$.rendered_payload.media_dispatch.output.provider_relative_locator'), \
             json_extract(payload, '$.rendered_payload.codec') \
             FROM tickets \
             WHERE state = 'ready' \
               AND json_extract(payload, '$.rendered_payload.media_dispatch') IS NOT NULL",
        )
        .fetch_all(&pool)
        .await else {
            continue;
        };
        for (ticket_id, operation, output_locator, codec) in outstanding {
            let result = match operation.as_deref() {
                Some("probe_file") => serde_json::json!({
                    "codec": codec.as_deref().unwrap_or("h264"),
                }),
                Some("remux" | "transcode_video") => serde_json::json!({
                    "output_path": output_locator
                        .clone()
                        .unwrap_or_else(|| "/staging/emulated.mkv".to_owned()),
                }),
                Some("back_up_file") => serde_json::json!({
                    "local_backup_id": format!("backup-{ticket_id}"),
                    "agent_observed": {
                        "outputs": [{
                            "facts": {
                                "size_bytes": 1024_u64,
                                "content_hash": "blake3:owner-node-emulator",
                            },
                        }]
                    },
                }),
                _ => serde_json::json!({}),
            };
            let _ = sqlx::query(
                "UPDATE tickets SET state = 'succeeded', result = ?, \
                 state_changed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), epoch = epoch + 1 \
                 WHERE id = ? AND state = 'ready'",
            )
            .bind(result.to_string())
            .bind(ticket_id)
            .execute(&pool)
            .await;
        }
    }
}
