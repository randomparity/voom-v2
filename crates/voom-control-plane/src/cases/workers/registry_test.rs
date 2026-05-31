use super::*;

use secrecy::{ExposeSecret, SecretString};
use sqlx::Row;
use std::sync::{Arc, Mutex};
use time::Duration;
use time::OffsetDateTime;
use voom_core::{Clock, ErrorCode, clock_test_support::ManualClock, rng_test_support::FrozenRng};
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::nodes::{NodeKind, NodeStatus};
use voom_store::repo::workers::WorkerKind;

use crate::cases::cp;
use crate::cases::workers::nodes::RegisterNodeInput;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

fn page_filter(kind: EventKind) -> EventFilter {
    EventFilter {
        kind: Some(kind),
        ..EventFilter::default()
    }
}

#[tokio::test]
async fn register_worker_emits_worker_registered() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerRegistered),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].envelope.subject_id, Some(w.id.0));
}

#[tokio::test]
async fn record_capability_emits_worker_capability_recorded() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let cap = cp
        .record_capability(NewCapability {
            worker_id: w.id,
            operation: "ingest".to_owned(),
            codecs: vec![],
            hardware: vec![],
            artifact_access: vec![],
            extra: serde_json::json!({}),
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerCapabilityRecorded),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::WorkerCapabilityRecorded(payload) = &page.items[0].envelope.payload
    else {
        panic!("expected WorkerCapabilityRecorded payload");
    };
    assert_eq!(payload.capability_id, cap.id);
    assert_eq!(payload.operation, "ingest");
}

#[tokio::test]
async fn record_grant_emits_worker_grant_recorded() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    let grant = cp
        .record_grant(NewGrant {
            worker_id: w.id,
            can_execute: vec!["ingest".to_owned()],
            can_access_read: vec![],
            can_access_write: vec![],
            denies: vec![],
            max_parallel: serde_json::json!({"ingest": 2}),
        })
        .await
        .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerGrantRecorded),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let voom_events::Event::WorkerGrantRecorded(payload) = &page.items[0].envelope.payload else {
        panic!("expected WorkerGrantRecorded payload");
    };
    assert_eq!(payload.grant_id, grant.id);
}

#[tokio::test]
async fn retire_worker_emits_worker_retired() {
    let (cp, _tmp) = cp().await;
    let w = cp
        .register_worker(NewWorker {
            name: "alpha".to_owned(),
            kind: WorkerKind::Synthetic,
            registered_at: OffsetDateTime::UNIX_EPOCH,
            node_id: None,
        })
        .await
        .unwrap();
    cp.retire_worker(
        w.id,
        w.epoch,
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
    )
    .await
    .unwrap();
    let page = cp
        .events()
        .list(
            page_filter(EventKind::WorkerRetired),
            Page {
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].envelope.subject_id, Some(w.id.0));
}

#[tokio::test]
async fn register_worker_for_node_links_worker_and_emits_required_event_sequence() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();
    let input = worker_for_node_input(
        registered.node.id,
        registered.token.expose_secret(),
        "worker-a",
    );

    let worker = cp.register_worker_for_node(input).await.unwrap();

    assert_eq!(worker.node_id, Some(registered.node.id));
    assert_eq!(
        worker_rows(&cp).await,
        vec![(worker.id.0, registered.node.id.0)]
    );
    assert_eq!(
        capability_rows(&cp).await,
        vec![
            (worker.id.0, "inspect".to_owned()),
            (worker.id.0, "transcode".to_owned())
        ]
    );
    assert_eq!(grant_worker_ids(&cp).await, vec![worker.id.0, worker.id.0]);

    let events = worker_events(&cp).await;
    assert_eq!(
        events
            .iter()
            .map(|row| row.envelope.payload.kind())
            .collect::<Vec<_>>(),
        vec![
            EventKind::WorkerRegistered,
            EventKind::WorkerLinkedToNode,
            EventKind::WorkerCapabilityRecorded,
            EventKind::WorkerCapabilityRecorded,
            EventKind::WorkerGrantRecorded,
            EventKind::WorkerGrantRecorded,
        ]
    );
    assert!(matches!(
        &events[1].envelope.payload,
        Event::WorkerLinkedToNode(payload)
            if payload.worker_id == worker.id.0 && payload.node_id == registered.node.id.0
    ));
}

