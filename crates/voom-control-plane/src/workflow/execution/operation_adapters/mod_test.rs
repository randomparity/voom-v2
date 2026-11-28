use super::*;

use serde_json::json;
use voom_core::{TicketOperation, WorkerKind};
use voom_store::repo::execution::leases::{LeaseFilter, LeaseState, NewLease};
use voom_store::repo::execution::tickets::{NewTicket, TicketState};
use voom_store::repo::execution::workers::{NewCapability, NewGrant};

#[tokio::test]
async fn heartbeat_failure_durably_fails_leased_ticket() {
    let (control, _database) = crate::cases::cp().await;
    let operation = OperationKind::VerifyArtifact;
    let kind = TicketOperation::from(operation);
    let worker = control
        .register_worker(crate::cases::workers::RegisterWorkerInput {
            name: "heartbeat-failure-worker".to_owned(),
            kind: WorkerKind::Synthetic,
        })
        .await
        .unwrap();
    control
        .record_capability(NewCapability {
            worker_id: worker.id,
            operation: kind.clone(),
            codecs: Vec::new(),
            hardware: Vec::new(),
            artifact_access: Vec::new(),
            extra: json!({}),
        })
        .await
        .unwrap();
    control
        .record_grant(NewGrant {
            worker_id: worker.id,
            can_execute: vec![kind.clone()],
            can_access_read: Vec::new(),
            can_access_write: Vec::new(),
            denies: Vec::new(),
            max_parallel: json!({}),
        })
        .await
        .unwrap();
    let ticket = control
        .create_ticket(NewTicket {
            job_id: None,
            kind,
            priority: 0,
            payload: json!({}),
            max_attempts: 1,
            created_at: control.clock().now(),
        })
        .await
        .unwrap();
    control
        .mark_ready_if_unblocked(ticket.id, control.clock().now())
        .await
        .unwrap();
    let timing = WorkflowTimingOptions::for_tests();
    let lease = control
        .acquire_lease(NewLease {
            ticket_id: ticket.id,
            worker_id: worker.id,
            ttl: time::Duration::try_from(timing.lease_ttl).unwrap(),
            now: control.clock().now(),
        })
        .await
        .unwrap();
    let chaos = WorkflowChaosOptions {
        fail_heartbeat_operation: Some(operation),
        ..WorkflowChaosOptions::default()
    };

    // A pending operation makes the injected heartbeat the only completion path.
    // Real Tokio time keeps SQLite pool acquisition independent of virtual time.
    let error = await_with_lease_heartbeats(
        LeaseHeartbeatContext {
            control: &control,
            lease_id: lease.id,
            timing: &timing,
            chaos: &chaos,
        },
        operation,
        std::future::pending::<Result<(), VoomError>>(),
    )
    .await
    .unwrap_err();

    assert!(matches!(error, VoomError::Conflict(ref message)
        if message == "injected heartbeat failure for VerifyArtifact"));
    // Returning the source error is insufficient: cleanup must already be durable.
    assert_eq!(
        control
            .get_ticket(ticket.id.0)
            .await
            .unwrap()
            .unwrap()
            .state,
        TicketState::Failed
    );
    assert!(
        control
            .list_scheduler_leases(
                LeaseFilter {
                    state: Some(LeaseState::Held),
                },
                None,
                1,
            )
            .await
            .unwrap()
            .is_empty()
    );
}
