use super::*;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_core::{
    ArtifactAccessMode, ErrorCode, FailureClass, LeaseId, LibraryId, NodeId, OperationKind,
    ProviderLocator, ProviderRelativeLocator, ScanSessionStatus, StorageProviderKind,
    StorageRootId, TicketId, TicketOperation, clock_test_support::FrozenClock,
};
use voom_events::EventKind;
use voom_scheduler::{
    NodeCandidate, SCORING_VERSION, SchedulerCandidate, ScoreDecision, ScoreOutcome,
    ScoreReasonCode, TicketCandidate, WorkerCandidate,
};
use voom_store::repo::execution::node_incarnations::NewNodeIncarnation;
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::repo::execution::remote_idempotency::RemoteMutationReplay;
use voom_store::repo::execution::scheduler_decisions::{
    SchedulerDecisionFilter, SchedulerDecisionOutcome, SchedulerReasonCode,
};
use voom_store::repo::execution::tickets::{NewTicket, TicketState};
use voom_store::repo::execution::workers::WorkerKind;
use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
use voom_store::repo::library::library_roots::{
    HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
};
use voom_store::repo::media::artifact_access_plans::ArtifactAccessPlanStatus;
use voom_store::repo::scan::sessions::ScanObservation;

use crate::cases::count;
use crate::cases::workers::nodes::RegisterNodeInput;
use crate::cases::workers::{
    NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterWorkerForNodeInput,
};
use crate::scan::{RemoteScanBatchInput, RemoteScanStartInput};
use crate::workflow::plan::ticket_payload::WorkflowTicketPayload;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const OP: &str = "test.remote";

#[test]
fn remote_replay_outcomes_reject_unknown_fields() {
    assert_unknown_rejected::<RemoteNodeHeartbeatOutcome>(json!({
        "node_id": 1,
        "status": "active"
    }));
    assert_unknown_rejected::<RemoteAcquireOutcome>(json!({
        "outcome": "idle",
        "worker_id": 1,
        "scheduler_decision_id": 2
    }));
    assert_unknown_rejected::<RemoteAcquireOutcome>(json!({
        "outcome": "no_candidate",
        "worker_id": 1,
        "scheduler_decision_id": 2
    }));
    let mut leased = remote_lease_dispatch_json();
    leased["outcome"] = json!("leased");
    assert_unknown_rejected::<RemoteAcquireOutcome>(leased);
    assert_unknown_rejected::<RemoteLeaseDispatch>(remote_lease_dispatch_json());
    assert_unknown_rejected::<RemoteLeaseHeartbeatOutcome>(json!({
        "lease_id": 1,
        "worker_id": 2,
        "ttl_seconds": 60
    }));
    assert_unknown_rejected::<RemoteCompleteOutcome>(json!({
        "lease_id": 1,
        "ticket_id": 2,
        "worker_id": 3,
        "artifact_access_plan": remote_artifact_access_plan_json()
    }));
    assert_unknown_rejected::<RemoteFailOutcome>(json!({
        "lease_id": 1,
        "ticket_id": 2,
        "worker_id": 3,
        "artifact_access_plan": remote_artifact_access_plan_json()
    }));
    assert_unknown_rejected::<RemoteArtifactAccessPlan>(remote_artifact_access_plan_json());
}

#[test]
fn remote_acquire_outcomes_preserve_durable_wire_shapes() {
    let idle = RemoteAcquireOutcome::Idle {
        worker_id: voom_core::WorkerId(1),
        scheduler_decision_id: 2,
    };
    assert_eq!(
        serde_json::to_value(idle).unwrap(),
        json!({
            "outcome": "idle",
            "worker_id": 1,
            "scheduler_decision_id": 2
        })
    );

    let no_candidate = RemoteAcquireOutcome::NoCandidate {
        worker_id: voom_core::WorkerId(1),
        scheduler_decision_id: 2,
    };
    assert_eq!(
        serde_json::to_value(no_candidate).unwrap(),
        json!({
            "outcome": "no_candidate",
            "worker_id": 1,
            "scheduler_decision_id": 2
        })
    );

    let leased = RemoteAcquireOutcome::Leased(RemoteLeaseDispatch {
        lease_id: LeaseId(1),
        scheduler_decision_id: 2,
        ticket_id: TicketId(3),
        worker_id: voom_core::WorkerId(4),
        operation: "test.remote".to_owned(),
        dispatch_payload: json!({}),
        lease_ttl_seconds: 60,
        heartbeat_after_seconds: 20,
        artifact_access_plan: RemoteArtifactAccessPlan {
            id: 1,
            owner_node_id: None,
            access_evidence: None,
        },
    });
    let mut expected = remote_lease_dispatch_json();
    expected["outcome"] = json!("leased");
    assert_eq!(serde_json::to_value(leased).unwrap(), expected);
}

fn assert_unknown_rejected<T>(value: serde_json::Value)
where
    T: serde::de::DeserializeOwned,
{
    assert!(serde_json::from_value::<T>(value.clone()).is_ok());
    let mut value = value;
    value["unexpected"] = json!(true);
    assert!(serde_json::from_value::<T>(value).is_err());
}

fn remote_lease_dispatch_json() -> serde_json::Value {
    json!({
        "lease_id": 1,
        "scheduler_decision_id": 2,
        "ticket_id": 3,
        "worker_id": 4,
        "operation": "test.remote",
        "dispatch_payload": {},
        "lease_ttl_seconds": 60,
        "heartbeat_after_seconds": 20,
        "artifact_access_plan": remote_artifact_access_plan_json()
    })
}

fn remote_artifact_access_plan_json() -> serde_json::Value {
    json!({
        "id": 1,
        "owner_node_id": null,
        "access_evidence": null
    })
}

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

struct RemoteFixture {
    cp: crate::ControlPlane,
    _tmp: voom_test_support::TempDatabase,
    node_id: NodeId,
    token: secrecy::SecretString,
    incarnation_id: NodeIncarnationId,
    worker_id: voom_core::WorkerId,
}

impl RemoteFixture {
    async fn ready_ticket(&self, kind: &str) -> TicketId {
        self.ready_ticket_with_priority(kind, 0).await
    }

    async fn ready_ticket_with_priority(&self, kind: &str, priority: i64) -> TicketId {
        let ticket = self
            .cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: ticket_op(kind),
                priority,
                payload: json!({
                    "dispatch": {"kind": kind},
                    "artifact_access": {
                        "inputs": ["handle:input:test"],
                        "outputs": ["handle:output:test"]
                    }
                }),
                max_attempts: 2,
                created_at: T0,
            })
            .await
            .unwrap();
        self.cp
            .mark_ready_if_unblocked(ticket.id, T0)
            .await
            .unwrap();
        ticket.id
    }

    fn acquire_input(&self, idempotency_key: &str, request_hash: &str) -> RemoteAcquireInput {
        RemoteAcquireInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            worker_id: self.worker_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            lease_ttl_seconds: 60,
        }
    }

    fn acquire_input_with_ttl(
        &self,
        idempotency_key: &str,
        request_hash: &str,
        lease_ttl_seconds: i64,
    ) -> RemoteAcquireInput {
        RemoteAcquireInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            worker_id: self.worker_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            lease_ttl_seconds,
        }
    }

    fn complete_input(
        &self,
        lease_id: LeaseId,
        idempotency_key: &str,
        request_hash: &str,
    ) -> RemoteCompleteInput {
        RemoteCompleteInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            worker_id: self.worker_id,
            lease_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            result: json!({
                "ok": true,
                "artifact_access": {"validated": true}
            }),
        }
    }

    fn node_heartbeat_input(
        &self,
        idempotency_key: &str,
        request_hash: &str,
    ) -> RemoteNodeHeartbeatInput {
        RemoteNodeHeartbeatInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
        }
    }

    fn lease_heartbeat_input(
        &self,
        lease_id: LeaseId,
        idempotency_key: &str,
        request_hash: &str,
    ) -> RemoteLeaseHeartbeatInput {
        RemoteLeaseHeartbeatInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            worker_id: self.worker_id,
            lease_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            lease_ttl_seconds: 60,
        }
    }

    fn fail_input(
        &self,
        lease_id: LeaseId,
        idempotency_key: &str,
        request_hash: &str,
    ) -> RemoteFailInput {
        RemoteFailInput {
            node_id: self.node_id,
            token: self.token.clone(),
            incarnation_id: self.incarnation_id,
            worker_id: self.worker_id,
            lease_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            reason: "artifact access mode shared_mount is not advertised".to_owned(),
            class: FailureClass::ArtifactUnavailable,
            evidence: json!({"validated": false}),
        }
    }
}

fn stale_incarnation_id() -> NodeIncarnationId {
    "fedcba9876543210fedcba9876543210".parse().unwrap()
}

#[tokio::test]
async fn remote_node_and_acquire_fences_precede_replay_reservation() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;

    let mut heartbeat = fixture.node_heartbeat_input("stale-node", "hash-stale-node");
    heartbeat.incarnation_id = stale_incarnation_id();
    let heartbeat_error = fixture
        .cp
        .remote_node_heartbeat(heartbeat)
        .await
        .unwrap_err();
    assert_eq!(heartbeat_error.error_code(), ErrorCode::Conflict);

    let mut acquire = fixture.acquire_input("stale-acquire", "hash-stale-acquire");
    acquire.incarnation_id = stale_incarnation_id();
    let acquire_error = fixture.cp.remote_acquire(acquire).await.unwrap_err();
    assert_eq!(acquire_error.error_code(), ErrorCode::Conflict);

    let reserved: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remote_idempotency_keys \
         WHERE idempotency_key LIKE '%stale-node' \
            OR idempotency_key LIKE '%stale-acquire'",
    )
    .fetch_one(fixture.cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(reserved, 0);
}

#[tokio::test]
async fn remote_fence_rejects_corrupt_active_pointer_before_replay_reservation() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let other = fixture
        .cp
        .register_node(remote_node_input("other-node"))
        .await
        .unwrap();
    let corrupt_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    sqlx::query(
        "INSERT INTO node_incarnations \
         (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES (?, ?, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .bind(corrupt_id)
    .bind(i64::try_from(other.node.id.0).unwrap())
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    sqlx::query("UPDATE nodes SET active_incarnation_id = ? WHERE id = ?")
        .bind(corrupt_id)
        .bind(i64::try_from(fixture.node_id.0).unwrap())
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();

    let error = fixture
        .cp
        .remote_node_heartbeat(
            fixture.node_heartbeat_input("corrupt-pointer", "corrupt-pointer-hash"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    let replay_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remote_idempotency_keys \
         WHERE idempotency_key LIKE '%:corrupt-pointer'",
    )
    .fetch_one(fixture.cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(replay_rows, 0);
}

#[tokio::test]
async fn node_incarnations_may_reuse_external_idempotency_keys() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture
        .cp
        .remote_node_heartbeat(fixture.node_heartbeat_input("shared-key", "first-hash"))
        .await
        .unwrap();

    let next_incarnation_id = stale_incarnation_id();
    fixture
        .cp
        .remote_activate(RemoteActivateInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            idempotency_key: "activate-next-incarnation".to_owned(),
            request_hash: "activate-next-incarnation-hash".to_owned(),
            incarnation_id: next_incarnation_id,
            workers: vec![RemoteWorkerDeclaration {
                logical_name: "replacement-worker".to_owned(),
                operations: vec![voom_core::OperationKind::TranscodeVideo],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            }],
        })
        .await
        .unwrap();
    fixture
        .cp
        .remote_node_heartbeat(RemoteNodeHeartbeatInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            incarnation_id: next_incarnation_id,
            idempotency_key: "shared-key".to_owned(),
            request_hash: "second-hash".to_owned(),
        })
        .await
        .unwrap();

    let replay_keys: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM remote_idempotency_keys \
         WHERE idempotency_key LIKE '%:shared-key' \
         ORDER BY idempotency_key",
    )
    .fetch_all(fixture.cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        replay_keys,
        vec![
            format!("{}:shared-key", fixture.incarnation_id),
            format!("{next_incarnation_id}:shared-key"),
        ]
    );
}