#[tokio::test]
async fn register_worker_for_node_debug_redacts_node_token_plaintext() {
    let token = "voom-node-v1.secret-token";
    let input = worker_for_node_input(voom_core::NodeId(1), token, "worker-a");

    let debug = format!("{input:?}");

    assert!(!debug.contains(token));
}

#[tokio::test]
async fn register_worker_for_node_rolls_back_when_capability_insert_fails_after_worker_events() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER fail_worker_capability_insert \
         BEFORE INSERT ON worker_capabilities \
         BEGIN \
           SELECT RAISE(ABORT, 'forced capability insert failure'); \
         END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();

    let err = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            registered.token.expose_secret(),
            "worker-a",
        ))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::DbUnreachable);
    assert_eq!(worker_rows(&cp).await.len(), 0);
    assert_eq!(capability_count(&cp).await, 0);
    assert_eq!(grant_count(&cp).await, 0);
    assert_eq!(worker_events(&cp).await.len(), 0);
}

#[tokio::test]
async fn register_worker_for_node_invalid_node_token_rejects_without_partial_rows() {
    let (cp, _clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();

    let err = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            "voom-node-v1.invalid",
            "worker-a",
        ))
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(worker_rows(&cp).await.len(), 0);
    assert_eq!(capability_count(&cp).await, 0);
    assert_eq!(grant_count(&cp).await, 0);
    assert_eq!(worker_events(&cp).await.len(), 0);
}

#[tokio::test]
async fn register_worker_for_node_stale_node_rejects_until_heartbeat() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();
    clock.advance(Duration::seconds(61));
    cp.mark_stale_nodes(clock.now()).await.unwrap();

    let err = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            registered.token.expose_secret(),
            "worker-a",
        ))
        .await
        .unwrap_err();
    assert_eq!(err.error_code(), ErrorCode::Conflict);

    cp.heartbeat_node(registered.node.id, registered.token.expose_secret())
        .await
        .unwrap();
    let worker = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            registered.token.expose_secret(),
            "worker-a",
        ))
        .await
        .unwrap();

    assert_eq!(worker.node_id, Some(registered.node.id));
}

#[tokio::test]
async fn register_worker_for_node_retired_node_rejects_registration() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();
    clock.advance(Duration::seconds(1));
    cp.retire_node(registered.node.id, registered.node.epoch, clock.now())
        .await
        .unwrap();

    let heartbeat_err = cp
        .heartbeat_node(registered.node.id, registered.token.expose_secret())
        .await
        .unwrap_err();
    let register_err = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            registered.token.expose_secret(),
            "worker-a",
        ))
        .await
        .unwrap_err();

    assert_eq!(heartbeat_err.error_code(), ErrorCode::Conflict);
    assert_eq!(register_err.error_code(), ErrorCode::Conflict);
    assert_eq!(worker_rows(&cp).await.len(), 0);
}

#[tokio::test]
async fn register_worker_for_node_fresh_registered_node_preserves_heartbeat_state() {
    let (cp, clock, _tmp) = cp_with_manual_clock(T0).await;
    let registered = cp
        .register_node(register_node_input("node-a"))
        .await
        .unwrap();
    clock.advance(Duration::seconds(30));

    let worker = cp
        .register_worker_for_node(worker_for_node_input(
            registered.node.id,
            registered.token.expose_secret(),
            "worker-a",
        ))
        .await
        .unwrap();
    let node = cp.get_node(registered.node.id).await.unwrap().unwrap();

    assert_eq!(worker.node_id, Some(registered.node.id));
    assert_eq!(node.last_seen_at, registered.node.last_seen_at);
    assert_eq!(node.status, NodeStatus::Registered);
    assert_eq!(node.epoch, registered.node.epoch);
}

fn register_node_input(name: &str) -> RegisterNodeInput {
    RegisterNodeInput {
        name: name.to_owned(),
        kind: NodeKind::Synthetic,
        heartbeat_ttl_seconds: 60,
        metadata: serde_json::json!({}),
    }
}

