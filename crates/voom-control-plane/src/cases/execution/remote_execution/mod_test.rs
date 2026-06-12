use super::*;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_core::{
    ErrorCode, FailureClass, LeaseId, NodeId, TicketId, TicketOperation,
    clock_test_support::FrozenClock,
};
use voom_events::EventKind;
use voom_scheduler::{
    NodeCandidate, SCORING_VERSION, SchedulerCandidate, ScoreDecision, ScoreOutcome,
    ScoreReasonCode, TicketCandidate, WorkerCandidate,
};
use voom_store::repo::artifact_access_plans::{ArtifactAccessMode, ArtifactAccessPlanStatus};
use voom_store::repo::nodes::NodeKind;
use voom_store::repo::remote_idempotency::RemoteMutationReplay;
use voom_store::repo::scheduler_decisions::{
    SchedulerDecisionFilter, SchedulerDecisionOutcome, SchedulerReasonCode,
};
use voom_store::repo::tickets::{NewTicket, TicketState};
use voom_store::repo::workers::WorkerKind;

use crate::cases::count;
use crate::cases::workers::nodes::RegisterNodeInput;
use crate::cases::workers::{
    NewWorkerCapabilityDraft, NewWorkerGrantDraft, RegisterWorkerForNodeInput,
};

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;
const OP: &str = "test.remote";

fn ticket_op(value: &str) -> TicketOperation {
    TicketOperation::new(value).unwrap()
}

struct RemoteFixture {
    cp: crate::ControlPlane,
    _tmp: tempfile::NamedTempFile,
    node_id: NodeId,
    token: secrecy::SecretString,
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
            worker_id: self.worker_id,
            lease_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            result: json!({
                "ok": true,
                "artifact_access": {
                    "validated": true,
                    "mode": "shared_mount",
                    "inputs_consumed": ["handle:input:test"],
                    "outputs_declared": ["handle:output:test"]
                }
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
            worker_id: self.worker_id,
            lease_id,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            reason: "artifact access mode shared_mount is not advertised".to_owned(),
            class: FailureClass::ArtifactUnavailable,
            evidence: json!({"validated": false, "selected_access_mode": "shared_mount"}),
        }
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
            artifact_access: vec!["shared_mount".to_owned()],
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
        worker_id: WorkerId(2),
        idempotency_key: "capacity-operation-fingerprint".to_owned(),
        request_hash: "hash".to_owned(),
        lease_ttl_seconds: 60,
    };

    let transcode_key = capacity_suppression_key(
        &fixture_input,
        SchedulerReasonCode::NodeCapacityFull.as_str(),
        &ticket_op("transcode"),
    );
    let probe_key = capacity_suppression_key(
        &fixture_input,
        SchedulerReasonCode::NodeCapacityFull.as_str(),
        &ticket_op("probe"),
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
            artifact_access: vec!["local_path".to_owned()],
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
async fn remote_acquire_replays_legacy_idle_without_decision_id() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    seed_legacy_acquire_replay(
        &fixture,
        "legacy-idle",
        "hash-legacy-idle",
        json!({
            "outcome": "idle",
            "worker_id": fixture.worker_id,
        }),
    )
    .await;

    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input("legacy-idle", "hash-legacy-idle"))
        .await
        .unwrap();

    assert_eq!(
        replay,
        RemoteAcquireOutcome::Idle {
            worker_id: fixture.worker_id,
            scheduler_decision_id: 0,
        }
    );
}

#[tokio::test]
async fn remote_acquire_poisoned_replay_is_rewritten_terminal() {
    // M7 regression: a completed idempotency row whose stored Ok{data} no
    // longer decodes into RemoteAcquireOutcome (e.g. after the outcome struct
    // changed) must be rewritten to a terminal Error replay in the same
    // transaction that observes the decode failure. Otherwise every future
    // call with the same key re-runs the identical decode failure forever.
    // The original mutation already executed, so it must NOT be re-run — only
    // the stored result is repointed.
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    seed_legacy_acquire_replay(
        &fixture,
        "poisoned",
        "hash-poisoned",
        json!({ "outcome": "unrecognized_future_variant" }),
    )
    .await;

    let err = fixture
        .cp
        .remote_acquire(fixture.acquire_input("poisoned", "hash-poisoned"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, VoomError::Internal(_)),
        "poisoned replay should surface a decode error, got: {err:?}"
    );

    // The stored row must now be a terminal Error replay, not the original
    // un-decodable Ok{data} that would re-fail decode on every future call.
    let stored = stored_replay(&fixture, "poisoned").await;
    assert!(
        matches!(stored, RemoteMutationReplay::Error { .. }),
        "poisoned replay must be rewritten terminal, still: {stored:?}"
    );
}