#[tokio::test]
async fn remote_lease_mutation_fences_precede_replay_reservation() {
    for route in ["heartbeat", "complete", "fail"] {
        let fixture = leased_fixture().await;
        let lease_id = fixture_lease_id(&fixture).await;
        let error = match route {
            "heartbeat" => {
                let mut input = fixture.lease_heartbeat_input(
                    lease_id,
                    "stale-lease-heartbeat",
                    "hash-stale-lease-heartbeat",
                );
                input.incarnation_id = stale_incarnation_id();
                fixture.cp.remote_lease_heartbeat(input).await.unwrap_err()
            }
            "complete" => {
                let mut input = fixture.complete_input(
                    lease_id,
                    "stale-lease-complete",
                    "hash-stale-lease-complete",
                );
                input.incarnation_id = stale_incarnation_id();
                fixture.cp.remote_complete(input).await.unwrap_err()
            }
            "fail" => {
                let mut input =
                    fixture.fail_input(lease_id, "stale-lease-fail", "hash-stale-lease-fail");
                input.incarnation_id = stale_incarnation_id();
                fixture.cp.remote_fail(input).await.unwrap_err()
            }
            _ => unreachable!(),
        };
        assert_eq!(error.error_code(), ErrorCode::Conflict);
        let reserved: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM remote_idempotency_keys \
             WHERE idempotency_key LIKE '%stale-lease-%'",
        )
        .fetch_one(fixture.cp.pool_for_test())
        .await
        .unwrap();
        assert_eq!(reserved, 0);
    }
}

#[tokio::test]
async fn remote_acquire_returns_idle_when_no_ready_work() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("acquire-idle", "hash-1"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Idle {
        worker_id,
        scheduler_decision_id: _,
    } = outcome
    else {
        panic!("expected idle remote acquire");
    };
    assert_eq!(worker_id, fixture.worker_id);
    assert_eq!(count(&fixture.cp, EventKind::LeaseAcquired).await, 0);
}

#[tokio::test]
async fn remote_acquire_idle_returns_and_persists_scheduler_decision() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("acquire-idle-decision", "hash-idle-decision"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Idle {
        worker_id,
        scheduler_decision_id,
    } = outcome
    else {
        panic!("expected idle remote acquire");
    };
    assert_eq!(worker_id, fixture.worker_id);

    let decision = fixture
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.outcome, SchedulerDecisionOutcome::Idle);
    assert_eq!(decision.request_worker_id, Some(fixture.worker_id));
}

#[tokio::test]
async fn remote_acquire_leased_returns_scheduler_decision_id_linked_to_lease() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("acquire-leased-decision", "hash-leased-decision"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected remote lease dispatch");
    };
    let decision = fixture
        .cp
        .scheduler_decision(dispatch.scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.selected_lease_id, Some(dispatch.lease_id));
    assert_eq!(decision.selected_worker_id, Some(fixture.worker_id));
}

#[tokio::test]
async fn remote_acquire_replay_returns_original_scheduler_decision_without_rescoring() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("replay-decision", "hash-replay-decision"))
        .await
        .unwrap();
    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input("replay-decision", "hash-replay-decision"))
        .await
        .unwrap();

    assert_eq!(replay, first);
    let decision_count = fixture
        .cp
        .scheduler_decisions(SchedulerDecisionFilter::default())
        .await
        .unwrap()
        .len();
    assert_eq!(decision_count, 1);
}

#[tokio::test]
async fn remote_acquire_uses_scored_priority_then_tie_breaker() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let low = fixture.ready_ticket_with_priority(OP, 0).await;
    let high = fixture.ready_ticket_with_priority(OP, 10).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("priority-score", "hash-priority-score"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected remote lease dispatch");
    };
    assert_eq!(dispatch.ticket_id, high);
    assert_eq!(
        fixture.cp.tickets().get(low).await.unwrap().unwrap().state,
        TicketState::Ready
    );
}

