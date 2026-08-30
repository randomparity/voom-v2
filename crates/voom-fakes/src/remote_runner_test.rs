use std::collections::HashSet;

use secrecy::ExposeSecret;
use serde_json::json;
use voom_api::router_with_control_plane;
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{LeaseId, NodeId, OperationKind, TicketId, TicketOperation};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::tickets::{NewTicket, SqliteTicketRepo, TicketState};
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

use crate::process_supervisor::{ProcessSupervisor, ProcessSupervisorMilestone};

use super::{ExecutionAction, RemoteRunnerConfig, RemoteSyntheticRunner};

const OP: &str = "transcode_video";

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

fn transcode_video_ticket(input_path: &str, output_name: &str) -> serde_json::Value {
    let output_path = std::env::temp_dir().join(format!(
        "voom-remote-runner-{}-{output_name}",
        std::process::id()
    ));
    json!({
        "input": {
            "path": input_path,
            "expected": {
                "size_bytes": 5_u64,
                "content_hash": "blake3:input"
            }
        },
        "output": {
            "staging_root": output_path.parent().unwrap().to_string_lossy().into_owned(),
            "path": output_path.to_string_lossy().into_owned(),
            "container": "mkv",
            "video_codec": "hevc",
            "overwrite": true
        },
        "profile": {
            "name": "default-hevc",
            "target_codec": "hevc",
            "encoder": "libx265",
            "crf": 23_u8,
            "preset": "medium"
        },
        "artifact_access": {
            "inputs": ["handle:input:test"],
            "outputs": ["handle:output:test"]
        }
    })
}

#[derive(Debug)]
struct AbandonFirst;

impl super::RemoteFaultPolicy for AbandonFirst {
    fn action(&self, _ticket_id: TicketId, acquisition_ordinal: u32) -> super::ExecutionAction {
        if acquisition_ordinal == 1 {
            super::ExecutionAction::Abandoned
        } else {
            super::ExecutionAction::Completed
        }
    }
}

#[derive(Debug)]
struct CompleteAll;

impl super::RemoteFaultPolicy for CompleteAll {
    fn action(&self, _ticket_id: TicketId, _acquisition_ordinal: u32) -> super::ExecutionAction {
        super::ExecutionAction::Completed
    }
}

#[tokio::test]
async fn execution_state_serializes_cross_session_ordinals() {
    let state = std::sync::Arc::new(super::RemoteExecutionState::new(std::sync::Arc::new(
        AbandonFirst,
    )));
    let ticket_id = TicketId(7);
    let first = {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .record_acquisition(ticket_id, voom_core::LeaseId(1), voom_core::WorkerId(1))
                .await
        })
    };
    let second = {
        let state = state.clone();
        tokio::spawn(async move {
            state
                .record_acquisition(ticket_id, voom_core::LeaseId(2), voom_core::WorkerId(2))
                .await
        })
    };
    let mut records = [first.await.unwrap(), second.await.unwrap()];
    records.sort_by_key(|record| record.acquisition_ordinal);

    assert_eq!(records[0].acquisition_ordinal, 1);
    assert_eq!(records[0].action, super::ExecutionAction::Abandoned);
    assert_eq!(records[1].acquisition_ordinal, 2);
    assert_eq!(records[1].action, super::ExecutionAction::Completed);
    assert_eq!(state.records().await.len(), 2);
}

