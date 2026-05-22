#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    reason = "integration tests fail fast on unexpected durable state"
)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{ConnectOptions, Row, SqlitePool};
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdin};
use voom_control_plane::ControlPlane;
use voom_control_plane::workflow::executor::WorkflowExecutorOptions;
use voom_control_plane::workflow::expansion::{
    ExpansionContext, expand_backup_completion, expand_probe_completion, expand_quality_completion,
    expand_scanner_completion, expand_transform_completion,
};
use voom_control_plane::workflow::ticket_payload::WorkflowTicketPayload;
use voom_control_plane::workflow::{WorkerRuntimeRegistry, WorkflowExecutor, WorkflowPlan};
use voom_core::rng_test_support::FrozenRng;
use voom_core::{ErrorCode, JobId, SystemClock, TicketId, WorkerId};
use voom_scheduler::SingleWorkerPerKindSelector;
use voom_store::repo::jobs::NewJob;
use voom_store::repo::tickets::{NewTicket, Ticket, TicketState};
use voom_store::repo::workers::{NewCapability, NewGrant, NewWorker, WorkerKind};
use voom_worker_protocol::{HttpClient, OperationKind, WorkerCredentials};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

type TestResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
async fn default_ci_workflow_runs_all_branches_through_real_scheduler() -> TestResult<()> {
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

#[tokio::test]
async fn restart_scanner_expansion_promotes_late_branch_tickets_once() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let scanner = fixture
        .seed_succeeded_ticket(
            "scan",
            "root",
            OperationKind::ScanLibrary,
            scanner_result_with_three_files(),
        )
        .await?;

    let created = expand_scanner_completion(&fixture.expansion_context(), &scanner).await?;
    let second = expand_scanner_completion(&fixture.expansion_context(), &scanner).await?;

    assert_eq!(created.len(), 9);
    assert!(second.is_empty());
    fixture.assert_ready_tickets(&created).await?;
    fixture
        .assert_unique_branch_tickets(&["probe", "hash", "identity"])
        .await?;
    fixture.assert_event_count("lease.released", 0).await?;
    Ok(())
}

#[tokio::test]
async fn restart_probe_expansion_promotes_late_quality_ticket_once() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let probe = fixture
        .seed_succeeded_ticket(
            "probe",
            "file-001",
            OperationKind::ProbeFile,
            json!({"codec": "h264"}),
        )
        .await?;

    let created = expand_probe_completion(&fixture.expansion_context(), "file-001", &probe).await?;
    let second = expand_probe_completion(&fixture.expansion_context(), "file-001", &probe).await?;

    assert_eq!(node_ids(&created), vec!["quality"]);
    assert!(second.is_empty());
    fixture.assert_ready_tickets(&created).await?;
    fixture.assert_unique_branch_tickets(&["quality"]).await?;
    fixture.assert_event_count("lease.released", 0).await?;
    Ok(())
}

#[tokio::test]
async fn restart_quality_expansion_promotes_selected_transform_once() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let transcode_parent = fixture
        .seed_succeeded_ticket(
            "quality",
            "file-000",
            OperationKind::ScoreQuality,
            json!({"needs_transcode": true}),
        )
        .await?;
    let remux_parent = fixture
        .seed_succeeded_ticket(
            "quality",
            "file-001",
            OperationKind::ScoreQuality,
            json!({"needs_transcode": false}),
        )
        .await?;

    let transcode_created =
        expand_quality_completion(&fixture.expansion_context(), "file-000", &transcode_parent)
            .await?;
    let transcode_second =
        expand_quality_completion(&fixture.expansion_context(), "file-000", &transcode_parent)
            .await?;
    let remux_created =
        expand_quality_completion(&fixture.expansion_context(), "file-001", &remux_parent).await?;
    let remux_second =
        expand_quality_completion(&fixture.expansion_context(), "file-001", &remux_parent).await?;

    assert_eq!(node_ids(&transcode_created), vec!["transcode"]);
    assert!(transcode_second.is_empty());
    assert_eq!(node_ids(&remux_created), vec!["remux"]);
    assert!(remux_second.is_empty());
    fixture.assert_ready_tickets(&transcode_created).await?;
    fixture.assert_ready_tickets(&remux_created).await?;
    fixture
        .assert_unique_branch_tickets(&["transcode", "remux"])
        .await?;
    fixture.assert_event_count("lease.released", 0).await?;
    Ok(())
}

