#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "net-resilience tests fail loudly and are opt-in (run via just net-resilience)"
)]
//! Network-resilience scenarios: inject real TCP faults with Toxiproxy between a
//! live `HttpServer` and `HttpClient`, and assert the client's timeout/error
//! contract. See docs/superpowers/specs/2026-07-09-issue-321-toxiproxy-net-resilience-design.md
//! and docs/adr/0033. These tests are `#[ignore]`d because they require an
//! external `toxiproxy-server`; run them with `just net-resilience`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use secrecy::SecretString;
use time::OffsetDateTime;
use voom_core::{LeaseId, PROTOCOL_VERSION, WorkerId};
use voom_worker_protocol::{
    ClientHandle, HttpClient, HttpServer, OperationDispatch, OperationHandler, OperationKind,
    OperationRequest, OperationResponse, ProgressFrame, ProtocolError, ServerHandle, ServerRunning,
    WorkerCredentials,
};

// --- Toxiproxy REST control (test-only) -------------------------------------

/// Control-API base URL, e.g. `http://127.0.0.1:8474`. Requires `TOXIPROXY_ADDR`
/// with no default: the wrong-server guard must be a property of the code path,
/// not one entry point, so a direct `cargo test --ignored` also fails loud
/// rather than driving a foreign toxiproxy on the well-known 8474 port.
fn toxiproxy_base() -> String {
    let addr = std::env::var("TOXIPROXY_ADDR").unwrap_or_else(|_| {
        panic!("TOXIPROXY_ADDR is not set; set it or run `just net-resilience`");
    });
    format!("http://{addr}")
}

struct Toxiproxy {
    base: String,
    http: reqwest::Client,
}

impl Toxiproxy {
    fn new() -> Self {
        Self {
            base: toxiproxy_base(),
            http: reqwest::Client::new(),
        }
    }

    /// Create a proxy fronting `upstream`, returning its resolved listen address.
    ///
    /// Idempotent: a best-effort delete of any same-named proxy runs first so a
    /// proxy leaked by a prior panicking run cannot 409-poison this call in the
    /// manually-started-toxiproxy TDD loop.
    async fn create_proxy(&self, name: &str, upstream: SocketAddr) -> SocketAddr {
        let _ = self
            .http
            .delete(format!("{}/proxies/{name}", self.base))
            .send()
            .await;
        let body = serde_json::json!({
            "name": name,
            "listen": "127.0.0.1:0",
            "upstream": upstream.to_string(),
            "enabled": true,
        });
        let resp = self
            .http
            .post(format!("{}/proxies", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert!(
            status.is_success(),
            "create proxy {name} failed: {status} {text}"
        );
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let listen = json["listen"].as_str().unwrap();
        listen.parse().unwrap()
    }

    /// Add a toxic to `proxy`. `attributes` carries the toxic-specific fields.
    async fn add_toxic(
        &self,
        proxy: &str,
        toxic_name: &str,
        toxic_type: &str,
        attributes: serde_json::Value,
    ) {
        let body = serde_json::json!({
            "name": toxic_name,
            "type": toxic_type,
            "stream": "downstream",
            "toxicity": 1.0,
            "attributes": attributes,
        });
        let resp = self
            .http
            .post(format!("{}/proxies/{proxy}/toxics", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        assert!(
            status.is_success(),
            "add toxic {toxic_type} to {proxy} failed: {status} {}",
            resp.text().await.unwrap()
        );
    }

    async fn delete_proxy(&self, name: &str) {
        let _ = self
            .http
            .delete(format!("{}/proxies/{name}", self.base))
            .send()
            .await;
    }
}

// --- Worker-side helpers (re-implemented; http_test.rs is a private module) --

fn creds() -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(7),
        worker_epoch: 3,
        secret: SecretString::from("net-resilience-secret"),
    }
}

fn fixed_time() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_779_192_000).unwrap()
}

fn request(lease_id: LeaseId) -> OperationRequest {
    OperationRequest {
        operation: OperationKind::ProbeFile,
        lease_id,
        payload: serde_json::json!({}),
        heartbeat_deadline_ms: 1_000,
        progress_idle_deadline_ms: 1_000,
    }
}

fn result_frame(lease_id: LeaseId) -> ProgressFrame {
    ProgressFrame::Result {
        lease_id,
        seq: 0,
        emitted_at: fixed_time(),
        payload: serde_json::json!({"ok": true}),
    }
}

/// A handler that returns one buffered terminal frame. It fires only in the
/// dispatch scenarios; a minimal valid response is sufficient because those
/// scenarios fault the connection around the response.
fn operation_handler() -> OperationHandler {
    Arc::new(|req: OperationRequest| {
        Box::pin(async move {
            let mut body = serde_json::to_vec(&result_frame(req.lease_id)).unwrap();
            body.push(b'\n');
            Ok(OperationDispatch::buffered(
                OperationResponse {
                    lease_id: req.lease_id,
                    accepted_at: fixed_time(),
                },
                body,
            ))
        })
    })
}

async fn running_server() -> (SocketAddr, ServerRunning) {
    let server = HttpServer::new(creds(), operation_handler());
    let running = server.serve("127.0.0.1:0".parse().unwrap()).await.unwrap();
    (running.bound, running)
}