#[tokio::test]
async fn node_session_activates_all_workers_once() {
    let fixture = RemoteRunnerFixture::new().await;
    let base = fixture.config();
    let workers = (0..3)
        .map(|index| super::RemoteWorkerConfig {
            logical_name: format!("stress-worker-{index}"),
            operations: base.operations.clone(),
            artifact_access: base.artifact_access.clone(),
            max_parallel: 2,
        })
        .collect();
    let session = super::RemoteNodeSession::new(
        super::RemoteNodeSessionConfig {
            base_url: base.base_url,
            node_id: base.node_id,
            token: base.token,
            workers,
            max_polls: 3,
            idle_timeout: std::time::Duration::from_millis(100),
            poll_interval: std::time::Duration::from_millis(5),
            lease_ttl_seconds: 1,
            healthy_heartbeat_ttl_seconds: 3,
        },
        std::sync::Arc::new(super::RemoteExecutionState::new(std::sync::Arc::new(
            CompleteAll,
        ))),
    );
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let pool = voom_store::connect(&fixture.url).await.unwrap();
    let task = tokio::spawn({
        let session = session.clone();
        async move { session.run_until_stopped(stop_rx).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    stop_tx.send(true).unwrap();
    task.await.unwrap().unwrap();

    let counts: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(DISTINCT node_incarnation_id), COUNT(*) FROM workers WHERE node_id = ?",
    )
    .bind(i64::try_from(fixture.node_id.0).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(counts, (1, 3));
}

#[tokio::test]
async fn runner_polls_acquires_dispatches_heartbeats_and_completes() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-primary.mkv",
        ))
        .await;

    let mut config = fixture.config();
    config.base_url.push('/');
    let summary = RemoteSyntheticRunner::new(config)
        .run_once_to_completion()
        .await
        .unwrap();

    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.idle_polls, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn runner_uses_fresh_idempotency_keys_for_each_run() {
    let fixture = RemoteRunnerFixture::new().await;
    let first_ticket = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-first.mkv",
        ))
        .await;
    let runner = RemoteSyntheticRunner::new(fixture.config());

    let first = runner.run_once_to_completion().await.unwrap();
    let second_ticket = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/second.mkv",
            "runner-second.mkv",
        ))
        .await;
    let second = runner.run_once_to_completion().await.unwrap();

    assert_eq!(first.completed, 1);
    assert_eq!(second.completed, 1);
    assert_eq!(
        fixture.ticket_state(first_ticket).await,
        TicketState::Succeeded
    );
    assert_eq!(
        fixture.ticket_state(second_ticket).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn runner_instances_use_random_idempotency_run_ids() {
    let first = super::new_run_id();
    let second = super::new_run_id();

    assert_eq!(first.len(), 32);
    assert_ne!(first, second);
}

#[tokio::test]
async fn runner_activation_declares_configured_artifact_access() {
    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-failure.mkv",
        ))
        .await;

    let mut config = fixture.config();
    config.artifact_access = vec!["control_plane_placeholder".to_owned()];
    let summary = RemoteSyntheticRunner::new(config)
        .run_once_to_completion()
        .await
        .unwrap();

    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}

#[tokio::test]
async fn process_crash_uses_activated_credentials_and_a_typed_request() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(7).await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/process.mkv",
            "process-crash.mkv",
        ))
        .await;
    let expected_lease_id = fixture.expected_first_lease_id();
    let supervisor = ProcessSupervisor::start_with_expected_lease_id(expected_lease_id);

    let (record, observation) = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_process_crash(
            &supervisor,
            std::env::current_exe().unwrap(),
            &HashSet::from([ticket_id]),
        )
        .await
        .unwrap();

    assert_eq!(record.ticket_id, ticket_id);
    assert_eq!(record.lease_id, expected_lease_id);
    assert_eq!(observation.lease_id, expected_lease_id);
    assert_eq!(record.worker_id, observation.worker_id);
    assert_eq!(record.acquisition_ordinal, 1);
    assert_eq!(record.action, ExecutionAction::Abandoned);
    assert_eq!(observation.node_id, fixture.node_id);
    assert_eq!(observation.ticket_id, ticket_id);
    assert_eq!(observation.exit_code, Some(101));
    assert_ne!(observation.pid, 0);
    let worker_epoch: i64 = sqlx::query_scalar("SELECT epoch FROM workers WHERE id = ?")
        .bind(i64::try_from(observation.worker_id.0).unwrap())
        .fetch_one(&voom_store::connect(&fixture.url).await.unwrap())
        .await
        .unwrap();
    assert_eq!(worker_epoch, 0);
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[test]
fn process_crash_credentials_preserve_the_activated_epoch_and_refresh_the_secret() {
    let active = super::ActiveWorker {
        incarnation_id: "0123456789abcdef0123456789abcdef".parse().unwrap(),
        worker_id: voom_core::WorkerId(77),
        worker_epoch: 43,
    };

    let first = super::process_credentials(active);
    let second = super::process_credentials(active);

    assert_eq!(first.worker_id, active.worker_id);
    assert_eq!(first.worker_epoch, active.worker_epoch);
    assert_ne!(first.secret.expose_secret(), second.secret.expose_secret());
}