#[tokio::test]
async fn restart_transform_expansion_promotes_downstream_branch_tickets_once() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let transform = fixture
        .seed_succeeded_ticket(
            "transcode",
            "file-001",
            OperationKind::TranscodeVideo,
            json!({"output_path": "/staging/file-001.h265.mkv"}),
        )
        .await?;

    let created =
        expand_transform_completion(&fixture.expansion_context(), "file-001", &transform).await?;
    let second =
        expand_transform_completion(&fixture.expansion_context(), "file-001", &transform).await?;

    assert_eq!(
        node_ids(&created),
        vec!["backup", "external-sync", "issue", "use-lease"]
    );
    assert!(second.is_empty());
    fixture.assert_ready_tickets(&created).await?;
    fixture
        .assert_unique_branch_tickets(&["backup", "external-sync", "issue", "use-lease"])
        .await?;
    fixture.assert_event_count("lease.released", 0).await?;
    Ok(())
}

#[tokio::test]
async fn restart_backup_expansion_promotes_late_verify_ticket_once() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let backup = fixture
        .seed_succeeded_ticket(
            "backup",
            "file-001",
            OperationKind::BackUpFile,
            json!({"local_backup_id": "backup-local-001"}),
        )
        .await?;

    let created =
        expand_backup_completion(&fixture.expansion_context(), "file-001", &backup).await?;
    let second =
        expand_backup_completion(&fixture.expansion_context(), "file-001", &backup).await?;

    assert_eq!(node_ids(&created), vec!["verify"]);
    assert!(second.is_empty());
    fixture.assert_ready_tickets(&created).await?;
    fixture.assert_unique_branch_tickets(&["verify"]).await?;
    fixture.assert_event_count("lease.released", 0).await?;
    Ok(())
}

#[tokio::test]
async fn pre_lease_no_worker_retries_then_terminal_fails_without_dispatch() -> TestResult<()> {
    let fixture = DurableWorkflowFixture::without_fake_providers().await?;
    let mut options = WorkflowExecutorOptions::for_tests();
    options.max_attempts = 2;

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
    options.max_attempts = 2;

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
    plan: WorkflowPlan,
    workflow_id: String,
    plan_id: String,
    job_id: JobId,
    registry: WorkerRuntimeRegistry,
    launches: Vec<ProviderLaunch>,
}

impl DurableWorkflowFixture {
    async fn start_all_fake_providers() -> TestResult<Self> {
        let mut fixture = Self::without_fake_providers().await?;
        for provider in provider_specs() {
            if let Err(err) = fixture.register_process_provider(provider).await {
                return combine_result_and_cleanup(Err(err), fixture.shutdown().await);
            }
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
        let plan = WorkflowPlan::default_ci();
        let job = cp
            .open_job(NewJob {
                kind: "synthetic.workflow.restart".to_owned(),
                priority: 0,
                created_at: T0,
            })
            .await?;

        Ok(Self {
            cp,
            pool,
            _tmp: tmp,
            plan_id: plan.id.clone(),
            plan,
            workflow_id: "restart-workflow".to_owned(),
            job_id: job.id,
            registry: WorkerRuntimeRegistry::new(),
            launches: Vec::new(),
        })
    }

    fn executor(&self) -> WorkflowExecutor<SingleWorkerPerKindSelector> {
        self.executor_with_options(WorkflowExecutorOptions::for_tests())
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

    fn expansion_context(&self) -> ExpansionContext<'_> {
        ExpansionContext::new(
            &self.cp,
            &self.plan,
            &self.workflow_id,
            &self.plan_id,
            self.job_id,
            T0,
        )
    }

    async fn register_process_provider(&mut self, provider: ProviderSpec) -> TestResult<()> {
        let secret = format!("durable-workflow-{}-secret", provider.name);
        let worker = self
            .register_worker_without_runtime(provider.name, provider.operations, 4, &secret)
            .await?;
        let launch = ProviderLaunch::spawn(provider, worker, &secret).await?;
        self.registry.register_in_process_runtime(
            worker,
            Arc::new(HttpClient::new(launch.bound)),
            launch.credentials.clone(),
        );
        self.launches.push(launch);
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
            })
            .await?;
        let operation_names: Vec<String> = operations.iter().copied().map(operation_name).collect();
        for operation in &operation_names {
            self.cp
                .record_capability(NewCapability {
                    worker_id: worker.id,
                    operation: operation.clone(),
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
                can_execute: operation_names,
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: Vec::new(),
                max_parallel: Value::Object(max_parallel_by_operation),
            })
            .await?;
        Ok(worker.id)
    }