#[tokio::test]
async fn remote_acquire_no_candidate_is_success_with_decision() {
    let fixture = remote_fixture(&[(OP, vec!["local_path"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("unsupported-no-candidate", "hash-no-candidate"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::NoCandidate {
        worker_id,
        scheduler_decision_id,
    } = outcome
    else {
        panic!("expected successful no-candidate remote acquire");
    };
    assert_eq!(worker_id, fixture.worker_id);

    let decision = fixture
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "unsupported_artifact_access");
}

#[test]
fn score_remote_candidates_uses_global_no_candidate_reason_priority() {
    let unsupported_artifact = scheduler_candidate("test.unsupported", TicketId(1));
    let missing_capability = SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id: TicketId(2),
            operation: ticket_op("test.missing_capability"),
            priority: 0,
            next_eligible_at_epoch_seconds: 0,
        },
        worker: WorkerCandidate {
            worker_id: voom_core::WorkerId(1),
            node_id: NodeId(1),
            executable: true,
            has_capability: false,
            has_grant: true,
            denied: false,
            active_leases: 0,
            max_parallel: 1,
            artifact_access: vec![ArtifactAccessMode::SharedMount],
        },
        node: NodeCandidate {
            node_id: NodeId(1),
            executable: true,
            heartbeat_fresh: true,
            active_leases: 0,
            max_parallel_leases: 1,
        },
    };

    let score = score_remote_candidates(&[unsupported_artifact, missing_capability]).unwrap();

    assert_eq!(score.outcome, ScoreOutcome::NoEligibleCandidate);
    assert_eq!(score.reason_code, ScoreReasonCode::MissingCapability);
    assert_eq!(score.candidate_count, 2);
    assert_eq!(score.explanation["operation"], serde_json::Value::Null);
    assert_eq!(score.explanation["candidates"].as_array().unwrap().len(), 2);
}

#[test]
fn scheduler_reason_maps_typed_score_reason_codes_to_store_vocab() {
    assert_eq!(
        scheduler_reason(ScoreReasonCode::MissingGrant),
        SchedulerReasonCode::MissingGrant
    );
    assert_eq!(
        scheduler_reason(ScoreReasonCode::UnsupportedArtifactAccess),
        SchedulerReasonCode::UnsupportedArtifactAccess
    );
    assert_eq!(
        scheduler_reason(ScoreReasonCode::NoEligibleCandidate),
        SchedulerReasonCode::NoEligibleCandidate
    );
}

#[test]
fn suppression_key_includes_operation_fingerprint() {
    let fixture_input = RemoteAcquireInput {
        node_id: NodeId(1),
        token: secrecy::SecretString::from("token"),
        incarnation_id: stale_incarnation_id(),
        worker_id: WorkerId(2),
        idempotency_key: "operation-fingerprint".to_owned(),
        request_hash: "hash".to_owned(),
        lease_ttl_seconds: 60,
    };
    let transcode = ScoreDecision {
        outcome: ScoreOutcome::NoEligibleCandidate,
        selected: None,
        candidate_count: 1,
        reason_code: ScoreReasonCode::UnsupportedArtifactAccess,
        explanation: json!({
            "scoring_version": SCORING_VERSION,
            "candidates": [{"operation": "transcode", "reasons": ["unsupported_artifact_access"]}]
        }),
    };
    let probe = ScoreDecision {
        explanation: json!({
            "scoring_version": SCORING_VERSION,
            "candidates": [{"operation": "probe", "reasons": ["unsupported_artifact_access"]}]
        }),
        ..transcode.clone()
    };

    let transcode_key = suppression_key(&fixture_input, &transcode).unwrap();
    let probe_key = suppression_key(&fixture_input, &probe).unwrap();

    assert_ne!(transcode_key, probe_key);
    assert!(transcode_key.contains("ops:transcode"));
    assert!(probe_key.contains("ops:probe"));
}

#[test]
fn capacity_suppression_key_includes_operation_fingerprint() {
    let fixture_input = RemoteAcquireInput {
        node_id: NodeId(1),
        token: secrecy::SecretString::from("token"),
        incarnation_id: stale_incarnation_id(),
        worker_id: WorkerId(2),
        idempotency_key: "capacity-operation-fingerprint".to_owned(),
        request_hash: "hash".to_owned(),
        lease_ttl_seconds: 60,
    };

    let transcode_key = capacity_suppression_key(
        &fixture_input,
        SchedulerReasonCode::NodeCapacityFull.as_str(),
        &ticket_op("transcode"),
        TicketId(3),
    );
    let probe_key = capacity_suppression_key(
        &fixture_input,
        SchedulerReasonCode::NodeCapacityFull.as_str(),
        &ticket_op("probe"),
        TicketId(3),
    );

    assert_ne!(transcode_key, probe_key);
    assert!(transcode_key.contains("ops:transcode"));
    assert!(probe_key.contains("ops:probe"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_remote_acquire_does_not_spuriously_fail_on_contention() {
    // M6 regression: worker grant defaults to max_parallel {"*": 1} and the
    // node default limit is 1, so exactly one of N concurrent acquires should
    // win a lease and the rest should cleanly observe "capacity full". A
    // deferred BEGIN makes the read-then-write transactions hit SQLITE_BUSY on
    // lease-insert contention (busy_timeout does not retry a lock upgrade), so
    // the losers error instead. BEGIN IMMEDIATE serializes them on the write
    // lock up front, so every acquire completes: 1 Leased + (N-1) NoCandidate,
    // 0 errors. The safety invariant (never more than one held lease) holds
    // either way.
    const N: usize = 8;
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    for _ in 0..N {
        fixture.ready_ticket(OP).await;
    }

    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
        let cp = fixture.cp.clone();
        let input = fixture.acquire_input(&format!("concurrent-{i}"), &format!("hash-{i}"));
        handles.push(tokio::spawn(async move { cp.remote_acquire(input).await }));
    }

    let mut leased = 0_usize;
    let mut no_candidate = 0_usize;
    let mut errors = Vec::new();
    for handle in handles {
        match handle.await.unwrap() {
            Ok(RemoteAcquireOutcome::Leased(_)) => leased += 1,
            Ok(_) => no_candidate += 1,
            Err(err) => errors.push(err),
        }
    }

    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leases WHERE worker_id = ? AND state = 'held'")
            .bind(i64::try_from(fixture.worker_id.0).unwrap())
            .fetch_one(fixture.cp.pool_for_test())
            .await
            .unwrap();

    assert!(
        errors.is_empty(),
        "concurrent acquires must not fail under contention, got {} error(s): {errors:?}",
        errors.len()
    );
    assert_eq!(held, 1, "exactly one held lease expected, found {held}");
    assert_eq!(leased, 1, "exactly one acquire should win the lease");
    assert_eq!(
        no_candidate,
        N - 1,
        "every loser should cleanly observe capacity full"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_node_registration_during_remote_acquire_does_not_fail() {
    // The M6 fix converted only the remote-execution handlers to BEGIN
    // IMMEDIATE; other writers still open a deferred BEGIN. The SQLITE_BUSY
    // trap is specific to *read-then-write* transactions (the write upgrades a
    // lock the busy handler won't retry). `register_node` opens a deferred
    // BEGIN but its first statement is the node INSERT — a clean write
    // acquisition that busy_timeout serializes — so it coexists with the
    // BEGIN IMMEDIATE acquires without failing. This guards that interaction
    // and documents the boundary: write-first deferred transactions are safe.
    const N: usize = 6;
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    for _ in 0..N {
        fixture.ready_ticket(OP).await;
    }

    let mut handles = Vec::with_capacity(N * 2);
    for i in 0..N {
        let cp = fixture.cp.clone();
        let input = fixture.acquire_input(&format!("mixed-acq-{i}"), &format!("mixed-h-{i}"));
        handles.push(tokio::spawn(async move {
            cp.remote_acquire(input)
                .await
                .err()
                .map(|err| format!("acquire-{i}: {err:?}"))
        }));
    }
    for i in 0..N {
        let cp = fixture.cp.clone();
        handles.push(tokio::spawn(async move {
            cp.register_node(node_input(&format!("mixed-node-{i}"), NodeKind::Remote))
                .await
                .err()
                .map(|err| format!("register-{i}: {err:?}"))
        }));
    }

    let mut errors = Vec::new();
    for handle in handles {
        if let Some(err) = handle.await.unwrap() {
            errors.push(err);
        }
    }
    assert!(
        errors.is_empty(),
        "mixed concurrent writers must not fail under contention, got {} error(s): {errors:?}",
        errors.len()
    );
}

#[tokio::test]
async fn node_default_limit_blocks_second_concurrent_remote_acquire() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    sqlx::query("UPDATE worker_grants SET max_parallel = ? WHERE worker_id = ?")
        .bind(serde_json::to_string(&json!({"*": 2})).unwrap())
        .bind(i64::try_from(fixture.worker_id.0).unwrap())
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();
    fixture.ready_ticket_with_priority(OP, 10).await;
    fixture.ready_ticket_with_priority(OP, 9).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("node-limit-first", "hash-node-limit-first"))
        .await
        .unwrap();
    assert!(matches!(first, RemoteAcquireOutcome::Leased(_)));

    let second = fixture
        .cp
        .remote_acquire(fixture.acquire_input("node-limit-second", "hash-node-limit-second"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::NoCandidate {
        worker_id,
        scheduler_decision_id,
    } = second
    else {
        panic!("expected node-capacity no-candidate remote acquire");
    };
    assert_eq!(worker_id, fixture.worker_id);

    let decision = fixture
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "node_capacity_full");
}

fn scheduler_candidate(operation: &str, ticket_id: TicketId) -> SchedulerCandidate {
    SchedulerCandidate {
        ticket: TicketCandidate {
            ticket_id,
            operation: ticket_op(operation),
            priority: 0,
            next_eligible_at_epoch_seconds: 0,
        },
        worker: WorkerCandidate {
            worker_id: voom_core::WorkerId(1),
            node_id: NodeId(1),
            executable: true,
            has_capability: true,
            has_grant: true,
            denied: false,
            active_leases: 0,
            max_parallel: 1,
            artifact_access: Vec::new(),
        },
        node: NodeCandidate {
            node_id: NodeId(1),
            executable: true,
            heartbeat_fresh: true,
            active_leases: 0,
            max_parallel_leases: 1,
        },
    }
}

#[tokio::test]
async fn remote_acquire_replays_new_idle_decision_without_duplicate_log() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let input = fixture.acquire_input("acquire-idle-replay", "hash-idle-replay");

    let first = fixture.cp.remote_acquire(input.clone()).await.unwrap();
    let replay = fixture.cp.remote_acquire(input).await.unwrap();

    assert_eq!(replay, first);
    let rows = fixture
        .cp
        .scheduler_decisions(SchedulerDecisionFilter::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn remote_acquire_repoints_noncanonical_replays_terminal() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let cases = [
        (
            "missing-decision-id",
            json!({
                "outcome": "idle",
                "worker_id": fixture.worker_id,
            }),
        ),
        (
            "null-decision-id",
            json!({
                "outcome": "idle",
                "worker_id": fixture.worker_id,
                "scheduler_decision_id": null,
            }),
        ),
        (
            "wrong-decision-id-type",
            json!({
                "outcome": "idle",
                "worker_id": fixture.worker_id,
                "scheduler_decision_id": "42",
            }),
        ),
        ("non-object-data", json!("idle")),
        (
            "unknown-outcome",
            json!({ "outcome": "unrecognized_future_variant" }),
        ),
    ];

    for (name, data) in cases {
        let key = format!("poison-{name}");
        let request_hash = format!("hash-{name}");
        seed_legacy_acquire_replay(&fixture, &key, &request_hash, data).await;

        let err = fixture
            .cp
            .remote_acquire(fixture.acquire_input(&key, &request_hash))
            .await
            .unwrap_err();
        assert!(
            matches!(err, VoomError::Internal(_)),
            "{name} replay should surface a decode error, got: {err:?}"
        );

        let stored = stored_replay(&fixture, &key).await;
        assert!(
            matches!(stored, RemoteMutationReplay::Error { .. }),
            "{name} replay must be rewritten terminal, still: {stored:?}"
        );
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "single scenario preserves the hard-error and scheduler-no-candidate boundary"
)]
async fn remote_acquire_requires_worker_node_ownership_capability_grant_and_no_deny() {
    let wrong_owner = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let other_node = wrong_owner
        .cp
        .register_node(remote_node_input("other-node"))
        .await
        .unwrap();
    wrong_owner.ready_ticket(OP).await;
    let err = wrong_owner
        .cp
        .remote_acquire(RemoteAcquireInput {
            node_id: other_node.node.id,
            token: other_node.token,
            incarnation_id: stale_incarnation_id(),
            worker_id: wrong_owner.worker_id,
            idempotency_key: "wrong-owner".to_owned(),
            request_hash: "hash-wrong-owner".to_owned(),
            lease_ttl_seconds: 60,
        })
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);

    let missing_grant = remote_fixture(&[(OP, vec!["shared_mount"])], &[], &[]).await;
    let missing_grant_ticket = missing_grant.ready_ticket(OP).await;
    let outcome = missing_grant
        .cp
        .remote_acquire(missing_grant.acquire_input("missing-grant", "hash-missing-grant"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::NoCandidate {
        scheduler_decision_id,
        ..
    } = outcome
    else {
        panic!("expected missing-grant no-candidate");
    };
    let decision = missing_grant
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "missing_grant");
    assert_eq!(
        missing_grant
            .cp
            .tickets()
            .get(missing_grant_ticket)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );

    let missing_capability = remote_fixture(&[], &[OP], &[]).await;
    let missing_capability_ticket = missing_capability.ready_ticket(OP).await;
    let outcome = missing_capability
        .cp
        .remote_acquire(
            missing_capability.acquire_input("missing-capability", "hash-missing-capability"),
        )
        .await
        .unwrap();
    let RemoteAcquireOutcome::NoCandidate {
        scheduler_decision_id,
        ..
    } = outcome
    else {
        panic!("expected missing-capability no-candidate");
    };
    let decision = missing_capability
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "missing_capability");
    assert_eq!(
        missing_capability
            .cp
            .tickets()
            .get(missing_capability_ticket)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );

    let denied = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[OP]).await;
    let denied_ticket = denied.ready_ticket(OP).await;
    let outcome = denied
        .cp
        .remote_acquire(denied.acquire_input("denied", "hash-denied"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Idle {
        scheduler_decision_id,
        ..
    } = outcome
    else {
        panic!("expected denied idle result");
    };
    let decision = denied
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.outcome, SchedulerDecisionOutcome::Idle);
    assert_eq!(
        denied
            .cp
            .tickets()
            .get(denied_ticket)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );

    let eligible = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let ticket_id = eligible.ready_ticket(OP).await;
    let outcome = eligible
        .cp
        .remote_acquire(eligible.acquire_input("eligible", "hash-eligible"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected remote lease dispatch");
    };
    assert_eq!(dispatch.ticket_id, ticket_id);
    assert_eq!(dispatch.worker_id, eligible.worker_id);
    assert_eq!(dispatch.artifact_access_plan.owner_node_id, None);
    assert_eq!(dispatch.artifact_access_plan.access_evidence, None);
}

#[tokio::test]
async fn remote_acquire_ignores_unknown_artifact_access_when_known_mode_is_advertised() {
    let fixture = remote_fixture(
        &[(OP, vec!["future_transport", "shared_mount"])],
        &[OP],
        &[],
    )
    .await;
    fixture.ready_ticket(OP).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("mixed-access", "hash-mixed-access"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected known artifact access mode to remain selectable");
    };
    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(dispatch.lease_id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(dispatch.artifact_access_plan.owner_node_id, None);
    assert_eq!(dispatch.artifact_access_plan.access_evidence, None);
    assert_eq!(plan.owner_node_id, None);
    assert_eq!(plan.access_evidence, None);
}

#[tokio::test]
async fn remote_acquire_replays_unsupported_artifact_access_no_candidate_without_leasing() {
    let fixture = remote_fixture(&[(OP, vec!["local_path"])], &[OP], &[]).await;
    let ticket_id = fixture.ready_ticket(OP).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("unsupported-access", "hash-unsupported-access"))
        .await
        .unwrap();

    assert!(matches!(first, RemoteAcquireOutcome::NoCandidate { .. }));
    sqlx::query(
        "UPDATE worker_capabilities \
         SET artifact_access = ? \
         WHERE worker_id = ? AND operation = ?",
    )
    .bind(serde_json::to_string(&vec!["shared_mount"]).unwrap())
    .bind(i64::try_from(fixture.worker_id.0).unwrap())
    .bind(OP)
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();

    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input("unsupported-access", "hash-unsupported-access"))
        .await
        .unwrap();

    assert_eq!(replay, first);
    assert_eq!(
        fixture
            .cp
            .tickets()
            .get(ticket_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );
    assert_eq!(count(&fixture.cp, EventKind::LeaseAcquired).await, 0);
}

#[tokio::test]
async fn remote_authentication_failures_share_unauthorized_error() {
    let missing_node = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let mut missing_node_input = missing_node.acquire_input("missing-node", "hash-missing-node");
    missing_node_input.node_id = NodeId(u64::MAX);
    let missing_node_error = missing_node
        .cp
        .remote_acquire(missing_node_input)
        .await
        .unwrap_err();

    let local_node = fixture_with_options(
        NodeKind::Local,
        WorkerKind::Remote,
        &[(OP, vec!["shared_mount"])],
        &[OP],
        &[],
    )
    .await;
    let local_node_error = local_node
        .cp
        .remote_acquire(local_node.acquire_input("local-node-auth", "hash-local-node-auth"))
        .await
        .unwrap_err();

    let wrong_token = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let mut wrong_token_input = wrong_token.acquire_input("wrong-token", "hash-wrong-token");
    wrong_token_input.token = secrecy::SecretString::from("incorrect-token");
    let wrong_token_error = wrong_token
        .cp
        .remote_acquire(wrong_token_input)
        .await
        .unwrap_err();

    for error in [&missing_node_error, &local_node_error, &wrong_token_error] {
        assert_eq!(error.error_code(), ErrorCode::Unauthorized);
        assert_eq!(
            error.to_string(),
            "unauthorized: remote node authentication failed"
        );
    }
}

#[tokio::test]
async fn remote_acquire_requires_remote_node_and_worker_kind() {
    let local_node = fixture_with_options(
        NodeKind::Local,
        WorkerKind::Remote,
        &[(OP, vec!["shared_mount"])],
        &[OP],
        &[],
    )
    .await;
    local_node.ready_ticket(OP).await;
    let err = local_node
        .cp
        .remote_acquire(local_node.acquire_input("local-node", "hash-local-node"))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Unauthorized);

    let local_worker = fixture_with_options(
        NodeKind::Remote,
        WorkerKind::Local,
        &[(OP, vec!["shared_mount"])],
        &[OP],
        &[],
    )
    .await;
    let ticket_id = local_worker.ready_ticket(OP).await;
    let err = local_worker
        .cp
        .remote_acquire(local_worker.acquire_input("local-worker", "hash-local-worker"))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(
        local_worker
            .cp
            .tickets()
            .get(ticket_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );
}

#[tokio::test]
async fn remote_acquire_skips_ineligible_higher_priority_work_for_eligible_ticket() {
    let fixture = remote_fixture(
        &[
            ("test.denied", vec!["shared_mount"]),
            ("test.allowed", vec!["shared_mount"]),
        ],
        &["test.denied", "test.allowed"],
        &["test.denied"],
    )
    .await;
    fixture.ready_ticket_with_priority("test.denied", 10).await;
    let eligible_ticket = fixture.ready_ticket_with_priority("test.allowed", 0).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("skip-denied", "hash-skip-denied"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected eligible lower-priority lease");
    };
    assert_eq!(dispatch.ticket_id, eligible_ticket);
    assert_eq!(dispatch.operation, "test.allowed");
}

const WF_OP: &str = "synthetic.workflow.operation.transcode_video";

/// A ready byte-touching ticket whose canonical declaration names the supplied
/// rendered source, as the workflow planner would persist it.
async fn ready_workflow_ticket(
    fixture: &RemoteFixture,
    priority: i64,
    source_storage_root_id: u64,
    source_location_id: u64,
) -> TicketId {
    let payload = WorkflowTicketPayload::new_for_test(
        "wf-owner-local",
        "plan-owner-local",
        "node-owner-local",
        "branch-owner-local",
        OperationKind::TranscodeVideo,
        json!({
            "operation": "transcode_video",
            "source_storage_root_id": source_storage_root_id,
            "source_location_id": source_location_id,
        }),
    )
    .to_ticket_payload()
    .unwrap();
    let ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op(WF_OP),
            priority,
            payload,
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(ticket.id, T0)
        .await
        .unwrap();
    ticket.id
}

#[tokio::test]
async fn remote_acquire_skips_unresolvable_declaration_and_preserves_order() {
    // The scoring path matches capability rows by ticket kind; the lease write
    // path rechecks under the operation's matching token, so both encodings are
    // registered.
    let fixture = remote_fixture(
        &[
            (WF_OP, vec!["shared_mount"]),
            ("transcode_video", vec!["shared_mount"]),
        ],
        &[WF_OP, "transcode_video"],
        &[],
    )
    .await;
    let root = create_remote_scan_root(&fixture, "owner-local-order").await;
    let location = seed_scan_location(&fixture.cp, root, "owner-local-order.mkv").await;

    // The higher-priority ticket declares a location that names no row: rejected
    // before scoring, so the lower-priority owner-local ticket leases.
    ready_workflow_ticket(&fixture, 10, 999_999_999, 999_999_998).await;
    let real = ready_workflow_ticket(&fixture, 0, root.0, location).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("gate-order", "hash-gate-order"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected the resolvable lower-priority ticket to lease");
    };
    assert_eq!(dispatch.ticket_id, real);
}

#[tokio::test]
async fn remote_acquire_rejects_byte_work_owned_by_another_node() {
    let fixture = remote_fixture(
        &[
            (WF_OP, vec!["shared_mount"]),
            ("transcode_video", vec!["shared_mount"]),
        ],
        &[WF_OP, "transcode_video"],
        &[],
    )
    .await;

    // A real, live root and location owned by a different node (the shared test
    // root's owner 9000001), so only the owner check can reject this candidate.
    voom_store::test_support::seed_test_rooted_location(fixture.cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO node_incarnations (incarnation_id, node_id, status, started_at, last_seen_at) \
         VALUES ('inc-9000001', 9000001, 'active', '1970-01-01T00:00:00Z', '1970-01-01T00:00:00Z')",
    )
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
    ready_workflow_ticket(&fixture, 0, 9_000_001, 9_000_001).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("gate-foreign-owner", "hash-gate-foreign-owner"))
        .await
        .unwrap();
    assert!(
        matches!(outcome, RemoteAcquireOutcome::Idle { .. }),
        "non-owner byte work must not become schedulable: {outcome:?}"
    );
    let leased: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(fixture.cp.pool_for_test())
        .await
        .unwrap();
    assert_eq!(leased, 0);
}

#[tokio::test]
async fn remote_acquire_invalid_ttl_is_idempotent_and_does_not_lease() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let ticket_id = fixture.ready_ticket(OP).await;

    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input_with_ttl("bad-ttl", "hash-a", 0))
        .await
        .unwrap_err();
    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input_with_ttl("bad-ttl", "hash-a", 0))
        .await
        .unwrap_err();
    let conflict = fixture
        .cp
        .remote_acquire(fixture.acquire_input_with_ttl("bad-ttl", "hash-b", 60))
        .await
        .unwrap_err();

    assert_eq!(first.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(replay.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(conflict.error_code(), ErrorCode::Conflict);
    assert_eq!(
        fixture
            .cp
            .tickets()
            .get(ticket_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Ready
    );
    assert_eq!(count(&fixture.cp, EventKind::LeaseAcquired).await, 0);
}

#[tokio::test]
async fn remote_complete_reuses_success_path_and_replays_same_idempotency_key() {
    let fixture = leased_fixture().await;
    let complete =
        fixture.complete_input(fixture_lease_id(&fixture).await, "complete-ok", "hash-1");

    let first = fixture.cp.remote_complete(complete.clone()).await.unwrap();
    let second = fixture.cp.remote_complete(complete).await.unwrap();

    assert_eq!(second, first);
    assert_eq!(count(&fixture.cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&fixture.cp, EventKind::TicketSucceeded).await, 1);

    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(first.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.status, ArtifactAccessPlanStatus::Consumed);
}

#[tokio::test]
async fn remote_complete_rejects_incomplete_or_mismatched_artifact_evidence() {
    // The plan proves absence of declared byte work (declaration-free ticket),
    // so an echo claiming any owner or evidence is a forgery.
    let claiming_owner = leased_fixture().await;
    let claiming_owner_lease = fixture_lease_id(&claiming_owner).await;
    let mut owner_input =
        claiming_owner.complete_input(claiming_owner_lease, "owner-forgery", "hash-owner");
    owner_input.result = json!({
        "ok": true,
        "artifact_access": {"validated": true, "owner_node_id": 1}
    });
    let err = claiming_owner
        .cp
        .remote_complete(owner_input)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(
        claiming_owner
            .cp
            .leases()
            .get(claiming_owner_lease)
            .await
            .unwrap()
            .unwrap()
            .released_at,
        None
    );

    let claiming_evidence = leased_fixture().await;
    let claiming_evidence_lease = fixture_lease_id(&claiming_evidence).await;
    let mut evidence_input = claiming_evidence.complete_input(
        claiming_evidence_lease,
        "evidence-forgery",
        "hash-evidence",
    );
    evidence_input.result = json!({
        "ok": true,
        "artifact_access": {"validated": true, "access_evidence": {"declaration": []}}
    });
    let err = claiming_evidence
        .cp
        .remote_complete(evidence_input)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(
        count(&claiming_evidence.cp, EventKind::TicketSucceeded).await,
        0
    );

    // A missing validation marker stays rejected regardless of plan shape.
    let unvalidated = leased_fixture().await;
    let unvalidated_lease_id = fixture_lease_id(&unvalidated).await;
    let mut missing_input =
        unvalidated.complete_input(unvalidated_lease_id, "missing-evidence", "hash-missing");
    missing_input.result = json!({"ok": true});
    let err = unvalidated
        .cp
        .remote_complete(missing_input)
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);
}

#[tokio::test]
async fn remote_heartbeat_rejects_stale_node_and_lease_heartbeat() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    fixture
        .cp
        .mark_stale_nodes(T0 + Duration::seconds(61))
        .await
        .unwrap();

    let err = fixture
        .cp
        .remote_acquire(fixture.acquire_input("stale-acquire", "hash-stale-acquire"))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);

    let heartbeat = fixture
        .cp
        .remote_node_heartbeat(fixture.node_heartbeat_input("node-heartbeat", "hash-node-hb"))
        .await
        .unwrap_err();
    assert_eq!(heartbeat.error_code(), ErrorCode::Conflict);

    let first = fixture
        .cp
        .remote_lease_heartbeat(fixture.lease_heartbeat_input(
            lease_id,
            "lease-heartbeat",
            "hash-lease-hb",
        ))
        .await
        .unwrap_err();
    let replay = fixture
        .cp
        .remote_lease_heartbeat(fixture.lease_heartbeat_input(
            lease_id,
            "lease-heartbeat",
            "hash-lease-hb",
        ))
        .await
        .unwrap_err();

    assert_eq!(first.error_code(), ErrorCode::Conflict);
    assert_eq!(replay.error_code(), ErrorCode::Conflict);
    assert_eq!(count(&fixture.cp, EventKind::LeaseReleased).await, 0);
}

#[tokio::test]
async fn remote_lease_heartbeat_invalid_ttl_is_idempotent_and_does_not_move_expiry() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    let before = fixture.cp.leases().get(lease_id).await.unwrap().unwrap();

    let mut input = fixture.lease_heartbeat_input(lease_id, "bad-heartbeat-ttl", "hash-a");
    input.lease_ttl_seconds = 0;
    let first = fixture
        .cp
        .remote_lease_heartbeat(input.clone())
        .await
        .unwrap_err();
    let replay = fixture.cp.remote_lease_heartbeat(input).await.unwrap_err();
    let mut different = fixture.lease_heartbeat_input(lease_id, "bad-heartbeat-ttl", "hash-b");
    different.lease_ttl_seconds = 60;
    let conflict = fixture
        .cp
        .remote_lease_heartbeat(different)
        .await
        .unwrap_err();
    let after = fixture.cp.leases().get(lease_id).await.unwrap().unwrap();

    assert_eq!(first.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(replay.error_code(), ErrorCode::ConfigInvalid);
    assert_eq!(conflict.error_code(), ErrorCode::Conflict);
    assert_eq!(after.last_heartbeat_at, before.last_heartbeat_at);
    assert_eq!(after.expires_at, before.expires_at);
}

#[tokio::test]
async fn remote_complete_replay_is_fenced_after_node_retirement() {
    let fixture = leased_fixture().await;
    let complete = fixture.complete_input(
        fixture_lease_id(&fixture).await,
        "complete-before-retire",
        "hash-1",
    );

    fixture.cp.remote_complete(complete.clone()).await.unwrap();
    let node = fixture.cp.get_node(fixture.node_id).await.unwrap().unwrap();
    fixture
        .cp
        .retire_node(fixture.node_id, node.epoch, T0)
        .await
        .unwrap();

    let replay = fixture.cp.remote_complete(complete).await.unwrap_err();

    assert_eq!(replay.error_code(), ErrorCode::Conflict);
    assert_eq!(count(&fixture.cp, EventKind::LeaseReleased).await, 1);
}

#[tokio::test]
async fn remote_fail_marks_artifact_plan_and_replays_without_second_mutation() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    let fail = fixture.fail_input(lease_id, "fail-artifact", "hash-fail");

    let first = fixture.cp.remote_fail(fail.clone()).await.unwrap();
    let replay = fixture.cp.remote_fail(fail).await.unwrap();

    assert_eq!(replay, first);
    assert_eq!(count(&fixture.cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(
        count(&fixture.cp, EventKind::TicketFailedRetriable).await,
        1
    );
    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.status, ArtifactAccessPlanStatus::Rejected);
}

#[tokio::test]
async fn remote_fail_marks_timeouts_and_crashes_as_failed_even_with_artifact_reason() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;

    fixture
        .cp
        .remote_fail(RemoteFailInput {
            node_id: fixture.node_id,
            token: fixture.token.clone(),
            incarnation_id: fixture.incarnation_id,
            worker_id: fixture.worker_id,
            lease_id,
            idempotency_key: "fail-timeout".to_owned(),
            request_hash: "hash-fail-timeout".to_owned(),
            reason: "artifact upload timed out".to_owned(),
            class: FailureClass::WorkerTimeout,
            evidence: json!({"timeout": true}),
        })
        .await
        .unwrap();

    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.status, ArtifactAccessPlanStatus::Failed);
}

