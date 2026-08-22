#![expect(
    clippy::panic_in_result_fn,
    reason = "the fallible TLS fixture returns Result while assertions verify trust behavior"
)]

use std::error::Error;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Json;
use axum::http::HeaderMap;
use axum::routing::post;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use secrecy::SecretString;
use serde_json::{Value as JsonValue, json};
use tempfile::TempDir;
use time::{Duration as TimeDuration, OffsetDateTime};
use voom_core::{ArtifactAccessMode, NodeId, OperationKind};
use voom_node_agent::client::{ControlPlaneClient, NodeHeartbeatRequest, RetryRequest};
use voom_node_agent::config::{AgentConfig, LoadedAgentConfig, TokenSource, WorkerConfig};

#[tokio::test]
async fn custom_ca_trust_is_hostname_bound() -> Result<(), Box<dyn Error>> {
    install_crypto_provider();
    let temp = TempDir::new()?;
    let certificate = localhost_certificate(temp.path())?;
    let tls =
        axum_server::tls_rustls::RustlsConfig::from_pem_file(&certificate.cert, &certificate.key)
            .await?;
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let handle = axum_server::Handle::new();
    let server_handle = handle.clone();
    let app = axum::Router::new().route(
        "/v1/execution/node/7/heartbeat",
        post(authenticated_heartbeat),
    );
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)?
            .handle(server_handle)
            .serve(app.into_make_service())
            .await
    });

    let request = RetryRequest::new(
        "heartbeat-key".to_owned(),
        &NodeHeartbeatRequest {
            incarnation_id: "0123456789abcdef0123456789abcdef".parse()?,
        },
    )?;
    let trusted = loaded_config(
        &format!("https://localhost:{}", address.port()),
        Some(certificate.ca.clone()),
    );
    let outcome = ControlPlaneClient::from_config(&trusted)?
        .node_heartbeat(NodeId(7), &request)
        .await?;
    assert_eq!(outcome.node_id, NodeId(7));
    assert_eq!(outcome.status, "active");

    let untrusted = loaded_config(&format!("https://localhost:{}", address.port()), None);
    assert_unbounded_request_keeps_retrying(
        ControlPlaneClient::from_config_with_unbounded_retries(&untrusted)?,
        &request,
    )
    .await;

    let wrong_hostname = loaded_config(
        &format!("https://127.0.0.1:{}", address.port()),
        Some(certificate.ca),
    );
    assert_unbounded_request_keeps_retrying(
        ControlPlaneClient::from_config_with_unbounded_retries(&wrong_hostname)?,
        &request,
    )
    .await;

    handle.graceful_shutdown(Some(Duration::from_secs(1)));
    tokio::time::timeout(Duration::from_secs(2), server).await???;
    Ok(())
}

async fn authenticated_heartbeat(
    headers: HeaderMap,
    Json(body): Json<JsonValue>,
) -> Json<JsonValue> {
    assert_eq!(headers["authorization"], "Bearer secret-token");
    assert_eq!(headers["x-voom-idempotency-key"], "heartbeat-key");
    assert_eq!(
        body,
        json!({"incarnation_id": "0123456789abcdef0123456789abcdef"})
    );
    Json(json!({
        "schema_version": "0",
        "command": "execution.node_heartbeat",
        "status": "ok",
        "data": {"node_id": 7, "status": "active"},
        "warnings": [],
        "error": null
    }))
}

async fn assert_unbounded_request_keeps_retrying(
    client: ControlPlaneClient,
    request: &RetryRequest<NodeHeartbeatRequest>,
) {
    let result = tokio::time::timeout(
        Duration::from_millis(400),
        client.node_heartbeat(NodeId(7), request),
    )
    .await;
    assert!(result.is_err(), "TLS failure must remain retryable");
}

struct TestCertificate {
    ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

fn localhost_certificate(directory: &Path) -> Result<TestCertificate, Box<dyn Error>> {
    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate()?;
    let ca_cert = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::new(ca_params, ca_key);

    let mut server_params = CertificateParams::new(vec!["localhost".to_owned()])?;
    server_params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    server_params.not_before = OffsetDateTime::now_utc() - TimeDuration::days(1);
    server_params.not_after = OffsetDateTime::now_utc() + TimeDuration::days(1);
    let server_key = KeyPair::generate()?;
    let server_cert = server_params.signed_by(&server_key, &issuer)?;

    let ca = directory.join("ca.pem");
    let cert = directory.join("server.pem");
    let key = directory.join("server.key");
    std::fs::write(&ca, ca_cert.pem())?;
    std::fs::write(&cert, server_cert.pem())?;
    std::fs::write(&key, server_key.serialize_pem())?;
    Ok(TestCertificate { ca, cert, key })
}

fn loaded_config(url: &str, ca_cert: Option<PathBuf>) -> LoadedAgentConfig {
    LoadedAgentConfig {
        config: AgentConfig {
            control_plane_url: url.to_owned(),
            ca_cert,
            node_id: NodeId(7),
            poll_interval_ms: 1_000,
            lease_ttl_seconds: 30,
            progress_idle_timeout_seconds: 300,
            shutdown_grace_seconds: 10,
            storage_roots: Vec::new(),
            node_token: TokenSource::Env {
                name: "VOOM_NODE_TOKEN".to_owned(),
            },
            workers: vec![WorkerConfig {
                name: "echo".to_owned(),
                program: PathBuf::from("/bin/echo"),
                args: Vec::new(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            }],
        },
        node_token: SecretString::from("secret-token"),
    }
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
