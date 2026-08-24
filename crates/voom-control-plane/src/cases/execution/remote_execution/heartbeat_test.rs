use std::sync::{Arc, Mutex};

use secrecy::SecretString;
use serde_json::json;
use time::{Duration, OffsetDateTime};
use voom_core::{
    ArtifactAccessMode, ErrorCode, OperationKind, clock_test_support::ManualClock,
    rng_test_support::FrozenRng,
};
use voom_store::repo::execution::nodes::NodeKind;

use super::super::{RemoteActivateInput, RemoteNodeHeartbeatInput, RemoteWorkerDeclaration};
use crate::cases::workers::nodes::RegisterNodeInput;

const T0: OffsetDateTime = OffsetDateTime::UNIX_EPOCH;

#[tokio::test]
async fn remote_node_heartbeat_advances_node_and_incarnation_together() {
    let (cp, clock, _database) = fixture().await;
    let (node_id, token, incarnation_id) = activate(&cp).await;
    let before = cp.get_node(node_id).await.unwrap().unwrap();
    clock.advance(Duration::seconds(5));

    cp.remote_node_heartbeat(heartbeat(node_id, token, incarnation_id, "advance"))
        .await
        .unwrap();

    let node = cp.get_node(node_id).await.unwrap().unwrap();
    let incarnations = cp.list_node_incarnations(node_id, 10).await.unwrap();
    assert_eq!(node.last_seen_at, T0 + Duration::seconds(5));
    assert_eq!(node.epoch, before.epoch + 1);
    assert_eq!(incarnations[0].last_seen_at, node.last_seen_at);
}

#[tokio::test]
async fn incarnation_heartbeat_failure_rolls_back_node_event_and_replay_reservation() {
    let (cp, clock, _database) = fixture().await;
    let (node_id, token, incarnation_id) = activate(&cp).await;
    let node_before = cp.get_node(node_id).await.unwrap().unwrap();
    let events_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_incarnation_heartbeat \
         BEFORE UPDATE OF last_seen_at ON node_incarnations \
         BEGIN SELECT RAISE(ABORT, 'reject incarnation heartbeat'); END",
    )
    .execute(cp.pool_for_test())
    .await
    .unwrap();
    clock.advance(Duration::seconds(5));

    let error = cp
        .remote_node_heartbeat(heartbeat(node_id, token, incarnation_id, "rollback"))
        .await
        .unwrap_err();

    assert_eq!(error.error_code(), ErrorCode::DbUnreachable);
    let node_after = cp.get_node(node_id).await.unwrap().unwrap();
    assert_eq!(node_after.last_seen_at, node_before.last_seen_at);
    assert_eq!(node_after.epoch, node_before.epoch);
    let events_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(cp.pool_for_test())
        .await
        .unwrap();
    let replay_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remote_idempotency_keys WHERE idempotency_key LIKE '%:rollback'",
    )
    .fetch_one(cp.pool_for_test())
    .await
    .unwrap();
    assert_eq!(events_after, events_before);
    assert_eq!(replay_rows, 0);
}

async fn fixture() -> (
    crate::ControlPlane,
    Arc<ManualClock>,
    voom_test_support::TempDatabase,
) {
    let database = voom_test_support::TempDatabase::new().unwrap();
    let url = format!("sqlite://{}", database.path().display());
    voom_store::init(&url).await.unwrap();
    let pool = voom_store::connect(&url).await.unwrap();
    let clock = Arc::new(ManualClock::new(T0));
    let cp = crate::ControlPlane::open_with_pool_and_rng(
        pool,
        clock.clone(),
        Arc::new(Mutex::new(FrozenRng::new(0x417))),
    )
    .await
    .unwrap();
    (cp, clock, database)
}

async fn activate(
    cp: &crate::ControlPlane,
) -> (
    voom_core::NodeId,
    SecretString,
    voom_core::NodeIncarnationId,
) {
    let registered = cp
        .register_node(RegisterNodeInput {
            name: "heartbeat-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 30,
            metadata: json!({}),
        })
        .await
        .unwrap();
    let incarnation_id = "0123456789abcdef0123456789abcdef".parse().unwrap();
    cp.remote_activate(RemoteActivateInput {
        node_id: registered.node.id,
        token: registered.token.clone(),
        idempotency_key: "activate".to_owned(),
        request_hash: "activate-body".to_owned(),
        incarnation_id,
        workers: vec![RemoteWorkerDeclaration {
            logical_name: "probe".to_owned(),
            operations: vec![OperationKind::ProbeFile],
            artifact_access: vec![ArtifactAccessMode::SharedMount],
            accelerator: None,
            max_parallel: 1,
        }],
    })
    .await
    .unwrap();
    (registered.node.id, registered.token, incarnation_id)
}

fn heartbeat(
    node_id: voom_core::NodeId,
    token: SecretString,
    incarnation_id: voom_core::NodeIncarnationId,
    key: &str,
) -> RemoteNodeHeartbeatInput {
    RemoteNodeHeartbeatInput {
        node_id,
        token,
        incarnation_id,
        idempotency_key: key.to_owned(),
        request_hash: format!("{key}-body"),
    }
}