#[tokio::test]
async fn remote_recover_marks_stale_nodes_and_expires_due_leases() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    let running_root = create_remote_scan_root(&fixture, "heartbeat-running").await;
    let running = fixture
        .cp
        .request_scan_session(running_root, 300)
        .await
        .unwrap();
    fixture
        .cp
        .start_scan_session(RemoteScanStartInput {
            node_id: fixture.node_id,
            scan_session_id: running.id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: "heartbeat-scan-start".to_owned(),
            request_hash: "heartbeat-scan-start-body".to_owned(),
        })
        .await
        .unwrap();
    let requested_root = create_remote_scan_root(&fixture, "heartbeat-requested").await;
    let requested = fixture
        .cp
        .request_scan_session(requested_root, 30)
        .await
        .unwrap();

    let report = fixture
        .cp
        .remote_recover(T0 + Duration::seconds(61))
        .await
        .unwrap();

    assert_eq!(report.stale_nodes, vec![fixture.node_id]);
    assert_eq!(report.stale_scan_sessions, vec![requested.id]);
    assert_eq!(report.expired_leases, vec![lease_id]);
    assert!(!report.requeued_tickets.is_empty());
    assert_eq!(count(&fixture.cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(
        count(&fixture.cp, EventKind::TicketRequeuedAfterLeaseExpiry).await,
        1
    );
    assert_eq!(
        fixture.cp.scan_session(running.id).await.unwrap().status,
        ScanSessionStatus::Stale
    );
    let recovery_order: Vec<String> = sqlx::query_scalar(
        "SELECT kind FROM events WHERE kind IN ('scan_session.stale', 'lease.expired') \
         ORDER BY event_id ASC",
    )
    .fetch_all(fixture.cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(
        recovery_order,
        vec!["scan_session.stale", "scan_session.stale", "lease.expired"]
    );
}

#[tokio::test]
async fn remote_recover_marks_scan_sessions_stale() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let root_id = create_remote_scan_root(&fixture, "timeout-running").await;
    let location_id = seed_scan_location(&fixture.cp, root_id, "old.mkv").await;
    let requested = fixture.cp.request_scan_session(root_id, 10).await.unwrap();
    fixture
        .cp
        .start_scan_session(RemoteScanStartInput {
            node_id: fixture.node_id,
            scan_session_id: requested.id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: "recover-scan-start".to_owned(),
            request_hash: "recover-scan-start-body".to_owned(),
        })
        .await
        .unwrap();
    fixture
        .cp
        .accept_scan_observation_batch(RemoteScanBatchInput {
            node_id: fixture.node_id,
            scan_session_id: requested.id,
            incarnation_id: fixture.incarnation_id,
            token: fixture.token.clone(),
            idempotency_key: "recover-scan-batch".to_owned(),
            request_hash: "a".repeat(64),
            sequence: 0,
            observations: vec![ScanObservation {
                provider_relative_locator: ProviderRelativeLocator::new("old.mkv".to_owned())
                    .unwrap(),
                provider_object_identity: "recover-object".to_owned(),
                size_bytes: 1,
                modified_at: T0,
                stability_started_at: T0,
                stability_confirmed_at: T0,
            }],
        })
        .await
        .unwrap();
    let requested_root = create_remote_scan_root(&fixture, "timeout-requested").await;
    let requested_only = fixture
        .cp
        .request_scan_session(requested_root, 10)
        .await
        .unwrap();

    let report = fixture
        .cp
        .remote_recover(T0 + Duration::seconds(10))
        .await
        .unwrap();

    assert_eq!(
        report.stale_scan_sessions,
        vec![requested.id, requested_only.id]
    );
    for id in [requested.id, requested_only.id] {
        assert_eq!(
            fixture.cp.scan_session(id).await.unwrap().status,
            ScanSessionStatus::Stale
        );
    }
    assert_eq!(
        scan_location_state(&fixture.cp, location_id).await,
        (None, 0, None)
    );
    assert_eq!(scan_root_pointer(&fixture.cp, root_id).await, None);
    let stale_events = count(&fixture.cp, EventKind::ScanSessionStale).await;
    let rerun = fixture
        .cp
        .remote_recover(T0 + Duration::seconds(10))
        .await
        .unwrap();
    assert!(rerun.stale_scan_sessions.is_empty());
    assert_eq!(
        count(&fixture.cp, EventKind::ScanSessionStale).await,
        stale_events
    );
}

#[tokio::test]
async fn remote_complete_same_key_different_body_rejects_without_second_mutation() {
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    fixture
        .cp
        .remote_complete(fixture.complete_input(lease_id, "complete-conflict", "hash-1"))
        .await
        .unwrap();

    let err = fixture
        .cp
        .remote_complete(fixture.complete_input(lease_id, "complete-conflict", "hash-2"))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(count(&fixture.cp, EventKind::LeaseReleased).await, 1);
    assert_eq!(count(&fixture.cp, EventKind::TicketSucceeded).await, 1);
}

async fn leased_fixture() -> RemoteFixture {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;
    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("leased-fixture", "hash-acquire"))
        .await
        .unwrap();
    assert!(matches!(outcome, RemoteAcquireOutcome::Leased(_)));
    fixture
}

