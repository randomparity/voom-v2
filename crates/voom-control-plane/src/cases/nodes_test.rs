use super::*;

use secrecy::ExposeSecret;
use serde_json::Value as JsonValue;
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use voom_core::{ErrorCode, NodeId, clock_test_support::FrozenClock};
use voom_events::{Event, EventKind, SubjectType};
use voom_store::repo::events::{EventFilter, EventRepo, Page};
use voom_store::repo::nodes::{Node, NodeKind, NodeStatus};

use crate::node_auth::hash_node_token;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn nodes_register_node_returns_plaintext_token_once_and_emits_event() {
    let (cp, _tmp) = cp_at(T0).await;

    let registered = cp
        .register_node(register_input("synthetic-a"))
        .await
        .unwrap();
    let token = registered.token.expose_secret();

    assert!(token.starts_with("voom-node-v1."));
    let stored_hash = auth_token_hash(&cp, registered.node.id).await;
    assert!(stored_hash.starts_with("voom-node-token-sha256-v1:"));
    assert_ne!(stored_hash, token);
    assert_eq!(stored_hash, hash_node_token(token));

    let events = events(&cp, EventKind::NodeRegistered).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].envelope.subject_type, SubjectType::Node);
    assert_eq!(events[0].envelope.subject_id, Some(registered.node.id.0));
}

#[tokio::test]
async fn nodes_heartbeat_with_valid_token_activates_node_and_emits_event() {
    let now = T0 + Duration::seconds(30);
    let (cp, _tmp) = cp_at(now).await;
    let registered = cp
        .register_node(register_input("synthetic-a"))
        .await
        .unwrap();

    let node = cp
        .heartbeat_node(registered.node.id, registered.token.expose_secret())
        .await
        .unwrap();

    assert_eq!(node.status, NodeStatus::Active);
    assert_eq!(node.epoch, 1);
    assert_eq!(node.last_seen_at, cp.clock().now());

    let events = events(&cp, EventKind::NodeHeartbeatRecorded).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].envelope.subject_id, Some(node.id.0));
    let Event::NodeHeartbeatRecorded(payload) = &events[0].envelope.payload else {
        panic!("expected NodeHeartbeatRecorded payload");
    };
    assert_eq!(payload.epoch, 1);
    assert_eq!(payload.last_seen_at, now);
}

#[tokio::test]
async fn nodes_heartbeat_with_invalid_token_returns_conflict_without_mutation() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = cp
        .register_node(register_input("synthetic-a"))
        .await
        .unwrap();

    let err = cp
        .heartbeat_node(registered.node.id, "voom-node-v1.invalid")
        .await
        .unwrap_err();

    assert_eq!(err.error_code(), ErrorCode::Conflict);
    let stored = cp.get_node(registered.node.id).await.unwrap().unwrap();
    assert_eq!(stored.status, NodeStatus::Registered);
    assert_eq!(stored.epoch, 0);
    assert_eq!(stored.last_seen_at, registered.node.last_seen_at);
    assert_eq!(events(&cp, EventKind::NodeHeartbeatRecorded).await.len(), 0);
}

#[tokio::test]
async fn mark_stale_nodes_is_idempotent_and_emits_once_per_changed_node() {
    let now = T0 + Duration::seconds(120);
    let (cp, _tmp) = cp_at(now).await;
    let first = cp
        .register_node(register_input("expired-a"))
        .await
        .unwrap()
        .node;
    let second = cp
        .register_node(register_input("expired-b"))
        .await
        .unwrap()
        .node;
    let already_stale = cp
        .register_node(register_input("already-stale"))
        .await
        .unwrap()
        .node;
    force_node_state(&cp, first.id, NodeStatus::Active, T0, 0).await;
    force_node_state(&cp, second.id, NodeStatus::Active, T0, 0).await;
    force_node_state(&cp, already_stale.id, NodeStatus::Stale, T0, 1).await;

    let changed = cp.mark_stale_nodes(now).await.unwrap();
    let changed_again = cp.mark_stale_nodes(now).await.unwrap();

    assert_eq!(
        changed.iter().map(|node| node.id).collect::<Vec<_>>(),
        vec![first.id, second.id]
    );
    assert!(changed_again.is_empty());
    assert_eq!(events(&cp, EventKind::NodeMarkedStale).await.len(), 2);
}