    async fn seed_succeeded_ticket(
        &self,
        node_id: &str,
        branch_id: &str,
        operation: OperationKind,
        result: Value,
    ) -> TestResult<Ticket> {
        let rendered_payload = rendered_payload_for_seed(operation, branch_id)?;
        let payload = WorkflowTicketPayload {
            workflow_id: self.workflow_id.clone(),
            plan_id: self.plan_id.clone(),
            node_id: node_id.to_owned(),
            branch_id: branch_id.to_owned(),
            operation,
            rendered_payload,
            timing: voom_control_plane::workflow::timing::EffectiveTiming::for_test(25, 10),
            source_file: Some(json!({
                "path": format!("/library/{branch_id}.mkv"),
                "size_bytes": 4_200_000_000_u64,
            })),
        }
        .to_ticket_payload()?;
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: Some(self.job_id),
                kind: ticket_kind(operation),
                priority: 0,
                payload,
                max_attempts: 1,
                created_at: T0,
            })
            .await?;
        self.set_ticket_succeeded(ticket.id, result).await?;
        self.ticket(ticket.id).await
    }

    async fn set_ticket_succeeded(&self, ticket_id: TicketId, result: Value) -> TestResult<()> {
        sqlx::query(
            "UPDATE tickets SET state = 'succeeded', result = ?, state_changed_at = ?, \
             epoch = epoch + 1 WHERE id = ?",
        )
        .bind(serde_json::to_string(&result)?)
        .bind(format_time(T0)?)
        .bind(i64::try_from(ticket_id.0)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn ticket(&self, ticket_id: TicketId) -> TestResult<Ticket> {
        let row = sqlx::query(
            "SELECT id, kind, state, payload, result, attempt, max_attempts, priority \
             FROM tickets WHERE id = ?",
        )
        .bind(i64::try_from(ticket_id.0)?)
        .fetch_one(&self.pool)
        .await?;
        Ok(Ticket {
            id: TicketId(u64::try_from(row.get::<i64, _>("id"))?),
            job_id: Some(self.job_id),
            kind: row.get("kind"),
            state: TicketState::parse_for_test(row.get::<String, _>("state").as_str()),
            priority: row.get("priority"),
            payload: serde_json::from_str(&row.get::<String, _>("payload"))?,
            result: row
                .get::<Option<String>, _>("result")
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
            attempt: u32::try_from(row.get::<i64, _>("attempt"))?,
            max_attempts: u32::try_from(row.get::<i64, _>("max_attempts"))?,
            next_eligible_at: T0,
            created_at: T0,
            state_changed_at: T0,
            epoch: 0,
        })
    }

    async fn assert_ready_tickets(&self, tickets: &[Ticket]) -> TestResult<()> {
        for ticket in tickets {
            let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = ?")
                .bind(i64::try_from(ticket.id.0)?)
                .fetch_one(&self.pool)
                .await?;
            assert_eq!(state, "ready", "ticket {} was not ready", ticket.id.0);
        }
        Ok(())
    }

    async fn assert_unique_branch_tickets(&self, node_ids: &[&str]) -> TestResult<()> {
        for node_id in node_ids {
            let duplicates: Vec<(String, i64)> = sqlx::query_as(
                "SELECT json_extract(payload, '$.branch_id') AS branch_id, COUNT(*) AS count \
                 FROM tickets \
                 WHERE job_id = ? AND json_extract(payload, '$.node_id') = ? \
                 GROUP BY branch_id HAVING count > 1",
            )
            .bind(i64::try_from(self.job_id.0)?)
            .bind(node_id)
            .fetch_all(&self.pool)
            .await?;
            assert!(
                duplicates.is_empty(),
                "duplicate workflow tickets for node {node_id}: {duplicates:?}"
            );
        }
        Ok(())
    }

    async fn assert_event_count(&self, kind: &str, expected: i64) -> TestResult<()> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = ?")
            .bind(kind)
            .fetch_one(&self.pool)
            .await?;
        assert_eq!(count, expected, "event count for {kind}");
        Ok(())
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
}

impl ProviderLaunch {
    async fn spawn(provider: ProviderSpec, worker_id: WorkerId, secret: &str) -> TestResult<Self> {
        let bin = provider_binary(provider.name)?;
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
        let bound = match read_bound_addr(&mut child, provider.name).await {
            Ok(bound) => bound,
            Err(err) => {
                let mut launch = Self {
                    child,
                    stdin,
                    bound: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                    credentials,
                    name: provider.name,
                };
                return combine_result_and_cleanup(Err(err), launch.terminate().await);
            }
        };
        Ok(Self {
            child,
            stdin,
            bound,
            credentials,
            name: provider.name,
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
        if !status.success() {
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

fn rendered_payload_for_seed(operation: OperationKind, branch_id: &str) -> TestResult<Value> {
    let branch = voom_control_plane::workflow::binding::BranchContext {
        branch_id: branch_id.to_owned(),
        path: format!("/library/{branch_id}.mkv"),
        probe_codec: Some("h264".to_owned()),
        source_file: Some(json!({
            "path": format!("/library/{branch_id}.mkv"),
            "size_bytes": 4_200_000_000_u64,
        })),
    };
    let timing = voom_control_plane::workflow::timing::EffectiveTiming::for_test(25, 10);
    if operation == OperationKind::ScanLibrary {
        Ok(
            voom_control_plane::workflow::binding::render_default_payload_with_fan_out(
                operation, &branch, timing, 3,
            )?,
        )
    } else {
        Ok(
            voom_control_plane::workflow::binding::render_default_payload(
                operation, &branch, timing,
            )?,
        )
    }
}

fn scanner_result_with_three_files() -> Value {
    json!({
        "files": [
            {"path": "/library/file-000.mkv", "size_bytes": 4_200_000_000_u64},
            {"path": "/library/file-001.mkv", "size_bytes": 4_200_000_001_u64},
            {"path": "/library/file-002.mkv", "size_bytes": 4_200_000_002_u64}
        ]
    })
}

fn node_ids(tickets: &[Ticket]) -> Vec<String> {
    tickets
        .iter()
        .map(|ticket| {
            WorkflowTicketPayload::parse_ticket(&ticket.kind, ticket.payload.clone())
                .unwrap()
                .node_id
        })
        .collect()
}

fn ticket_kind(operation: OperationKind) -> String {
    format!("synthetic.workflow.operation.{}", operation_name(operation))
}

fn operation_name(operation: OperationKind) -> String {
    serde_json::to_value(operation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn format_time(t: OffsetDateTime) -> TestResult<String> {
    Ok(t.format(&time::format_description::well_known::Iso8601::DEFAULT)?)
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

trait TicketStateParseForTest {
    fn parse_for_test(state: &str) -> TicketState;
}

impl TicketStateParseForTest for TicketState {
    fn parse_for_test(state: &str) -> TicketState {
        match state {
            "pending" => TicketState::Pending,
            "ready" => TicketState::Ready,
            "leased" => TicketState::Leased,
            "succeeded" => TicketState::Succeeded,
            "failed" => TicketState::Failed,
            other => panic!("unknown ticket state {other}"),
        }
    }
}