async fn fixture_lease_id(fixture: &RemoteFixture) -> LeaseId {
    let leases = sqlx::query_scalar::<_, i64>("SELECT id FROM leases ORDER BY id DESC LIMIT 1")
        .fetch_one(fixture.cp.pool_for_test())
        .await
        .unwrap();
    LeaseId(u64::try_from(leases).unwrap())
}

async fn seed_legacy_acquire_replay(
    fixture: &RemoteFixture,
    idempotency_key: &str,
    request_hash: &str,
    data: serde_json::Value,
) {
    let response = serde_json::to_string(&RemoteMutationReplay::Ok { data }).unwrap();
    sqlx::query(
        "INSERT INTO remote_idempotency_keys \
         (node_id, route_key, worker_scope_id, worker_id, idempotency_key, request_hash, \
          response_json, status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'completed', '1970-01-01T00:00:00Z')",
    )
    .bind(i64::try_from(fixture.node_id.0).unwrap())
    .bind(ROUTE_ACQUIRE)
    .bind(i64::try_from(fixture.worker_id.0).unwrap())
    .bind(i64::try_from(fixture.worker_id.0).unwrap())
    .bind(incarnation_replay_key(
        fixture.incarnation_id,
        idempotency_key,
    ))
    .bind(request_hash)
    .bind(response)
    .execute(fixture.cp.pool_for_test())
    .await
    .unwrap();
}

async fn stored_replay(fixture: &RemoteFixture, key: &str) -> RemoteMutationReplay {
    let json: String = sqlx::query_scalar(
        "SELECT response_json FROM remote_idempotency_keys WHERE idempotency_key = ?",
    )
    .bind(incarnation_replay_key(fixture.incarnation_id, key))
    .fetch_one(fixture.cp.pool_for_test())
    .await
    .unwrap();
    serde_json::from_str(&json).unwrap()
}

async fn remote_fixture(
    capabilities: &[(&str, Vec<&str>)],
    can_execute: &[&str],
    denies: &[&str],
) -> RemoteFixture {
    fixture_with_options(
        NodeKind::Remote,
        WorkerKind::Remote,
        capabilities,
        can_execute,
        denies,
    )
    .await
}

async fn create_remote_scan_root(fixture: &RemoteFixture, suffix: &str) -> StorageRootId {
    let library = fixture
        .cp
        .create_library(NewLibrary {
            slug: format!("remote-recover-{suffix}"),
            display_name: format!("Remote recover {suffix}"),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = fixture
        .cp
        .create_library_root(remote_scan_root_input(library.id, fixture.node_id, suffix))
        .await
        .unwrap();
    fixture
        .cp
        .activate_library_root(root.id, format!("remote-recover-{suffix}"))
        .await
        .unwrap();
    root.id
}

fn remote_scan_root_input(
    library_id: LibraryId,
    owner_node_id: NodeId,
    suffix: &str,
) -> NewLibraryRoot {
    NewLibraryRoot {
        library_id,
        owner_node_id,
        provider_kind: StorageProviderKind::LocalFilesystem,
        provider_locator: ProviderLocator::new(format!("/remote-recover/{suffix}")).unwrap(),
        display_locator: format!("/remote-recover/{suffix}"),
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        extension_allowlist: Vec::new(),
        scan_mode: LibraryScanMode::ManualRecursive,
        symlink_policy: SymlinkPolicy::Reject,
        hidden_file_policy: HiddenFilePolicy::Ignore,
        max_depth: None,
        stability_seconds: 0,
        debounce_seconds: 0,
        default_output_root_id: None,
        default_staging_root_id: None,
        default_backup_root_id: None,
        enabled: true,
    }
}

async fn seed_scan_location(
    cp: &crate::ControlPlane,
    storage_root_id: StorageRootId,
    locator: &str,
) -> u64 {
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(cp.pool_for_test())
        .await
        .unwrap()
        .last_insert_rowid();
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(asset_id)
    .bind(format!("remote-recover-{locator}"))
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    let location_id = sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, observed_at, epoch) \
         VALUES (?, 'rooted', ?, ?, '1970-01-01T00:00:00Z', 0)",
    )
    .bind(version_id)
    .bind(i64::try_from(storage_root_id.0).unwrap())
    .bind(locator)
    .execute(cp.pool_for_test())
    .await
    .unwrap()
    .last_insert_rowid();
    u64::try_from(location_id).unwrap()
}

