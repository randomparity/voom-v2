#![expect(
    clippy::unwrap_used,
    reason = "integration tests favor unwrap over plumbing Result<()> through every assertion"
)]

//! Disk-mode parity: run the M1 fixture flow against a tempfile-backed
//! disk DB, close the pool, reopen, and assert every row persists.
//! Satisfies the architectural-spec exit clause that in-memory and disk
//! modes exercise the same repositories.

use std::sync::Arc;

use serde_json::json;
use time::Duration;

use voom_control_plane::ControlPlane;
use voom_core::{SystemClock, TicketOperation};
use voom_store::repo::leases::{NewLease, ReleaseReason};
use voom_store::repo::tickets::{NewTicket, TicketState};
use voom_store::repo::workers::{NewWorker, WorkerKind};
use voom_store::test_support::{T0, sqlite_url_for};
use voom_store::{connect, init};

#[tokio::test]
async fn m1_fixture_flow_persists_across_reconnect() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = sqlite_url_for(tmp.path());

    // Initial run: init + drive a ticket through to success via ControlPlane.
    let _report = init(&url).await.unwrap();
    let (tid, lid, wid) = {
        let pool = connect(&url).await.unwrap();
        let cp = ControlPlane::open_with_pool(pool.clone(), Arc::new(SystemClock))
            .await
            .unwrap();
        let t = cp
            .create_ticket(NewTicket {
                job_id: None,
                kind: TicketOperation::new("disk.test").unwrap(),
                priority: 0,
                payload: json!({}),
                max_attempts: 1,
                created_at: T0,
            })
            .await
            .unwrap();
        let w = cp
            .register_worker(NewWorker {
                name: "w-disk".to_owned(),
                kind: WorkerKind::Synthetic,
                registered_at: T0,
                node_id: None,
            })
            .await
            .unwrap();
        cp.mark_ready_if_unblocked(t.id, T0).await.unwrap();
        let l = cp
            .acquire_lease(NewLease {
                ticket_id: t.id,
                worker_id: w.id,
                ttl: Duration::seconds(60),
                now: T0,
            })
            .await
            .unwrap();
        cp.release_lease(l.id, json!({"ok": true}), T0 + Duration::seconds(1))
            .await
            .unwrap();
        pool.close().await;
        (t.id, l.id, w.id)
    };

    // Reopen: confirm rows survived the close.
    let pool2 = connect(&url).await.unwrap();
    let cp2 = ControlPlane::open_with_pool(pool2, Arc::new(SystemClock))
        .await
        .unwrap();
    let t = cp2.tickets().get(tid).await.unwrap().unwrap();
    assert_eq!(t.state, TicketState::Succeeded, "ticket persisted");
    let l = cp2.leases().get(lid).await.unwrap().unwrap();
    assert_eq!(
        l.release_reason,
        Some(ReleaseReason::Released),
        "lease persisted"
    );
    let w = cp2.workers().get(wid).await.unwrap().unwrap();
    assert_eq!(w.name, "w-disk", "worker persisted");
}
