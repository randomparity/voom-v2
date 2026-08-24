#![cfg(feature = "test")]
#![expect(
    clippy::panic_in_result_fn,
    reason = "integration tests use direct assertions after fallible transport setup"
)]

mod support;

use std::error::Error;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::routing::get;
use clap::Parser;
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::TlsConnector;
use voom_api::config::{Cli, ServerConfig, ServerLimits};
use voom_api::router_with_control_plane;
use voom_api::server::RunningServer;
use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
use voom_control_plane::workers::RegisterNodeInput;
use voom_control_plane::{ControlPlane, HealthPlane};
use voom_core::{ArtifactAccessMode, NodeIncarnationId, OperationKind};
use voom_events::{Event, EventKind};
use voom_store::repo::audit::events::{EventFilter, EventRepo, Page};
use voom_store::repo::execution::nodes::NodeKind;
use voom_store::test_support::sqlite_url_for;
use voom_test_support::TempDatabase;

use support::certs::{TestCertificate, expired_localhost, rustls_client, valid_localhost};

type TestResult = Result<(), Box<dyn Error>>;

fn tls_config(
    certificate: &TestCertificate,
    limits: Option<ServerLimits>,
) -> Result<ServerConfig, Box<dyn Error>> {
    tls_config_at(
        certificate,
        limits,
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
    )
}

fn tls_config_at(
    certificate: &TestCertificate,
    limits: Option<ServerLimits>,
    bind: SocketAddr,
) -> Result<ServerConfig, Box<dyn Error>> {
    let cert = certificate.cert_path.to_string_lossy().into_owned();
    let key = certificate.key_path.to_string_lossy().into_owned();
    let bind = bind.to_string();
    let config = Cli::try_parse_from([
        "voom-api",
        "--bind",
        &bind,
        "--tls-cert",
        &cert,
        "--tls-key",
        &key,
    ])?
    .validate()?;
    Ok(match limits {
        Some(limits) => config.with_limits_for_test(limits),
        None => config,
    })
}

fn cleartext_config(limits: ServerLimits) -> Result<ServerConfig, Box<dyn Error>> {
    Ok(Cli::try_parse_from([
        "voom-api",
        "--bind",
        "127.0.0.1:0",
        "--allow-cleartext-loopback",
    ])?
    .validate()?
    .with_limits_for_test(limits))
}

async fn tls_request(
    addr: SocketAddr,
    client: Arc<rustls::ClientConfig>,
    request: &[u8],
) -> Result<(Vec<u8>, Option<Vec<u8>>), Box<dyn Error>> {
    let tcp = TcpStream::connect(addr).await?;
    let name = rustls::pki_types::ServerName::try_from("localhost")?.to_owned();
    let mut stream = TlsConnector::from(client).connect(name, tcp).await?;
    let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
    stream.write_all(request).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    Ok((response, alpn))
}