async fn scan_location_state(
    cp: &crate::ControlPlane,
    location_id: u64,
) -> (Option<String>, i64, Option<i64>) {
    sqlx::query_as(
        "SELECT retired_at, epoch, retired_by_scan_session_id \
         FROM file_locations WHERE id = ?",
    )
    .bind(i64::try_from(location_id).unwrap())
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap()
}

async fn scan_root_pointer(cp: &crate::ControlPlane, root_id: StorageRootId) -> Option<i64> {
    sqlx::query_scalar("SELECT last_scan_session_id FROM library_roots WHERE id = ?")
        .bind(i64::try_from(root_id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
}

async fn fixture_with_options(
    node_kind: NodeKind,
    worker_kind: WorkerKind,
    capabilities: &[(&str, Vec<&str>)],
    can_execute: &[&str],
    denies: &[&str],
) -> RemoteFixture {
    let (cp, tmp) = cp_at(T0).await;
    let registered = cp
        .register_node(node_input("remote-node", node_kind))
        .await
        .unwrap();
    let worker = cp
        .register_worker_for_node(RegisterWorkerForNodeInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            name: "remote-worker".to_owned(),
            kind: worker_kind,
            capabilities: capabilities
                .iter()
                .map(|(operation, artifact_access)| NewWorkerCapabilityDraft {
                    operation: ticket_op(operation),
                    codecs: vec!["json".to_owned()],
                    hardware: Vec::new(),
                    artifact_access: artifact_access
                        .iter()
                        .map(|mode| (*mode).to_owned())
                        .collect(),
                    extra: json!({}),
                })
                .collect(),
            grants: vec![NewWorkerGrantDraft {
                can_execute: can_execute.iter().map(|op| ticket_op(op)).collect(),
                can_access_read: Vec::new(),
                can_access_write: Vec::new(),
                denies: denies.iter().map(|op| ticket_op(op)).collect(),
                max_parallel: json!({"*": 1}),
            }],
        })
        .await
        .unwrap();
    let incarnation_id: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse().unwrap();
    let mut tx = cp.pool_for_test().begin().await.unwrap();
    cp.node_incarnations
        .insert_in_tx(
            &mut tx,
            NewNodeIncarnation {
                id: incarnation_id,
                node_id: registered.node.id,
                started_at: T0,
            },
        )
        .await
        .unwrap();
    cp.nodes
        .activate_incarnation_in_tx(&mut tx, registered.node.id, None, incarnation_id, T0)
        .await
        .unwrap();
    cp.workers
        .bind_incarnation_in_tx(&mut tx, worker.id, registered.node.id, incarnation_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    RemoteFixture {
        cp,
        _tmp: tmp,
        node_id: registered.node.id,
        token: registered.token,
        incarnation_id,
        worker_id: worker.id,
    }
}

async fn cp_at(now: OffsetDateTime) -> (crate::ControlPlane, voom_test_support::TempDatabase) {
    let tmp = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        std::sync::Arc::new(FrozenClock::new(now)),
        std::sync::Arc::new(std::sync::Mutex::new(
            voom_core::rng_test_support::FrozenRng::new(0x0808_0808),
        )),
    )
    .await
    .unwrap();
    (cp, tmp)
}

fn remote_node_input(name: &str) -> RegisterNodeInput {
    node_input(name, NodeKind::Remote)
}

fn node_input(name: &str, kind: NodeKind) -> RegisterNodeInput {
    RegisterNodeInput {
        name: name.to_owned(),
        kind,
        heartbeat_ttl_seconds: 60,
        metadata: json!({}),
    }
}

// --- Issue #478: atomic owner-local acquisition and changed-gate decisions ---

#[tokio::test]
async fn remote_acquire_leased_binds_one_lease_plan_and_decision_atomically() {
    let fixture = remote_fixture(
        &[
            (WF_OP, vec!["shared_mount"]),
            ("transcode_video", vec!["shared_mount"]),
        ],
        &[WF_OP, "transcode_video"],
        &[],
    )
    .await;
    let root = create_remote_scan_root(&fixture, "atomic-bind").await;
    let location = seed_scan_location(&fixture.cp, root, "atomic-bind.mkv").await;
    let ticket = ready_workflow_ticket(&fixture, 0, root.0, location).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("atomic-bind", "hash-atomic-bind"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected the owner-local ticket to lease: {outcome:?}");
    };
    assert_eq!(dispatch.ticket_id, ticket);
    // Criterion 4: the namespaced byte-work ticket dispatches under its bare
    // wire token, exactly like the canonical encoding.
    assert_eq!(dispatch.operation, "transcode_video");
    assert!(dispatch.artifact_access_plan.owner_node_id.is_some());
    assert!(dispatch.artifact_access_plan.access_evidence.is_some());

    let pool = fixture.cp.pool_for_test();
    // Criterion 3: exactly one lease, bound to the exact plan and decision.
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 1);
    let plan: (i64, Option<i64>, String, i64, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT p.id, p.owner_node_id, p.access_evidence, p.lease_id, p.ticket_id, \
                p.worker_id, p.node_id, p.status \
         FROM artifact_access_plans p",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(plan.7.as_deref(), Some("selected"));
    assert_eq!(plan.3, i64::try_from(dispatch.lease_id.0).unwrap());
    assert_eq!(plan.4, i64::try_from(ticket.0).unwrap());
    assert_eq!(plan.5, i64::try_from(fixture.worker_id.0).unwrap());
    assert_eq!(plan.6, i64::try_from(fixture.node_id.0).unwrap());
    let owner_node_id = plan.1.unwrap();
    assert_ne!(owner_node_id, 0, "owner-local plan names its owner");
    assert_eq!(owner_node_id, i64::try_from(fixture.node_id.0).unwrap());
    let plan_evidence: serde_json::Value = serde_json::from_str(&plan.2).unwrap();

    let decision = fixture
        .cp
        .scheduler_decision(dispatch.scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.outcome, SchedulerDecisionOutcome::Selected);
    assert_eq!(
        decision.selected_lease_id,
        Some(dispatch.lease_id),
        "the selected decision names its lease at creation"
    );
    assert_eq!(decision.ticket_id, Some(ticket));
    assert_eq!(decision.selected_worker_id, Some(fixture.worker_id));
    assert_eq!(decision.selected_node_id, Some(fixture.node_id));
    let voom_store::repo::execution::scheduler_decisions::SchedulerDecision {
        access_evidence: decision_evidence,
        ..
    } = decision;
    let Some(voom_core::owner_access_evidence::DecisionAccessEvidence::Owner(evidence)) =
        decision_evidence
    else {
        panic!("selected byte-work decision carries owner evidence");
    };
    // Plan and decision carry the same canonical access evidence.
    assert_eq!(
        serde_json::to_value(&evidence).unwrap(),
        plan_evidence,
        "plan and decision bind the identical canonical evidence"
    );
}

#[tokio::test]
async fn remote_acquire_changed_gate_missing_capability_decides_without_leasing() {
    // The worker advertises and is granted only the namespaced encoding, so
    // candidate scoring (raw-token lookup) selects the ticket while the lease
    // write path rechecks under the bare matching token. The changed gate must
    // decide — one durable no-candidate row — instead of raising, and must
    // leave zero leases and zero bound access plans.
    let fixture = remote_fixture(&[(WF_OP, vec!["shared_mount"])], &[WF_OP], &[]).await;
    let root = create_remote_scan_root(&fixture, "changed-gate").await;
    let location = seed_scan_location(&fixture.cp, root, "changed-gate.mkv").await;
    let ticket = ready_workflow_ticket(&fixture, 0, root.0, location).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("changed-gate", "hash-changed-gate"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::NoCandidate {
        worker_id: _,
        scheduler_decision_id,
    } = outcome
    else {
        panic!("expected the changed gate to reject without leasing: {outcome:?}");
    };

    let decision = fixture
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        decision.outcome,
        SchedulerDecisionOutcome::NoEligibleCandidate
    );
    assert_eq!(decision.reason_code, SchedulerReasonCode::MissingCapability);
    assert_eq!(decision.ticket_id, Some(ticket));
    assert_eq!(decision.candidate_count, 1);
    let key = decision.suppression_key.unwrap();
    assert!(!key.is_empty(), "changed-gate decisions are suppressed");
    assert!(
        key.contains(&format!(":ticket:{}:", ticket.0)),
        "suppression key names the ticket: {key}"
    );
    assert!(
        key.contains("reason:missing_capability"),
        "suppression key carries the documented stable reason: {key}"
    );
    let explanation = decision.explanation;
    assert_eq!(explanation["reason"], "missing_capability");
    assert_eq!(explanation["operation"], "transcode_video");

    let pool = fixture.cp.pool_for_test();
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 0, "a changed gate never leases");
    let plan_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_access_plans")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(plan_count, 0, "a changed gate never binds an access plan");
    let selected_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scheduler_decisions WHERE outcome = 'selected'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(selected_count, 0, "a changed gate never leaves a selection");
    // The rejected ticket stays ready for a correctly-configured worker.
    let state: String = sqlx::query_scalar("SELECT state FROM tickets WHERE id = ?")
        .bind(i64::try_from(ticket.0).unwrap())
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(state, "ready");
}

#[tokio::test]
async fn remote_acquire_changed_gate_missing_grant_and_denied_decide_with_documented_reasons() {
    // Capability rows exist for both encodings but the grant only covers the
    // namespaced one, so candidate scoring (raw-token lookup) selects the
    // namespaced byte-work ticket while the lease path rechecks the bare
    // matching token and finds no grant.
    let fixture = remote_fixture(
        &[
            (WF_OP, vec!["shared_mount"]),
            ("transcode_video", vec!["shared_mount"]),
        ],
        &[WF_OP],
        &[],
    )
    .await;
    let root = create_remote_scan_root(&fixture, "changed-grant").await;
    let location = seed_scan_location(&fixture.cp, root, "changed-grant.mkv").await;
    let ticket = ready_workflow_ticket(&fixture, 0, root.0, location).await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("changed-grant", "hash-changed-grant"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::NoCandidate {
        scheduler_decision_id,
        ..
    } = outcome
    else {
        panic!("expected the missing grant to decide: {outcome:?}");
    };
    let decision = fixture
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code, SchedulerReasonCode::MissingGrant);
    assert_eq!(decision.ticket_id, Some(ticket));
    let pool = fixture.cp.pool_for_test();
    let lease_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(lease_count, 0);
}