#[tokio::test]
async fn process_crash_rejects_an_acquired_ticket_outside_the_selected_set_before_dispatch() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(7).await;
    let acquired_ticket = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/unexpected.mkv",
            "unexpected.mkv",
        ))
        .await;
    let supervisor =
        ProcessSupervisor::start_with_expected_lease_id(fixture.expected_first_lease_id());

    let error = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_process_crash(
            &supervisor,
            std::env::current_exe().unwrap(),
            &HashSet::from([TicketId(acquired_ticket.0 + 1)]),
        )
        .await
        .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("outside selected process tickets")
    );
    let exits = supervisor.shutdown().await.unwrap();
    assert_eq!(exits.len(), 1);
    assert!(exits[0].success);
    let held: (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), SUM(CASE WHEN state = 'held' THEN 1 ELSE 0 END) FROM leases",
    )
    .fetch_one(&voom_store::connect(&fixture.url).await.unwrap())
    .await
    .unwrap();
    assert_eq!(held, (1, 1));
}

#[tokio::test]
async fn process_crash_rejects_a_clean_child_exit_after_dispatch() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(8).await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/clean.mkv",
            "clean-exit.mkv",
        ))
        .await;
    let supervisor =
        ProcessSupervisor::start_with_expected_lease_id(fixture.expected_first_lease_id());

    let error = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_process_crash(
            &supervisor,
            std::env::current_exe().unwrap(),
            &HashSet::from([ticket_id]),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("exited successfully"));
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[tokio::test]
async fn process_crash_rejects_child_that_exits_between_readiness_and_dispatch() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(10).await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/pre-dispatch-exit.mkv",
            "pre-dispatch-exit.mkv",
        ))
        .await;
    let supervisor = ProcessSupervisor::start();

    let outcome = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_process_crash(
            &supervisor,
            std::env::current_exe().unwrap(),
            &HashSet::from([ticket_id]),
        )
        .await;

    assert!(supervisor.shutdown().await.unwrap().is_empty());
    let error = outcome.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("without connection termination evidence")
    );
}

#[tokio::test]
async fn process_crash_timeout_reaps_stay_alive_worker_after_pending_wait() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(9).await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/stay-alive.mkv",
            "stay-alive-timeout.mkv",
        ))
        .await;
    let expected_lease_id = fixture.expected_first_lease_id();
    let (supervisor, mut milestones) =
        ProcessSupervisor::start_with_test_milestones_and_expected_lease_id(expected_lease_id);
    let supervisor = std::sync::Arc::new(supervisor);
    let inner_supervisor = std::sync::Arc::clone(&supervisor);
    let inner = tokio::spawn(async move {
        RemoteSyntheticRunner::new(fixture.config())
            .run_once_to_process_crash(
                &inner_supervisor,
                std::env::current_exe().unwrap(),
                &HashSet::from([ticket_id]),
            )
            .await
    });

    let child_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(ProcessSupervisorMilestone::WaitRegistered(child_id)) =
                milestones.recv().await
            {
                break child_id;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        !inner.is_finished(),
        "registered child wait must remain pending"
    );
    let completion = inner.await;

    let supervisor = std::sync::Arc::try_unwrap(supervisor).ok().unwrap();
    let cleanup = supervisor.shutdown().await;
    let (mut watcher_completed, mut registry_empty) = (false, false);
    while !watcher_completed || !registry_empty {
        match milestones.recv().await.unwrap() {
            ProcessSupervisorMilestone::WatcherCompleted(completed) => {
                watcher_completed = completed == child_id;
            }
            ProcessSupervisorMilestone::RegistryEmpty => registry_empty = true,
            ProcessSupervisorMilestone::ChildRegistered(_)
            | ProcessSupervisorMilestone::AwaitingReadiness(_)
            | ProcessSupervisorMilestone::WaitRegistered(_)
            | ProcessSupervisorMilestone::TombstoneStored(_) => {}
        }
    }

    let exits = cleanup.unwrap();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].child_id, child_id);
    assert!(
        !exits[0].success,
        "stay-alive child must exit only after kill"
    );
    let error = completion.unwrap().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("dispatch and exit observation timed out after five seconds")
    );
}

#[tokio::test]
async fn process_crash_observation_follows_explicit_wait_and_registry_removal() {
    let fixture = RemoteRunnerFixture::new().await;
    fixture.reserve_worker_ids(7).await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/reaped.mkv",
            "explicit-wait.mkv",
        ))
        .await;
    let expected_lease_id = fixture.expected_first_lease_id();
    let supervisor = ProcessSupervisor::start_with_expected_lease_id(expected_lease_id);

    let (_, observation) = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_process_crash(
            &supervisor,
            std::env::current_exe().unwrap(),
            &HashSet::from([ticket_id]),
        )
        .await
        .unwrap();

    assert_eq!(observation.exit_code, Some(101));
    assert_eq!(observation.lease_id, expected_lease_id);
    assert!(supervisor.shutdown().await.unwrap().is_empty());
}