#[tokio::test]
async fn trusted_ca_reaches_http1_health() -> TestResult {
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let server = RunningServer::start(
        tls_config(&certificate, None)?,
        Router::new().route("/health", get(|| async { "healthy" })),
    )
    .await?;
    let client = rustls_client(certificate.ca_der.clone(), vec![b"http/1.1".to_vec()])?;
    let (response, alpn) = tls_request(
        server.local_addr(),
        client,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    )
    .await?;

    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with(b"healthy"));
    assert_eq!(alpn.as_deref(), Some(b"http/1.1".as_slice()));
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn unknown_ca_expired_cert_and_http2_only_fail_before_http() -> TestResult {
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let other_directory = TempDir::new()?;
    let other = valid_localhost(other_directory.path())?;
    let server = RunningServer::start(tls_config(&certificate, None)?, Router::new()).await?;
    let untrusted = rustls_client(other.ca_der, vec![b"http/1.1".to_vec()])?;
    assert!(
        tls_request(
            server.local_addr(),
            untrusted,
            b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .await
        .is_err()
    );
    let h2_only = rustls_client(certificate.ca_der, vec![b"h2".to_vec()])?;
    assert!(
        tls_request(
            server.local_addr(),
            h2_only,
            b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
        )
        .await
        .is_err()
    );
    server.shutdown_on(std::future::ready(())).await?;

    let expired_directory = TempDir::new()?;
    let expired = expired_localhost(expired_directory.path())?;
    let expired_server = RunningServer::start(tls_config(&expired, None)?, Router::new()).await?;
    let client = rustls_client(expired.ca_der, vec![b"http/1.1".to_vec()])?;
    assert!(
        tls_request(
            expired_server.local_addr(),
            client,
            b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .await
        .is_err()
    );
    expired_server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn tls_handshake_and_request_head_are_bounded() -> TestResult {
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let short = ServerLimits::new_for_test(
        1024 * 1024,
        Duration::from_millis(25),
        Duration::from_millis(25),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )?;
    let server =
        RunningServer::start(tls_config(&certificate, Some(short))?, Router::new()).await?;
    let mut silent = TcpStream::connect(server.local_addr()).await?;
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), silent.read_to_end(&mut bytes)).await??;
    assert!(bytes.is_empty());

    let tcp = TcpStream::connect(server.local_addr()).await?;
    let name = rustls::pki_types::ServerName::try_from("localhost")?.to_owned();
    let client = rustls_client(certificate.ca_der, vec![b"http/1.1".to_vec()])?;
    let mut stream = TlsConnector::from(client).connect(name, tcp).await?;
    stream.write_all(b"GET / HTTP/1.1\r\nHost:").await?;
    let mut response = Vec::new();
    let read =
        tokio::time::timeout(Duration::from_secs(1), stream.read_to_end(&mut response)).await?;
    if let Err(error) = read {
        assert!(matches!(
            error.kind(),
            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
        ));
    }
    assert!(response.is_empty());
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn invalid_bearer_keeps_existing_generic_401_without_token_leak() -> TestResult {
    let database = TempDatabase::new()?;
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await?;
    let control_plane = ControlPlane::open(&url).await?;
    let registered = control_plane
        .register_node(RegisterNodeInput {
            name: "tls-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await?;
    assert!(!registered.token.expose_secret().is_empty());
    let health_plane = HealthPlane::open(&url).await?;
    let router = router_with_control_plane(health_plane, control_plane);
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let server = RunningServer::start(tls_config(&certificate, None)?, router).await?;
    let client = rustls_client(certificate.ca_der, vec![b"http/1.1".to_vec()])?;
    let sentinel = "sentinel-invalid-bearer-secret";
    let request_body = json!({
        "incarnation_id": "0123456789abcdef0123456789abcdef"
    })
    .to_string();
    let request = format!(
        "POST /v1/execution/node/{}/heartbeat HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer {sentinel}\r\nContent-Type: application/json\r\n\
         X-Voom-Idempotency-Key: tls-invalid-token\r\nContent-Length: {}\r\n\r\n{request_body}",
        registered.node.id.0,
        request_body.len()
    );
    let (response, _) = tls_request(server.local_addr(), client, request.as_bytes()).await?;
    let response = String::from_utf8(response)?;
    assert!(response.starts_with("HTTP/1.1 401 Unauthorized\r\n"));
    assert!(
        response
            .to_ascii_lowercase()
            .contains("www-authenticate: bearer")
    );
    assert!(response.contains("\"code\":\"UNAUTHORIZED\""));
    assert!(!response.contains(sentinel));
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn valid_bearer_commits_heartbeat_through_real_tls() -> TestResult {
    let database = TempDatabase::new()?;
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await?;
    let control_plane = ControlPlane::open(&url).await?;
    let registered = control_plane
        .register_node(RegisterNodeInput {
            name: "tls-success-node".to_owned(),
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
            idempotency_key: "tls-activation".to_owned(),
            request_hash: "tls-activation-body".to_owned(),
            incarnation_id,
            workers: vec![RemoteWorkerDeclaration {
                logical_name: "tls-worker".to_owned(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                accelerator: None,
                max_parallel: 1,
            }],
        })
        .await?;
    let health_plane = HealthPlane::open(&url).await?;
    let router = router_with_control_plane(health_plane, control_plane.clone());
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let server = RunningServer::start(tls_config(&certificate, None)?, router).await?;
    let client = rustls_client(certificate.ca_der, vec![b"http/1.1".to_vec()])?;
    let request_body = json!({"incarnation_id": incarnation_id}).to_string();
    let request = format!(
        "POST /v1/execution/node/{}/heartbeat HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer {}\r\nContent-Type: application/json\r\n\
         X-Voom-Idempotency-Key: tls-valid-heartbeat\r\nContent-Length: {}\r\n\r\n{request_body}",
        registered.node.id.0,
        registered.token.expose_secret(),
        request_body.len()
    );
    let (response, _) = tls_request(server.local_addr(), client, request.as_bytes()).await?;
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    let response: Value = serde_json::from_slice(http_response_body(&response)?)?;
    assert_eq!(response["schema_version"], "0");
    assert_eq!(response["command"], "execution.node_heartbeat");
    assert_eq!(response["status"], "ok");
    assert_eq!(
        heartbeat_event_count(&control_plane, registered.node.id).await?,
        1
    );
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn chunked_oversized_execution_json_uses_exact_413_envelope() -> TestResult {
    let database = TempDatabase::new()?;
    let url = sqlite_url_for(database.path());
    voom_store::init(&url).await?;
    let control_plane = ControlPlane::open(&url).await?;
    let registered = control_plane
        .register_node(RegisterNodeInput {
            name: "tls-chunked-node".to_owned(),
            kind: NodeKind::Remote,
            heartbeat_ttl_seconds: 60,
            metadata: json!({}),
        })
        .await?;
    let health_plane = HealthPlane::open(&url).await?;
    let router = router_with_control_plane(health_plane, control_plane);
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    let server = RunningServer::start(tls_config(&certificate, None)?, router).await?;
    let client = rustls_client(certificate.ca_der, vec![b"http/1.1".to_vec()])?;
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    let mut request = format!(
        "POST /v1/execution/node/{}/heartbeat HTTP/1.1\r\nHost: localhost\r\n\
         Authorization: Bearer {}\r\nContent-Type: application/json\r\n\
         X-Voom-Idempotency-Key: tls-chunked-oversized\r\n\
         Transfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        registered.node.id.0,
        registered.token.expose_secret(),
        oversized.len()
    )
    .into_bytes();
    request.extend_from_slice(&oversized);
    request.extend_from_slice(b"\r\n0\r\n\r\n");

    let (response, _) = tls_request(server.local_addr(), client, &request).await?;
    assert!(response.starts_with(b"HTTP/1.1 413 Payload Too Large\r\n"));
    assert_eq!(
        serde_json::from_slice::<Value>(http_response_body(&response)?)?,
        json!({
            "schema_version": "0",
            "command": "api.request",
            "status": "error",
            "data": null,
            "warnings": [],
            "error": {
                "code": "PAYLOAD_TOO_LARGE",
                "message": "request body exceeds the 1048576-byte limit",
                "hint": "Send a request body of 1048576 bytes or fewer"
            }
        })
    );
    server.shutdown_on(std::future::ready(())).await?;
    Ok(())
}

#[tokio::test]
async fn malformed_pem_diagnostic_is_static_and_bind_never_opens() -> TestResult {
    let directory = TempDir::new()?;
    let cert_path = directory.path().join("sentinel-cert-path.pem");
    let key_path = directory.path().join("sentinel-key-path.pem");
    let sentinel = "sentinel-private-key-material";
    std::fs::write(&cert_path, sentinel)?;
    std::fs::write(&key_path, sentinel)?;
    let certificate = TestCertificate {
        ca_der: rustls::pki_types::CertificateDer::from(Vec::new()),
        cert_path: cert_path.clone(),
        key_path: key_path.clone(),
    };
    let reservation = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let bind = reservation.local_addr()?;
    drop(reservation);
    let result =
        RunningServer::start(tls_config_at(&certificate, None, bind)?, Router::new()).await;
    let error = result.err().ok_or("malformed PEM unexpectedly started")?;
    let diagnostic = error.to_string();

    assert!(diagnostic.contains("--tls-cert"));
    assert!(diagnostic.contains("--tls-key"));
    assert!(!diagnostic.contains(sentinel));
    assert!(!diagnostic.contains(&cert_path.to_string_lossy().into_owned()));
    assert!(!diagnostic.contains(&key_path.to_string_lossy().into_owned()));
    assert!(TcpStream::connect(bind).await.is_err());
    Ok(())
}

#[tokio::test]
async fn slow_reader_is_cut_off_for_cleartext_and_tls() -> TestResult {
    let directory = TempDir::new()?;
    let certificate = valid_localhost(directory.path())?;
    for use_tls in [false, true] {
        let limits = ServerLimits::new_for_test(
            1024 * 1024,
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_millis(50),
            Duration::from_secs(1),
        )?;
        let config = if use_tls {
            tls_config(&certificate, Some(limits))?
        } else {
            cleartext_config(limits)?
        };
        let server = RunningServer::start(
            config,
            Router::new().route(
                "/large",
                get(|| async { Bytes::from(vec![b'x'; 8 * 1024 * 1024]) }),
            ),
        )
        .await?;
        let tcp = small_receive_buffer_stream(server.local_addr()).await?;
        if use_tls {
            let name = rustls::pki_types::ServerName::try_from("localhost")?.to_owned();
            let client = rustls_client(certificate.ca_der.clone(), vec![b"http/1.1".to_vec()])?;
            let mut stream = TlsConnector::from(client).connect(name, tcp).await?;
            stream
                .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await?;
            wait_for_connection_cycle(&server).await?;
            drop(stream);
        } else {
            let mut stream = tcp;
            stream
                .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await?;
            wait_for_connection_cycle(&server).await?;
            drop(stream);
        }
        server.shutdown_on(std::future::ready(())).await?;
    }
    Ok(())
}

async fn wait_for_connection_cycle(server: &RunningServer) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.connection_count() == 0 {
            tokio::task::yield_now().await;
        }
        while server.connection_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

async fn small_receive_buffer_stream(addr: SocketAddr) -> Result<TcpStream, std::io::Error> {
    let socket = TcpSocket::new_v4()?;
    socket.set_recv_buffer_size(1024)?;
    socket.connect(addr).await
}

fn http_response_body(response: &[u8]) -> Result<&[u8], std::io::Error> {
    let offset = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing HTTP body"))?;
    Ok(&response[offset + 4..])
}

async fn heartbeat_event_count(
    control_plane: &ControlPlane,
    node_id: voom_core::NodeId,
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