#[tokio::test]
async fn remote_acquire_dispatches_custom_local_operations_under_their_exact_token() {
    let fixture = remote_fixture(&[("disk.test", vec!["shared_mount"])], &["disk.test"], &[]).await;
    fixture.ready_ticket("disk.test").await;

    let outcome = fixture
        .cp
        .remote_acquire(fixture.acquire_input("custom-local", "hash-custom-local"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected the custom local ticket to lease: {outcome:?}");
    };
    // Criterion 4: an exact custom local operation keeps its exact token.
    assert_eq!(dispatch.operation, "disk.test");
    // No declared byte work: the plan proves absence with the NULL pair.
    assert_eq!(dispatch.artifact_access_plan.owner_node_id, None);
    assert_eq!(dispatch.artifact_access_plan.access_evidence, None);
}

#[test]
fn changed_gate_outcomes_map_to_the_documented_stable_reasons() {
    use super::acquire::{changed_gate_explanation, outcome_reason_code};
    use voom_store::repo::execution::leases::{
        LeaseAcquireOutcome, LeaseIneligibilityReason, WorkerCapacitySaturation,
    };

    let cases: [(LeaseAcquireOutcome, SchedulerReasonCode); 6] = [
        (
            LeaseAcquireOutcome::TicketNotReady {
                ticket_id: TicketId(1),
            },
            SchedulerReasonCode::NoReadyTicket,
        ),
        (
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id: WorkerId(2),
                operation: ticket_op("probe_file"),
                reason: LeaseIneligibilityReason::WorkerStale,
            },
            SchedulerReasonCode::WorkerNotExecutable,
        ),
        (
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id: WorkerId(2),
                operation: ticket_op("probe_file"),
                reason: LeaseIneligibilityReason::WorkerRetired,
            },
            SchedulerReasonCode::WorkerNotExecutable,
        ),
        (
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id: WorkerId(2),
                operation: ticket_op("probe_file"),
                reason: LeaseIneligibilityReason::OperationDenied,
            },
            SchedulerReasonCode::OperationDenied,
        ),
        (
            LeaseAcquireOutcome::WorkerIneligible {
                worker_id: WorkerId(2),
                operation: ticket_op("probe_file"),
                reason: LeaseIneligibilityReason::MissingCapability,
            },
            SchedulerReasonCode::MissingCapability,
        ),
        (
            LeaseAcquireOutcome::CapacityFull(WorkerCapacitySaturation {
                worker_id: WorkerId(2),
                operation: ticket_op("probe_file"),
                active_leases: 1,
                max_parallel: 1,
            }),
            SchedulerReasonCode::WorkerCapacityFull,
        ),
    ];
    for (outcome, expected) in cases {
        assert_eq!(outcome_reason_code(&outcome), expected, "{outcome:?}");
        let explanation = changed_gate_explanation(&outcome, expected);
        assert_eq!(explanation["reason"], expected.as_str());
        assert_eq!(explanation["outcome"], "no_eligible_candidate");
        assert!(explanation.get("scoring_version").is_some());
    }
}

// ---- #479: terminal-safe owner-local acquisition replay ----

fn leased_dispatch(outcome: &RemoteAcquireOutcome) -> &RemoteLeaseDispatch {
    let RemoteAcquireOutcome::Leased(dispatch) = outcome else {
        panic!("expected a leased acquire outcome, got {outcome:?}");
    };
    dispatch
}

async fn evidence_counts(fixture: &RemoteFixture) -> (i64, i64, i64, i64) {
    let pool = fixture.cp.pool_for_test();
    let leases: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leases")
        .fetch_one(pool)
        .await
        .unwrap();
    let plans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artifact_access_plans")
        .fetch_one(pool)
        .await
        .unwrap();
    let decisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduler_decisions")
        .fetch_one(pool)
        .await
        .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(pool)
        .await
        .unwrap();
    (leases, plans, decisions, events)
}

/// The stored `data` payload of a completed acquire replay row.
async fn stored_acquire_data(fixture: &RemoteFixture, key: &str) -> serde_json::Value {
    let json: String = sqlx::query_scalar(
        "SELECT response_json FROM remote_idempotency_keys WHERE idempotency_key = ?",
    )
    .bind(incarnation_replay_key(fixture.incarnation_id, key))
    .fetch_one(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let mut envelope: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(envelope["status"], json!("ok"));
    envelope.get_mut("data").unwrap().take()
}

async fn overwrite_acquire_data(fixture: &RemoteFixture, key: &str, data: serde_json::Value) {
    let json: String = sqlx::query_scalar(
        "SELECT response_json FROM remote_idempotency_keys WHERE idempotency_key = ?",
    )
    .bind(incarnation_replay_key(fixture.incarnation_id, key))
    .fetch_one(fixture.cp.pool_for_test())
    .await
    .unwrap();
    let mut envelope: serde_json::Value = serde_json::from_str(&json).unwrap();
    envelope["data"] = data;
    sqlx::query("UPDATE remote_idempotency_keys SET response_json = ? WHERE idempotency_key = ?")
        .bind(envelope.to_string())
        .bind(incarnation_replay_key(fixture.incarnation_id, key))
        .execute(fixture.cp.pool_for_test())
        .await
        .unwrap();
}

/// A byte-work fixture: live root owned by the acquiring node plus one rooted
/// location, so the declared declaration resolves owner-local (the control-plane
/// twin of the API route's owner-local seeding).
async fn byte_work_fixture() -> (RemoteFixture, StorageRootId, u64) {
    let namespaced = "synthetic.workflow.operation.transcode_video";
    let fixture = remote_fixture(
        &[
            (namespaced, vec!["shared_mount"]),
            ("transcode_video", vec!["shared_mount"]),
        ],
        &[namespaced, "transcode_video"],
        &[],
    )
    .await;
    let library = fixture
        .cp
        .create_library(NewLibrary {
            slug: "owner-local-replay".to_owned(),
            display_name: "Owner local replay".to_owned(),
            media_kind: LibraryMediaKind::Movie,
            description: None,
            enabled: true,
        })
        .await
        .unwrap();
    let root = fixture
        .cp
        .create_library_root(NewLibraryRoot {
            library_id: library.id,
            owner_node_id: fixture.node_id,
            provider_kind: StorageProviderKind::LocalFilesystem,
            provider_locator: ProviderLocator::new("/owner-local-replay".to_owned()).unwrap(),
            display_locator: "/owner-local-replay".to_owned(),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: Vec::new(),
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        })
        .await
        .unwrap();
    fixture
        .cp
        .activate_library_root(root.id, "owner-local-replay".to_owned())
        .await
        .unwrap();
    let pool = fixture.cp.pool_for_test();
    let asset_id = sqlx::query("INSERT INTO file_assets (created_at, epoch) VALUES (?, 0)")
        .bind("1970-01-01T00:00:00Z")
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid();
    let version_id = sqlx::query(
        "INSERT INTO file_versions (file_asset_id, content_hash, size_bytes, produced_by, \
         produced_from_version_id, created_at, retired_at, epoch) \
         VALUES (?, ?, 1, 'ingest', NULL, '1970-01-01T00:00:00Z', NULL, 0)",
    )
    .bind(asset_id)
    .bind("owner-local-replay")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    let location_id = sqlx::query(
        "INSERT INTO file_locations (file_version_id, address_state, storage_root_id, \
         provider_relative_locator, observed_at, epoch) \
         VALUES (?, 'rooted', ?, ?, '1970-01-01T00:00:00Z', 0)",
    )
    .bind(version_id)
    .bind(i64::try_from(root.id.0).unwrap())
    .bind("movie.mkv")
    .execute(pool)
    .await
    .unwrap()
    .last_insert_rowid();
    ready_byte_work_ticket(&fixture, root.id, u64::try_from(location_id).unwrap()).await;
    (fixture, root.id, u64::try_from(location_id).unwrap())
}

/// Create and ready one owner-local byte-work ticket against the fixture's
/// rooted location.
async fn ready_byte_work_ticket(fixture: &RemoteFixture, root_id: StorageRootId, location_id: u64) {
    let namespaced = "synthetic.workflow.operation.transcode_video";
    let payload = json!({
        "workflow_id": "wf-replay",
        "plan_id": "plan-replay",
        "node_id": "node-replay",
        "branch_id": "branch-replay",
        "operation": "transcode_video",
        "rendered_payload": {
            "operation": "transcode_video",
            "source_storage_root_id": root_id.0,
            "source_location_id": location_id,
        },
        "timing": {"duration_ms": 25, "progress_interval_ms": 10},
        "declared_artifact_access": [
            {"target": {"kind": "storage_root", "storage_root_id": root_id.0}, "rights": ["write"]},
            {"target": {"kind": "file_location", "storage_root_id": root_id.0,
                        "file_location_id": location_id}, "rights": ["read"]}
        ],
    });
    let ticket = fixture
        .cp
        .create_ticket(NewTicket {
            job_id: None,
            kind: ticket_op(namespaced),
            priority: 0,
            payload,
            max_attempts: 2,
            created_at: T0,
        })
        .await
        .unwrap();
    fixture
        .cp
        .mark_ready_if_unblocked(ticket.id, T0)
        .await
        .unwrap();
}

#[tokio::test]
async fn remote_acquire_leased_replay_proves_evidence_and_creates_nothing() {
    let (fixture, _root, _location) = byte_work_fixture().await;
    let key = "replay-proves-evidence";
    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input(key, "hash-rpe"))
        .await
        .unwrap();
    let dispatch = leased_dispatch(&first).clone();
    assert!(dispatch.artifact_access_plan.owner_node_id.is_some());
    assert!(dispatch.artifact_access_plan.access_evidence.is_some());

    let before = evidence_counts(&fixture).await;
    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input(key, "hash-rpe"))
        .await
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(evidence_counts(&fixture).await, before);
}

#[tokio::test]
async fn remote_acquire_leased_replay_survives_completion_and_failure() {
    // Replay after normal completion.
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    fixture.ready_ticket(OP).await;
    let complete_key = "replay-after-complete";
    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input(complete_key, "hash-rac"))
        .await
        .unwrap();
    let lease_id = leased_dispatch(&first).lease_id;
    fixture
        .cp
        .remote_complete(fixture.complete_input(lease_id, "complete-rac", "hash-cr"))
        .await
        .unwrap();
    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input(complete_key, "hash-rac"))
        .await
        .unwrap();
    assert_eq!(replay, first);

    // Replay after failure.
    let failed = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    failed.ready_ticket(OP).await;
    let fail_key = "replay-after-fail";
    let failed_first = failed
        .cp
        .remote_acquire(failed.acquire_input(fail_key, "hash-raf"))
        .await
        .unwrap();
    let failed_lease = leased_dispatch(&failed_first).lease_id;
    failed
        .cp
        .remote_fail(failed.fail_input(failed_lease, "fail-raf", "hash-fr"))
        .await
        .unwrap();
    let failed_replay = failed
        .cp
        .remote_acquire(failed.acquire_input(fail_key, "hash-raf"))
        .await
        .unwrap();
    assert_eq!(failed_replay, failed_first);
}