#[tokio::test]
async fn process_crash_record_seeds_the_synthetic_retry_ordinal() {
    let state = super::RemoteExecutionState::new(std::sync::Arc::new(CompleteAll));
    let ticket_id = TicketId(12);
    state
        .record_process_crash(super::ExecutionRecord {
            ticket_id,
            lease_id: voom_core::LeaseId(21),
            worker_id: voom_core::WorkerId(31),
            acquisition_ordinal: 1,
            action: ExecutionAction::Abandoned,
        })
        .await
        .unwrap();

    let retry = state
        .record_acquisition(ticket_id, voom_core::LeaseId(22), voom_core::WorkerId(32))
        .await;

    assert_eq!(retry.acquisition_ordinal, 2);
    assert_eq!(retry.action, ExecutionAction::Completed);
    assert_eq!(state.records().await.len(), 2);
}

struct RemoteRunnerFixture {
    _tmp: TempDatabase,
    url: String,
    base_url: String,
    cp: ControlPlane,
    server: tokio::task::JoinHandle<()>,
    node_id: NodeId,
    token: secrecy::SecretString,
    expected_first_lease_id: LeaseId,
}

impl RemoteRunnerFixture {
    async fn new() -> Self {
        let tmp = TempDatabase::new().unwrap();
        let url = sqlite_url_for(tmp.path());
        voom_store::init(&url).await.unwrap();
        let cp = ControlPlane::open(&url).await.unwrap();
        let registered = cp
            .register_node(RegisterNodeInput {
                name: "remote-node".to_owned(),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 60,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let health = HealthPlane::open(&url).await.unwrap();
        let app = router_with_control_plane(health, cp.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _tmp: tmp,
            url,
            base_url: format!("http://{addr}"),
            cp,
            server,
            node_id: registered.node.id,
            token: registered.token,
            expected_first_lease_id: LeaseId(1),
        }
    }

    fn config(&self) -> RemoteRunnerConfig {
        RemoteRunnerConfig {
            base_url: self.base_url.clone(),
            node_id: self.node_id,
            token: self.token.expose_secret().to_owned().into(),
            worker_logical_name: "remote-worker".to_owned(),
            operations: vec![OperationKind::TranscodeVideo],
            artifact_access: vec!["shared_mount".to_owned()],
            max_parallel: 1,
            max_polls: 3,
            idle_timeout: std::time::Duration::from_millis(100),
            lease_heartbeat_interval: std::time::Duration::from_millis(10),
            lease_ttl_seconds: 60,
            healthy_heartbeat_ttl_seconds: 60,
        }
    }

    fn expected_first_lease_id(&self) -> LeaseId {
        self.expected_first_lease_id
    }

    async fn ready_ticket(&self, payload: serde_json::Value) -> TicketId {
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: ticket_op(OP),
                priority: 0,
                payload,
                max_attempts: 2,
                created_at: time::OffsetDateTime::UNIX_EPOCH,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, time::OffsetDateTime::UNIX_EPOCH)
            .await
            .unwrap();
        ticket.id
    }

    async fn ticket_state(&self, ticket_id: TicketId) -> TicketState {
        let pool = voom_store::connect(&self.url).await.unwrap();
        SqliteTicketRepo::new(pool)
            .get(ticket_id)
            .await
            .unwrap()
            .unwrap()
            .state
    }

    async fn reserve_worker_ids(&self, count: usize) {
        for ordinal in 0..count {
            let registered = self
                .cp
                .register_node(RegisterNodeInput {
                    name: format!("reserved-process-node-{ordinal}"),
                    kind: NodeKind::Remote,
                    heartbeat_ttl_seconds: 60,
                    metadata: json!({"reserved": true}),
                })
                .await
                .unwrap();
            let mut config = self.config();
            config.node_id = registered.node.id;
            config.token = registered.token;
            let runner = RemoteSyntheticRunner::new(config);
            let incarnation_id = voom_core::NodeIncarnationId::generate().unwrap();
            runner
                .activate(incarnation_id, format!("reserve-worker-{ordinal}"))
                .await
                .unwrap();
        }
    }
}

impl Drop for RemoteRunnerFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Issue #478: the synthetic runner consumes the acquire response's dispatch
/// contract end to end — the leased byte-work operation (already normalized
/// by the control plane before dispatch) executes and completes, and the
/// durable records bind one lease to its access plan and selected decision.
///
/// The activation surface declares closed bare `OperationKind` tokens only
/// (ADR 0071/#423), so this e2e exercises the canonical encoding; the
/// namespaced-encoding equivalence and owner-local evidence are covered at
/// the control-plane and API surfaces where worker registration is explicit.
#[tokio::test]
async fn runner_completes_acquired_byte_work_with_bound_plan_and_decision() {
    use voom_store::repo::execution::leases::{LeaseFilter, SqliteLeaseRepo};
    use voom_store::repo::execution::scheduler_decisions::{
        SchedulerDecisionFilter, SchedulerDecisionOutcome,
    };

    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-plan-bind.mkv",
        ))
        .await;

    let summary = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_completion()
        .await
        .unwrap();
    assert_eq!(summary.acquired, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );

    // Exactly one lease, exactly one plan bound to it, and the selected
    // decision names that same lease.
    let pool = voom_store::connect(&fixture.url).await.unwrap();
    // The completed lease has left `held`, so list without a state filter.
    let leases = SqliteLeaseRepo::new(pool.clone())
        .list(LeaseFilter { state: None }, None, 10)
        .await
        .unwrap();
    assert_eq!(
        leases.len(),
        1,
        "exactly one lease was acquired for the byte-work ticket"
    );
    let plans = voom_store::repo::media::artifact_access_plans::SqliteArtifactAccessPlanRepo::new(
        pool.clone(),
    )
    .list_by_ticket(ticket_id)
    .await
    .unwrap();
    assert_eq!(plans.len(), 1, "exactly one plan binds the ticket's lease");
    let decisions =
        voom_store::repo::execution::scheduler_decisions::SqliteSchedulerDecisionRepo::new(pool)
            .list(SchedulerDecisionFilter {
                ticket_id: Some(ticket_id),
                outcome: Some(SchedulerDecisionOutcome::Selected),
                limit: 10,
                ..SchedulerDecisionFilter::default()
            })
            .await
            .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].selected_lease_id,
        Some(plans[0].lease_id),
        "the selected decision and the plan bind the same lease"
    );
}