#[tokio::test]
async fn nodes_retire_node_is_terminal_and_emits_event() {
    let now = T0 + Duration::seconds(60);
    let (cp, _tmp) = cp_at(now).await;
    let registered = cp
        .register_node(register_input("synthetic-a"))
        .await
        .unwrap()
        .node;

    let retired = cp
        .retire_node(registered.id, registered.epoch, now)
        .await
        .unwrap();
    let err = cp
        .retire_node(retired.id, retired.epoch, now + Duration::seconds(1))
        .await
        .unwrap_err();

    assert_eq!(retired.status, NodeStatus::Retired);
    assert_eq!(retired.retired_at, Some(now));
    assert_eq!(retired.epoch, registered.epoch + 1);
    assert_eq!(err.error_code(), ErrorCode::Conflict);
    assert_eq!(events(&cp, EventKind::NodeRetired).await.len(), 1);
}

#[tokio::test]
async fn list_and_show_nodes_do_not_expose_token_hash() {
    let (cp, _tmp) = cp_at(T0).await;
    let registered = cp
        .register_node(register_input("synthetic-a"))
        .await
        .unwrap();
    let token = registered.token.expose_secret().to_owned();

    let shown = cp.get_node(registered.node.id).await.unwrap().unwrap();
    let listed = cp.list_nodes(None, 10).await.unwrap();
    let dto_json = serde_json::json!({
        "show": node_dto(&shown),
        "list": listed.iter().map(node_dto).collect::<Vec<_>>(),
    });

    assert_no_secret_keys_or_values(&dto_json, &token);
}

fn register_input(name: &str) -> RegisterNodeInput {
    RegisterNodeInput {
        name: name.to_owned(),
        kind: NodeKind::Synthetic,
        heartbeat_ttl_seconds: 60,
        metadata: serde_json::json!({"zone": "test"}),
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
            voom_core::rng_test_support::FrozenRng::new(0x0707_0707),
        )),
    )
    .await
    .unwrap();
    (cp, tmp)
}

async fn auth_token_hash(cp: &crate::ControlPlane, node_id: NodeId) -> String {
    sqlx::query("SELECT auth_token_hash FROM nodes WHERE id = ?")
        .bind(i64::try_from(node_id.0).unwrap())
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap()
        .try_get("auth_token_hash")
        .unwrap()
}

async fn force_node_state(
    cp: &crate::ControlPlane,
    node_id: NodeId,
    status: NodeStatus,
    last_seen_at: OffsetDateTime,
    epoch: u64,
) {
    sqlx::query("UPDATE nodes SET status = ?, last_seen_at = ?, epoch = ? WHERE id = ?")
        .bind(status.as_str())
        .bind(
            last_seen_at
                .format(&time::format_description::well_known::Iso8601::DEFAULT)
                .unwrap(),
        )
        .bind(i64::try_from(epoch).unwrap())
        .bind(i64::try_from(node_id.0).unwrap())
        .execute(cp.pool_for_test())
        .await
        .unwrap();
}

async fn events(cp: &crate::ControlPlane, kind: EventKind) -> Vec<voom_store::repo::EventRow> {
    cp.events()
        .list(
            EventFilter {
                kind: Some(kind),
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

fn node_dto(node: &Node) -> JsonValue {
    serde_json::json!({
        "id": node.id.0,
        "name": node.name,
        "kind": node.kind.as_str(),
        "status": node.status.as_str(),
        "registered_at": node.registered_at,
        "last_seen_at": node.last_seen_at,
        "retired_at": node.retired_at,
        "heartbeat_ttl_seconds": node.heartbeat_ttl_seconds,
        "metadata": node.metadata,
        "epoch": node.epoch,
    })
}

fn assert_no_secret_keys_or_values(value: &JsonValue, plaintext_token: &str) {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                assert_ne!(key, "token");
                assert_ne!(key, "auth_token_hash");
                assert_no_secret_keys_or_values(child, plaintext_token);
            }
        }
        JsonValue::Array(items) => {
            for item in items {
                assert_no_secret_keys_or_values(item, plaintext_token);
            }
        }
        JsonValue::String(s) => {
            assert_ne!(s, plaintext_token);
            assert!(!s.starts_with("voom-node-token-sha256-v1:"));
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => {}
    }
}