#[tokio::test]
async fn remote_acquire_replay_rejects_semantic_corruption_as_database_error() {
    fn zero(field: &str) -> impl Fn(&mut serde_json::Value) {
        move |d: &mut serde_json::Value| d[field] = json!(0)
    }
    let cases: Vec<(&str, Box<dyn Fn(&mut serde_json::Value)>)> = vec![
        ("zero-lease-id", Box::new(zero("lease_id"))),
        (
            "zero-scheduler-decision-id",
            Box::new(zero("scheduler_decision_id")),
        ),
        ("zero-ticket-id", Box::new(zero("ticket_id"))),
        ("zero-worker-id", Box::new(zero("worker_id"))),
        (
            "zero-plan-id",
            Box::new(|d| d["artifact_access_plan"]["id"] = json!(0)),
        ),
        (
            "wrong-owner",
            Box::new(|d: &mut serde_json::Value| {
                d["artifact_access_plan"]["owner_node_id"] = json!(987654);
            }),
        ),
        (
            "evidence-mismatch",
            Box::new(|d: &mut serde_json::Value| {
                let epoch = u64::try_from(
                    d["artifact_access_plan"]["access_evidence"]["root_epochs"][0]["root_epoch"]
                        .as_i64()
                        .unwrap(),
                )
                .unwrap();
                d["artifact_access_plan"]["access_evidence"]["root_epochs"][0]["root_epoch"] =
                    json!(epoch + 1);
            }),
        ),
        (
            "altered-operation",
            Box::new(|d: &mut serde_json::Value| d["operation"] = json!("remux")),
        ),
        (
            "altered-payload",
            Box::new(|d: &mut serde_json::Value| {
                d["dispatch_payload"] = json!({"tampered": true});
            }),
        ),
    ];

    for (name, mutate) in cases {
        let (fixture, _root, _location) = byte_work_fixture().await;
        let key = format!("corrupt-{name}");
        let hash = format!("hash-{name}");
        let first = fixture
            .cp
            .remote_acquire(fixture.acquire_input(&key, &hash))
            .await
            .unwrap();
        assert!(
            matches!(first, RemoteAcquireOutcome::Leased(_),),
            "{name}: fixture must lease"
        );
        let mut data = stored_acquire_data(&fixture, &key).await;
        mutate(&mut data);
        overwrite_acquire_data(&fixture, &key, data).await;

        let before = evidence_counts(&fixture).await;
        let err = fixture
            .cp
            .remote_acquire(fixture.acquire_input(&key, &hash))
            .await
            .unwrap_err();
        assert_eq!(
            err.error_code(),
            ErrorCode::DbUnreachable,
            "{name}: semantic corruption must be a database error, got {err:?}"
        );
        assert_eq!(
            evidence_counts(&fixture).await,
            before,
            "{name}: replay created nothing"
        );
        // Semantic corruption never repoints the stored response.
        let stored = stored_acquire_data(&fixture, &key).await;
        assert!(
            !stored.to_string().contains("\"status\":\"error\""),
            "{name}: stored response must be retained"
        );
    }
}

#[tokio::test]
async fn remote_acquire_replay_rejects_row_drift_as_database_error() {
    // Row-level drift the response cannot see: every mutation keeps the
    // schema's own constraints but breaks one identity the replay must prove.
    for name in [
        "plan-deleted",
        "plan-evidence-tampered",
        "decision-deleted",
        "ticket-kind-drift",
        "ticket-payload-drift",
    ] {
        let (fixture, _root, _location) = byte_work_fixture().await;
        let key = format!("drift-{name}");
        let hash = format!("hash-{name}");
        let first = fixture
            .cp
            .remote_acquire(fixture.acquire_input(&key, &hash))
            .await
            .unwrap();
        let dispatch = leased_dispatch(&first).clone();
        let pool = fixture.cp.pool_for_test();
        match name {
            "plan-deleted" => {
                sqlx::query("DELETE FROM artifact_access_plans WHERE lease_id = ?")
                    .bind(i64::try_from(dispatch.lease_id.0).unwrap())
                    .execute(pool)
                    .await
                    .unwrap();
            }
            "plan-evidence-tampered" => {
                sqlx::query(
                    "UPDATE artifact_access_plans SET access_evidence = ? WHERE lease_id = ?",
                )
                .bind(json!({"declaration": [], "root_epochs": []}).to_string())
                .bind(i64::try_from(dispatch.lease_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
            }
            "decision-deleted" => {
                sqlx::query("DELETE FROM scheduler_decisions WHERE id = ?")
                    .bind(i64::try_from(dispatch.scheduler_decision_id).unwrap())
                    .execute(pool)
                    .await
                    .unwrap();
            }
            "ticket-kind-drift" => {
                sqlx::query(
                    "UPDATE tickets SET kind = 'synthetic.workflow.operation.remux' WHERE id = ?",
                )
                .bind(i64::try_from(dispatch.ticket_id.0).unwrap())
                .execute(pool)
                .await
                .unwrap();
            }
            _ => {
                sqlx::query("UPDATE tickets SET payload = ? WHERE id = ?")
                    .bind(json!({"tampered": true}).to_string())
                    .bind(i64::try_from(dispatch.ticket_id.0).unwrap())
                    .execute(pool)
                    .await
                    .unwrap();
            }
        }

        let err = fixture
            .cp
            .remote_acquire(fixture.acquire_input(&key, &hash))
            .await
            .unwrap_err();
        assert_eq!(
            err.error_code(),
            ErrorCode::DbUnreachable,
            "{name}: got {err:?}"
        );
    }
}

#[tokio::test]
async fn remote_acquire_idle_replay_rejects_corrupt_decision_reference() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    let key = "idle-corrupt";
    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input(key, "hash-idc"))
        .await
        .unwrap();
    let RemoteAcquireOutcome::Idle {
        scheduler_decision_id,
        ..
    } = first
    else {
        panic!("expected idle outcome");
    };
    assert!(scheduler_decision_id > 0);

    for bad in [json!(0), json!(987654)] {
        let mut data = stored_acquire_data(&fixture, key).await;
        data["scheduler_decision_id"] = bad.clone();
        overwrite_acquire_data(&fixture, key, data).await;
        let err = fixture
            .cp
            .remote_acquire(fixture.acquire_input(key, "hash-idc"))
            .await
            .unwrap_err();
        assert_eq!(
            err.error_code(),
            ErrorCode::DbUnreachable,
            "decision id {bad}: got {err:?}"
        );
        // restore a valid row for the next iteration
        let mut restored = stored_acquire_data(&fixture, key).await;
        restored["scheduler_decision_id"] = json!(scheduler_decision_id);
        overwrite_acquire_data(&fixture, key, restored).await;
    }
}

#[tokio::test]
async fn remote_complete_requires_exact_typed_consumption_evidence() {
    // Declaration-free plan: the only valid echo is exactly {"validated": true}.
    let fixture = leased_fixture().await;
    let lease_id = fixture_lease_id(&fixture).await;
    for (name, echo) in [
        (
            "legacy-shape",
            json!({"validated": true, "mode": "shared_mount"}),
        ),
        (
            "unknown-field",
            json!({"validated": true, "inputs_consumed": ["handle:input"]}),
        ),
        ("unvalidated", json!({"validated": false})),
        ("missing-marker", json!({})),
    ] {
        let mut input = fixture.complete_input(lease_id, &format!("exact-{name}"), "hash-x");
        input.result = json!({"ok": true, "artifact_access": echo});
        let err = fixture.cp.remote_complete(input).await.unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::Conflict, "{name}: got {err:?}");
        assert_eq!(
            count(&fixture.cp, EventKind::TicketSucceeded).await,
            0,
            "{name}: no terminal mutation"
        );
    }

    // The exact shape completes and stores the typed echo as consumption evidence.
    let mut ok = fixture.complete_input(lease_id, "exact-ok", "hash-ok");
    ok.result = json!({"ok": true, "artifact_access": {"validated": true}});
    let outcome = fixture.cp.remote_complete(ok).await.unwrap();
    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(outcome.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.status, ArtifactAccessPlanStatus::Consumed);
    assert_eq!(plan.evidence, json!({"validated": true}));
}

#[tokio::test]
async fn remote_complete_byte_work_requires_matching_owner_and_evidence_echo() {
    let (fixture, _root, _location) = byte_work_fixture().await;
    let key = "byte-work-complete";
    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input(key, "hash-bwc"))
        .await
        .unwrap();
    let dispatch = leased_dispatch(&first).clone();
    let owner = dispatch.artifact_access_plan.owner_node_id.unwrap();
    let evidence = dispatch.artifact_access_plan.access_evidence.clone();

    // Wrong evidence value: conflict, nothing consumed.
    let mut forged = evidence.clone();
    if let Some(first_epoch) = forged.as_mut().and_then(|e| e.root_epochs.first_mut()) {
        first_epoch.root_epoch += 1;
    }
    for (name, echo) in [
        (
            "wrong-owner",
            json!({
                "validated": true,
                "owner_node_id": owner + 1,
                "access_evidence": serde_json::to_value(&evidence).unwrap()
            }),
        ),
        (
            "wrong-evidence",
            json!({
                "validated": true,
                "owner_node_id": owner,
                "access_evidence": serde_json::to_value(&forged).unwrap()
            }),
        ),
        (
            "missing-evidence",
            json!({"validated": true, "owner_node_id": owner}),
        ),
    ] {
        let mut input = fixture.complete_input(dispatch.lease_id, &format!("bwc-{name}"), "hash-b");
        input.result = json!({"ok": true, "artifact_access": echo});
        let err = fixture.cp.remote_complete(input).await.unwrap_err();
        assert_eq!(err.error_code(), ErrorCode::Conflict, "{name}: got {err:?}");
        let plan = fixture
            .cp
            .artifact_access_plans()
            .get_by_lease(dispatch.lease_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            plan.status,
            ArtifactAccessPlanStatus::Selected,
            "{name}: consumption not claimed"
        );
    }

    // The exact typed echo consumes.
    let mut ok = fixture.complete_input(dispatch.lease_id, "bwc-ok", "hash-bok");
    ok.result = json!({
        "ok": true,
        "artifact_access": {
            "validated": true,
            "owner_node_id": owner,
            "access_evidence": serde_json::to_value(&evidence).unwrap()
        }
    });
    let outcome = fixture.cp.remote_complete(ok).await.unwrap();
    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(outcome.lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.status, ArtifactAccessPlanStatus::Consumed);
}

#[tokio::test]
async fn remote_fail_never_claims_consumption() {
    let (fixture, _root, _location) = byte_work_fixture().await;
    let first = fixture
        .cp
        .remote_acquire(fixture.acquire_input("fail-bw", "hash-fbw"))
        .await
        .unwrap();
    let lease_id = leased_dispatch(&first).lease_id;
    let mut input = fixture.fail_input(lease_id, "fail-key", "hash-ff");
    input.evidence = json!({"consumed": true});
    fixture.cp.remote_fail(input).await.unwrap();
    let plan = fixture
        .cp
        .artifact_access_plans()
        .get_by_lease(lease_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(plan.status, ArtifactAccessPlanStatus::Consumed);
    assert_ne!(plan.status, ArtifactAccessPlanStatus::Selected);
}