fn worker_for_node_input(
    node_id: voom_core::NodeId,
    token: &str,
    name: &str,
) -> RegisterWorkerForNodeInput {
    RegisterWorkerForNodeInput {
        node_id,
        token: SecretString::from(token.to_owned()),
        name: name.to_owned(),
        kind: WorkerKind::Synthetic,
        capabilities: vec![
            NewWorkerCapabilityDraft {
                operation: "inspect".to_owned(),
                codecs: vec!["json".to_owned()],
                hardware: vec!["cpu".to_owned()],
                artifact_access: vec!["read".to_owned()],
                extra: serde_json::json!({"priority": 1}),
            },
            NewWorkerCapabilityDraft {
                operation: "transcode".to_owned(),
                codecs: vec!["h264".to_owned()],
                hardware: vec!["gpu".to_owned()],
                artifact_access: vec!["read".to_owned(), "write".to_owned()],
                extra: serde_json::json!({}),
            },
        ],
        grants: vec![
            NewWorkerGrantDraft {
                can_execute: vec!["inspect".to_owned()],
                can_access_read: vec!["artifact:*".to_owned()],
                can_access_write: vec![],
                denies: vec![],
                max_parallel: serde_json::json!({"inspect": 1}),
            },
            NewWorkerGrantDraft {
                can_execute: vec!["transcode".to_owned()],
                can_access_read: vec!["artifact:*".to_owned()],
                can_access_write: vec!["artifact:*".to_owned()],
                denies: vec!["delete".to_owned()],
                max_parallel: serde_json::json!({"transcode": 2}),
            },
        ],
    }
}

async fn cp_with_manual_clock(
    now: OffsetDateTime,
) -> (
    crate::ControlPlane,
    Arc<ManualClock>,
    tempfile::NamedTempFile,
) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite://{}", tmp.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(u32::MAX))),
    )
    .await
    .unwrap();
    (cp, clock, tmp)
}

async fn worker_events(cp: &crate::ControlPlane) -> Vec<voom_store::repo::EventRow> {
    cp.events()
        .list(
            EventFilter {
                subject_type: Some(SubjectType::Worker),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await
        .unwrap()
        .items
}

async fn worker_rows(cp: &crate::ControlPlane) -> Vec<(u64, u64)> {
    sqlx::query("SELECT id, node_id FROM workers ORDER BY id")
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
        .iter()
        .map(|row| {
            let id: i64 = row.try_get("id").unwrap();
            let node_id: i64 = row.try_get("node_id").unwrap();
            (u64::try_from(id).unwrap(), u64::try_from(node_id).unwrap())
        })
        .collect()
}

async fn capability_rows(cp: &crate::ControlPlane) -> Vec<(u64, String)> {
    sqlx::query("SELECT worker_id, operation FROM worker_capabilities ORDER BY id")
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
        .iter()
        .map(|row| {
            let worker_id: i64 = row.try_get("worker_id").unwrap();
            let operation = row.try_get("operation").unwrap();
            (u64::try_from(worker_id).unwrap(), operation)
        })
        .collect()
}

async fn grant_worker_ids(cp: &crate::ControlPlane) -> Vec<u64> {
    sqlx::query("SELECT worker_id FROM worker_grants ORDER BY id")
        .fetch_all(cp.pool_for_test())
        .await
        .unwrap()
        .iter()
        .map(|row| {
            let worker_id: i64 = row.try_get("worker_id").unwrap();
            u64::try_from(worker_id).unwrap()
        })
        .collect()
}

async fn capability_count(cp: &crate::ControlPlane) -> usize {
    row_count(cp, "worker_capabilities").await
}

async fn grant_count(cp: &crate::ControlPlane) -> usize {
    row_count(cp, "worker_grants").await
}

async fn row_count(cp: &crate::ControlPlane, table: &str) -> usize {
    let sql = format!("SELECT COUNT(*) AS count FROM {table}");
    let count: i64 = sqlx::query(&sql)
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
        .try_get("count")
        .unwrap();
    usize::try_from(count).unwrap()
}
