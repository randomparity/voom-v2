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
use tokio::io::{AsyncWriteExt, DuplexStream};
use tower::ServiceExt;
use voom_api::router_with_control_plane;
use voom_api::server::DeadlineStream;
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::NodeId;
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
    let health_plane = HealthPlane::open(&url).await?;
    let router = router_with_control_plane(health_plane, control_plane.clone());
    let replay_router = router.clone();
    let idempotency_key = "committed-response-loss";
    let path = format!("/v1/execution/node/{}/heartbeat", registered.node.id.0);
    let request_bytes = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nX-Voom-Idempotency-Key: {idempotency_key}\r\n\
         Content-Length: 2\r\n\r\n{{}}",
        registered.token.expose_secret()
    );
    let (server_io, mut client_io) = tokio::io::duplex(1);
    let server_task = spawn_one_connection(router, server_io);
    client_io.write_all(request_bytes.as_bytes()).await?;

    let stored =
        wait_for_committed_result(&pool, registered.node.id, &path, idempotency_key).await?;
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
                .body(Body::from("{}"))?,
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
        idempotency_row_count(&pool, registered.node.id, &path, idempotency_key).await?,
        1
    );
    Ok(())
}

fn spawn_one_connection(
    router: axum::Router,
    stream: DuplexStream,
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