#[tokio::test]
async fn remote_acquire_replays_legacy_lease_without_decision_id() {
    let fixture = remote_fixture(&[(OP, vec!["shared_mount"])], &[OP], &[]).await;
    seed_legacy_acquire_replay(
        &fixture,
        "legacy-leased",
        "hash-legacy-leased",
        json!({
            "outcome": "leased",
            "lease_id": 91,
            "ticket_id": 92,
            "worker_id": fixture.worker_id,
            "operation": OP,
            "dispatch_payload": {"dispatch": {"kind": OP}},
            "lease_ttl_seconds": 60,
            "heartbeat_after_seconds": 30,
            "artifact_access_plan": {
                "id": 93,
                "input_handles": ["handle:input:test"],
                "output_handles": ["handle:output:test"],
                "selected_access_mode": "shared_mount"
            }
        }),
    )
    .await;

    let replay = fixture
        .cp
        .remote_acquire(fixture.acquire_input("legacy-leased", "hash-legacy-leased"))
        .await
        .unwrap();

    let RemoteAcquireOutcome::Leased(dispatch) = replay else {
        panic!("expected legacy leased replay");
    };
    assert_eq!(dispatch.scheduler_decision_id, 0);
    assert_eq!(dispatch.worker_id, fixture.worker_id);
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
    let RemoteAcquireOutcome::NoCandidate {
        scheduler_decision_id,
        ..
    } = outcome
    else {
        panic!("expected denied no-candidate");
    };
    let decision = denied
        .cp
        .scheduler_decision(scheduler_decision_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decision.reason_code.as_str(), "operation_denied");
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
    assert_eq!(
        dispatch.artifact_access_plan.selected_access_mode,
        ArtifactAccessMode::SharedMount
    );
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
    assert_eq!(err.error_code(), ErrorCode::Conflict);

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
    let missing = leased_fixture().await;
    let missing_lease_id = fixture_lease_id(&missing).await;
    let mut missing_input =
        missing.complete_input(missing_lease_id, "missing-evidence", "hash-missing");
    missing_input.result = json!({
        "ok": true,
        "artifact_access": {"validated": true}
    });

    let err = missing.cp.remote_complete(missing_input).await.unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(
        missing
            .cp
            .leases()
            .get(missing_lease_id)
            .await
            .unwrap()
            .unwrap()
            .released_at,
        None
    );

    let mismatched = leased_fixture().await;
    let mismatched_lease_id = fixture_lease_id(&mismatched).await;
    let mut mismatched_input = mismatched.complete_input(
        mismatched_lease_id,
        "mismatched-evidence",
        "hash-mismatched",
    );
    mismatched_input.result = json!({
        "ok": true,
        "artifact_access": {
            "validated": true,
            "mode": "control_plane_placeholder",
            "inputs_consumed": ["handle:input:test"],
            "outputs_declared": ["handle:output:test"]
        }
    });

    let err = mismatched
        .cp
        .remote_complete(mismatched_input)
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(count(&mismatched.cp, EventKind::TicketSucceeded).await, 0);
}

#[tokio::test]
async fn remote_heartbeat_reactivates_stale_node_and_replays_lease_heartbeat() {
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
        .unwrap();
    assert_eq!(heartbeat.node_id, fixture.node_id);
    assert_eq!(heartbeat.status, "active");

    let first = fixture
        .cp
        .remote_lease_heartbeat(fixture.lease_heartbeat_input(
            lease_id,
            "lease-heartbeat",
            "hash-lease-hb",
        ))
        .await
        .unwrap();
    let replay = fixture
        .cp
        .remote_lease_heartbeat(fixture.lease_heartbeat_input(
            lease_id,
            "lease-heartbeat",
            "hash-lease-hb",
        ))
        .await
        .unwrap();

    assert_eq!(replay, first);
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
async fn remote_complete_replay_ignores_later_node_retirement() {
    let fixture = leased_fixture().await;
    let complete = fixture.complete_input(
        fixture_lease_id(&fixture).await,
        "complete-before-retire",
        "hash-1",
    );

    let first = fixture.cp.remote_complete(complete.clone()).await.unwrap();
    let node = fixture.cp.get_node(fixture.node_id).await.unwrap().unwrap();
    fixture
        .cp
        .retire_node(fixture.node_id, node.epoch, T0)
        .await
        .unwrap();

    let replay = fixture.cp.remote_complete(complete).await.unwrap();

    assert_eq!(replay, first);
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

    let report = fixture
        .cp
        .remote_recover(T0 + Duration::seconds(61))
        .await
        .unwrap();

    assert_eq!(report.stale_nodes, vec![fixture.node_id]);
    assert_eq!(report.expired_leases, vec![lease_id]);
    assert!(!report.requeued_tickets.is_empty());
    assert_eq!(count(&fixture.cp, EventKind::LeaseExpired).await, 1);
    assert_eq!(
        count(&fixture.cp, EventKind::TicketRequeuedAfterLeaseExpiry).await,
        1
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
    .bind(idempotency_key)
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
    .bind(key)
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

    RemoteFixture {
        cp,
        _tmp: tmp,
        node_id: registered.node.id,
        token: registered.token,
        worker_id: worker.id,
    }
}

async fn cp_at(now: OffsetDateTime) -> (crate::ControlPlane, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
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