/// Issue #479: the acquire key the synthetic runner used replays the original
/// leased outcome after the ticket reached its terminal state — one lease,
/// one plan, one execution, no second mutation.
#[tokio::test]
async fn runner_acquire_key_replays_identically_without_second_execution() {
    use voom_control_plane::execution::{RemoteAcquireInput, RemoteAcquireOutcome};
    use voom_core::{NodeIncarnationId, WorkerId};

    let fixture = RemoteRunnerFixture::new().await;
    let ticket_id = fixture
        .ready_ticket(transcode_video_ticket(
            "/library/movie.mkv",
            "runner-replay.mkv",
        ))
        .await;
    let summary = RemoteSyntheticRunner::new(fixture.config())
        .run_once_to_completion()
        .await
        .unwrap();
    assert_eq!(summary.completed, 1);

    let pool = voom_store::connect(&fixture.url).await.unwrap();
    let (stored_key, request_hash): (String, String) = sqlx::query_as(
        "SELECT idempotency_key, request_hash FROM remote_idempotency_keys \
         WHERE route_key = 'POST /v1/execution/lease/acquire' AND status = 'completed' \
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (incarnation_token, client_key) = stored_key.split_once(':').unwrap();
    let incarnation_id: NodeIncarnationId = incarnation_token.parse().unwrap();

    let replay = fixture
        .cp
        .remote_acquire(RemoteAcquireInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            incarnation_id,
            worker_id: WorkerId(1),
            idempotency_key: client_key.to_owned(),
            request_hash,
            lease_ttl_seconds: 60,
        })
        .await
        .unwrap();
    assert!(
        matches!(replay, RemoteAcquireOutcome::Leased(_)),
        "replay must return the original leased outcome"
    );
    let RemoteAcquireOutcome::Leased(dispatch) = replay else {
        return;
    };
    assert_eq!(dispatch.ticket_id.0, ticket_id.0);

    let leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(&pool)
        .await
        .unwrap();
    let plans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_access_plans")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leases, 1);
    assert_eq!(plans, 1);
    assert_eq!(
        fixture.ticket_state(ticket_id).await,
        TicketState::Succeeded
    );
}
