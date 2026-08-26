#![expect(
    clippy::panic_in_result_fn,
    reason = "integration tests use direct assertions after fallible transport setup"
)]

use std::error::Error;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::io::{AsyncWriteExt, Join, ReadHalf, SimplexStream, WriteHalf};
use tower::ServiceExt;
use voom_api::router_with_control_plane;
use voom_api::server::DeadlineStream;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{ArtifactAccessMode, NodeId, NodeIncarnationId, OperationKind};
use voom_events::{Event, EventKind};
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

type TestResult = Result<(), Box<dyn Error>>;

#[tokio::test]
async fn committed_response_loss_replays_original_result() -> TestResult {
    let database = TempDatabase::new()?;
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await?;
    let pool = voom_store::connect(&url).await?;
    let control_plane = ControlPlane::open(&url).await?;
    let registered = control_plane
        .register_node(RegisterNodeInput {
            name: "cutoff-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await?;
    let incarnation_id: NodeIncarnationId = "0123456789abcdef0123456789abcdef".parse()?;
    control_plane
        .remote_activate(RemoteActivateInput {
            node_id: registered.node.id,
            token: registered.token.clone(),
            idempotency_key: "cutoff-activation".to_owned(),
            request_hash: "cutoff-activation-body".to_owned(),
            incarnation_id,
            workers: vec![RemoteWorkerDeclaration {
                logical_name: "cutoff-worker".to_owned(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                accelerator: None,
                max_parallel: 1,
            }],
        })
        .await?;
    let health_plane = HealthPlane::open(&url).await?;
    let router = router_with_control_plane(health_plane, control_plane.clone());
    let replay_router = router.clone();
    let idempotency_key = "committed-response-loss";
    let stored_idempotency_key = format!("{incarnation_id}:{idempotency_key}");
    let path = format!("/v1/execution/node/{}/heartbeat", registered.node.id.0);
    let request_body = json!({"incarnation_id": incarnation_id}).to_string();
    let request_bytes = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nX-Voom-Idempotency-Key: {idempotency_key}\r\n\
         Content-Length: {}\r\n\r\n{request_body}",
        registered.token.expose_secret(),
        request_body.len()
    );
    // The two directions are sized independently on purpose. The request pipe holds the
    // whole request, so `write_all` below completes in one poll without the server reading
    // a byte, and so cannot race the connection deadline. The response pipe is one byte and
    // is never drained: that is what makes the server block partway through writing its
    // response until the deadline cuts the connection, which is the committed-response loss
    // under test. `_response_reader` stays bound for the rest of the test because dropping
    // the read half would fail the server's write immediately instead of blocking it.
    let (request_reader, mut request_writer) = tokio::io::simplex(request_bytes.len());
    let (_response_reader, response_writer) = tokio::io::simplex(1);
    let server_task =
        spawn_one_connection(router, tokio::io::join(request_reader, response_writer));
    request_writer.write_all(request_bytes.as_bytes()).await?;

    let stored =
        wait_for_committed_result(&pool, registered.node.id, &path, &stored_idempotency_key)
            .await?;
    assert_eq!(
        heartbeat_event_count(&control_plane, registered.node.id).await?,
        1
    );
    let transport_result = server_task.await??;
    assert!(transport_result.is_err());

    let replay = replay_router
        .oneshot(
            Request::post(&path)
                .header("content-type", "application/json")
                .header(
                    "authorization",
                    format!("Bearer {}", registered.token.expose_secret()),
                )
                .header("x-voom-idempotency-key", idempotency_key)
                .body(Body::from(request_body))?,
        )
        .await?;
    assert_eq!(replay.status(), StatusCode::OK);
    let replay_json: Value =
        serde_json::from_slice(&replay.into_body().collect().await?.to_bytes())?;
    assert_eq!(replay_json["data"], stored["data"]);
    assert_eq!(
        heartbeat_event_count(&control_plane, registered.node.id).await?,
        1
    );
    assert_eq!(
        idempotency_row_count(&pool, registered.node.id, &path, &stored_idempotency_key,).await?,
        1
    );
    Ok(())
}

fn spawn_one_connection(
    router: axum::Router,
    stream: Join<ReadHalf<SimplexStream>, WriteHalf<SimplexStream>>,
) -> tokio::task::JoinHandle<Result<Result<(), hyper::Error>, tokio::time::error::Elapsed>> {
    tokio::spawn(async move {
        let bounded = DeadlineStream::new(stream, Duration::from_millis(250));
        let service = TowerToHyperService::new(router);
        tokio::time::timeout(
            Duration::from_secs(1),
            http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(bounded), service),
        )
        .await
    })
}

async fn wait_for_committed_result(
    pool: &SqlitePool,
    node_id: NodeId,
    path: &str,
    idempotency_key: &str,
) -> Result<Value, Box<dyn Error>> {
    let node_id = i64::try_from(node_id.0)?;
    let route_key = format!("POST {path}");
    let result = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let response = sqlx::query_scalar::<_, String>(
                "SELECT response_json FROM remote_idempotency_keys \
                 WHERE node_id = ? AND route_key = ? AND worker_scope_id = 0 \
                   AND idempotency_key = ? AND status = 'completed'",
            )
            .bind(node_id)
            .bind(&route_key)
            .bind(idempotency_key)
            .fetch_optional(pool)
            .await?;
            if let Some(response) = response {
                return Ok::<_, sqlx::Error>(response);
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(serde_json::from_str(&result)?)
}

async fn idempotency_row_count(
    pool: &SqlitePool,
    node_id: NodeId,
    path: &str,
    idempotency_key: &str,
) -> Result<u64, Box<dyn Error>> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM remote_idempotency_keys \
         WHERE node_id = ? AND route_key = ? AND worker_scope_id = 0 AND idempotency_key = ?",
    )
    .bind(i64::try_from(node_id.0)?)
    .bind(format!("POST {path}"))
    .bind(idempotency_key)
    .fetch_one(pool)
    .await?;
    Ok(u64::try_from(count)?)
}

async fn heartbeat_event_count(
    control_plane: &ControlPlane,
    node_id: NodeId,
) -> Result<usize, Box<dyn Error>> {
    let events = control_plane
        .events()
        .list(
            EventFilter {
                kind: Some(EventKind::NodeHeartbeatRecorded),
                ..EventFilter::default()
            },
            Page {
                limit: 20,
                cursor: None,
            },
        )
        .await?;
    Ok(events
        .items
        .iter()
        .filter(|row| {
            matches!(
                &row.envelope.payload,
                Event::NodeHeartbeatRecorded(payload) if payload.node_id == node_id
            )
        })
        .count())
}