async fn shutdown(running: ServerRunning) {
    let _ = running.shutdown.send(());
    let _ = running.joined.await;
}

const SHORT: Duration = Duration::from_millis(500);
const LENIENT: Duration = Duration::from_secs(2);

// --- Scenarios --------------------------------------------------------------

#[tokio::test]
#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]
async fn handshake_timeout_yields_timeout_error() {
    let (upstream, running) = running_server().await;
    let tp = Toxiproxy::new();
    let name = "net-resilience-handshake-timeout";
    let proxy = tp.create_proxy(name, upstream).await;
    tp.add_toxic(
        name,
        "stall",
        "timeout",
        serde_json::json!({ "timeout": 0 }),
    )
    .await;

    let client = HttpClient::with_timeouts(proxy, SHORT, SHORT);
    let err = client.handshake(PROTOCOL_VERSION).await.unwrap_err();
    assert!(
        matches!(err, ProtocolError::Timeout { .. }),
        "expected Timeout, got {err:?}"
    );

    tp.delete_proxy(name).await;
    shutdown(running).await;
}

#[tokio::test]
#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]
async fn dispatch_timeout_yields_timeout_error() {
    // A full-downstream timeout toxic blocks the response head, so the client
    // hangs at `request().await` and the dispatch deadline fires there — this
    // validates the dispatch-deadline wrapper, not the response-line read.
    let (upstream, running) = running_server().await;
    let tp = Toxiproxy::new();
    let name = "net-resilience-dispatch-timeout";
    let proxy = tp.create_proxy(name, upstream).await;
    tp.add_toxic(
        name,
        "stall",
        "timeout",
        serde_json::json!({ "timeout": 0 }),
    )
    .await;

    let client = HttpClient::with_timeouts(proxy, SHORT, SHORT);
    let err = client
        .dispatch(
            &creds(),
            "net-resilience-dispatch-timeout",
            request(LeaseId(1)),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ProtocolError::Timeout { .. }),
        "expected Timeout, got {err:?}"
    );

    tp.delete_proxy(name).await;
    shutdown(running).await;
}

#[tokio::test]
#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]
async fn handshake_reset_yields_connection_error_not_timeout() {
    let (upstream, running) = running_server().await;
    let tp = Toxiproxy::new();
    let name = "net-resilience-handshake-reset";
    let proxy = tp.create_proxy(name, upstream).await;
    tp.add_toxic(
        name,
        "rst",
        "reset_peer",
        serde_json::json!({ "timeout": 0 }),
    )
    .await;

    let client = HttpClient::with_timeouts(proxy, LENIENT, LENIENT);
    let err = client.handshake(PROTOCOL_VERSION).await.unwrap_err();
    assert_connection_error(&err);

    tp.delete_proxy(name).await;
    shutdown(running).await;
}

#[tokio::test]
#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]
async fn dispatch_reset_yields_connection_error_not_timeout() {
    let (upstream, running) = running_server().await;
    let tp = Toxiproxy::new();
    let name = "net-resilience-dispatch-reset";
    let proxy = tp.create_proxy(name, upstream).await;
    tp.add_toxic(
        name,
        "rst",
        "reset_peer",
        serde_json::json!({ "timeout": 0 }),
    )
    .await;

    let client = HttpClient::with_timeouts(proxy, LENIENT, LENIENT);
    let err = client
        .dispatch(
            &creds(),
            "net-resilience-dispatch-reset",
            request(LeaseId(1)),
        )
        .await
        .unwrap_err();
    assert_connection_error(&err);

    tp.delete_proxy(name).await;
    shutdown(running).await;
}

#[tokio::test]
#[ignore = "run via just net-resilience; requires a toxiproxy-server process"]
async fn latency_under_deadline_succeeds() {
    // Liveness + latency tolerance against the *production* handshake deadline
    // (HttpClient::new = 10s), so a slow-but-alive link is not spuriously failed
    // and a future deadline tightened below the injected latency would trip this.
    let (upstream, running) = running_server().await;
    let tp = Toxiproxy::new();
    let name = "net-resilience-latency";
    let proxy = tp.create_proxy(name, upstream).await;
    tp.add_toxic(
        name,
        "slow",
        "latency",
        serde_json::json!({ "latency": 200, "jitter": 0 }),
    )
    .await;

    let client = HttpClient::new(proxy);
    let resp = client.handshake(PROTOCOL_VERSION).await.unwrap();
    assert_eq!(resp.agreed, PROTOCOL_VERSION);

    tp.delete_proxy(name).await;
    shutdown(running).await;
}

/// The invariant for a connection fault: a typed, non-`Timeout` error (proves
/// the client surfaced the fault promptly instead of hanging until the
/// deadline). Tightened to the `request:`-prefixed `InvalidPayload` that
/// `client.rs` produces at the `request().await` seam.
fn assert_connection_error(err: &ProtocolError) {
    assert!(
        !matches!(err, ProtocolError::Timeout { .. }),
        "connection fault must not surface as Timeout: {err:?}"
    );
    match err {
        ProtocolError::InvalidPayload { detail } => assert!(
            detail.starts_with("request:"),
            "expected a `request:` connection-seam detail, got: {detail}"
        ),
        other => panic!("expected InvalidPayload for a connection fault, got {other:?}"),
    }
}
