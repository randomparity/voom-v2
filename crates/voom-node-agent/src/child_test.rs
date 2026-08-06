use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tempfile::TempDir;
use voom_core::{ArtifactAccessMode, OperationKind, WorkerId};
use voom_worker_protocol::{
    HttpServer, OperationHandler, ProtocolError, ServerHandle, ServerRunning, WorkerCredentials,
};

use super::*;

#[test]
fn readiness_parser_accepts_only_nonzero_ipv4_loopback() {
    let valid = parse_bound_line("BOUND addr=127.0.0.1:4321").unwrap();
    assert_eq!(valid.ip(), &std::net::Ipv4Addr::LOCALHOST);
    assert_eq!(valid.port(), 4321);

    for line in [
        "127.0.0.1:4321",
        "BOUND addr=127.0.0.1:0",
        "BOUND addr=0.0.0.0:4321",
        "BOUND addr=192.0.2.1:4321",
        "BOUND addr=[::1]:4321",
        "BOUND addr=not-an-address",
    ] {
        assert!(parse_bound_line(line).is_err(), "{line}");
    }
}

#[tokio::test]
async fn child_receives_direct_argv_and_exact_environment_then_exits_on_eof() {
    let credentials = credentials(7, 3, "test-secret");
    let server = identity_server(credentials.clone()).await;
    let fixture = NativeChildFixture::new(server.bound());
    let spec = fixture.spec(
        "echo",
        credentials.clone(),
        &["literal arg", "$(must-not-execute)", "semi;colon"],
    );
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(200));

    let children = supervisor.start_all(vec![spec]).await.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].endpoint(), server.bound());
    assert_eq!(children[0].spec().credentials.worker_id, WorkerId(7));
    let record = fixture.record();
    assert_eq!(
        record.argv,
        ["literal arg", "$(must-not-execute)", "semi;colon"]
    );
    assert_eq!(record.env.len(), 4, "unexpected child environment size");
    for (name, value) in [
        ("VOOM_WORKER_BIND", "127.0.0.1:0"),
        ("VOOM_WORKER_EPOCH", "3"),
        ("VOOM_WORKER_ID", "7"),
        ("VOOM_WORKER_SECRET", "test-secret"),
    ] {
        assert!(
            record.env.get(name).is_some_and(|actual| actual == value),
            "child environment value missing or incorrect for {name}"
        );
    }
    assert!(!format!("{:?}", children[0]).contains("test-secret"));

    supervisor.shutdown_all(children).await.unwrap();
    assert_eq!(std::fs::read_to_string(&fixture.exited).unwrap(), "eof");
    assert_reaped(record.pid);
    server.stop().await;
}

#[tokio::test]
async fn readiness_is_length_newline_time_and_address_bounded() {
    let cases = [
        ("x".repeat(4097), 0, true),
        ("BOUND addr=127.0.0.1:4321".to_owned(), 0, false),
        ("BOUND addr=127.0.0.1:4321".to_owned(), 150, true),
        ("malformed".to_owned(), 0, true),
        ("BOUND addr=0.0.0.0:4321".to_owned(), 0, true),
        ("BOUND addr=[::1]:4321".to_owned(), 0, true),
        ("BOUND addr=127.0.0.1:0".to_owned(), 0, true),
    ];
    for (line, delay_ms, newline) in cases {
        let fixture = ChildFixture::new();
        fixture.write(&line, delay_ms, newline, false);
        let supervisor =
            ChildSupervisor::with_timeouts(Duration::from_millis(40), Duration::from_millis(40));
        let result = supervisor
            .start_all(vec![fixture.spec("bad", credentials(1, 0, "secret"), &[])])
            .await;

        assert!(result.is_err(), "line length={}", line.len());
        assert_reaped(fixture.record().pid);
    }
}

#[tokio::test]
async fn protocol_and_identity_mismatch_kill_and_reap_the_child() {
    let wrong_protocol = wrong_protocol_server().await;
    let protocol_fixture = ChildFixture::new();
    protocol_fixture.write(
        &format!("BOUND addr={}", wrong_protocol.address),
        0,
        true,
        false,
    );
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(50));
    let error = supervisor
        .start_all(vec![protocol_fixture.spec(
            "protocol",
            credentials(2, 0, "secret"),
            &[],
        )])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("protocol handshake"));
    assert_reaped(protocol_fixture.record().pid);
    wrong_protocol.stop().await;

    let expected = credentials(3, 4, "expected-secret");
    let identity_server = identity_server(credentials(30, 4, "different-secret")).await;
    let identity_fixture = ChildFixture::new();
    identity_fixture.write(
        &format!("BOUND addr={}", identity_server.bound()),
        0,
        true,
        false,
    );
    let error = supervisor
        .start_all(vec![identity_fixture.spec("identity", expected, &[])])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("identity proof"));
    assert_reaped(identity_fixture.record().pid);
    identity_server.stop().await;
}

#[tokio::test]
async fn partial_startup_failure_reaps_every_started_sibling() {
    let valid_credentials = credentials(4, 1, "valid-secret");
    let server = identity_server(valid_credentials.clone()).await;
    let valid = ChildFixture::new();
    valid.write(&format!("BOUND addr={}", server.bound()), 0, true, false);
    let invalid = ChildFixture::new();
    invalid.write("not readiness", 0, true, false);
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(100));

    let result = supervisor
        .start_all(vec![
            valid.spec("valid", valid_credentials, &[]),
            invalid.spec("invalid", credentials(5, 0, "invalid-secret"), &[]),
        ])
        .await;

    assert!(result.is_err());
    assert_reaped(valid.record().pid);
    assert_reaped(invalid.record().pid);
    server.stop().await;
}

#[tokio::test]
async fn shutdown_kills_and_reaps_a_child_that_ignores_stdin_eof() {
    let credentials = credentials(6, 2, "stubborn-secret");
    let server = identity_server(credentials.clone()).await;
    let fixture = ChildFixture::new();
    fixture.write(&format!("BOUND addr={}", server.bound()), 0, true, true);
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(30));
    let children = supervisor
        .start_all(vec![fixture.spec("stubborn", credentials, &[])])
        .await
        .unwrap();
    let pid = fixture.record().pid;

    supervisor.shutdown_all(children).await.unwrap();

    assert!(!fixture.exited.exists());
    assert_reaped(pid);
    server.stop().await;
}

#[tokio::test]
async fn restart_preserves_identity_and_resets_only_after_full_startup() {
    let credentials = credentials(8, 9, "restart-secret");
    let server = identity_server(credentials.clone()).await;
    let fixture = ChildFixture::new();
    fixture.write(&format!("BOUND addr={}", server.bound()), 0, true, false);
    let spec = fixture.spec("restart", credentials.clone(), &[]);
    let mut supervisor =
        ChildSupervisor::with_timeouts(Duration::from_millis(100), Duration::from_millis(50));
    let initial = supervisor.start_all(vec![spec.clone()]).await.unwrap();
    supervisor.shutdown_all(initial).await.unwrap();

    fixture.write("bad", 0, true, false);
    assert_eq!(
        supervisor.restart(&spec).await.unwrap_err().kind(),
        ChildErrorKind::Startup
    );
    assert_eq!(
        supervisor.restart(&spec).await.unwrap_err().kind(),
        ChildErrorKind::Startup
    );

    fixture.write(&format!("BOUND addr={}", server.bound()), 0, true, false);
    let restarted = supervisor.restart(&spec).await.unwrap();
    assert_eq!(
        restarted.spec().credentials.worker_id,
        credentials.worker_id
    );
    assert_eq!(
        restarted.spec().credentials.worker_epoch,
        credentials.worker_epoch
    );
    assert!(
        restarted.spec().credentials.secret.expose_secret() == credentials.secret.expose_secret(),
        "restart changed the worker secret"
    );
    supervisor.shutdown_all(vec![restarted]).await.unwrap();

    fixture.write("bad", 0, true, false);
    assert_eq!(
        supervisor.restart(&spec).await.unwrap_err().kind(),
        ChildErrorKind::Startup
    );
    assert_eq!(
        supervisor.restart(&spec).await.unwrap_err().kind(),
        ChildErrorKind::Startup
    );
    assert_eq!(
        supervisor.restart(&spec).await.unwrap_err().kind(),
        ChildErrorKind::RestartExhausted
    );
    assert_reaped(fixture.record().pid);
    server.stop().await;
}

fn credentials(worker_id: u64, worker_epoch: u64, secret: &str) -> WorkerCredentials {
    WorkerCredentials {
        worker_id: WorkerId(worker_id),
        worker_epoch,
        secret: SecretString::from(secret),
    }
}

struct IdentityServer {
    running: ServerRunning,
}

impl IdentityServer {
    fn bound(&self) -> SocketAddrV4 {
        self.running.bound.to_string().parse().unwrap()
    }

    async fn stop(self) {
        let _ = self.running.shutdown.send(());
        self.running.joined.await.unwrap();
    }
}

async fn identity_server(credentials: WorkerCredentials) -> IdentityServer {
    let handler: OperationHandler = Arc::new(|_| {
        Box::pin(async {
            Err(ProtocolError::InvalidPayload {
                detail: "operation handler is unused by child startup tests".to_owned(),
            })
        })
    });
    let running = HttpServer::new(credentials, handler)
        .serve("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    IdentityServer { running }
}

struct WrongProtocolServer {
    address: SocketAddr,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl WrongProtocolServer {
    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn wrong_protocol_server() -> WrongProtocolServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/v1/handshake",
        axum::routing::post(|| async {
            axum::Json(serde_json::json!({"agreed": voom_core::PROTOCOL_VERSION + 1}))
        }),
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    WrongProtocolServer { address, task }
}

struct ChildFixture {
    _temp: TempDir,
    script: PathBuf,
    record_path: PathBuf,
    exited: PathBuf,
}

struct NativeChildFixture {
    _temp: TempDir,
    program: PathBuf,
    record_path: PathBuf,
    exited: PathBuf,
    endpoint: SocketAddrV4,
}

impl NativeChildFixture {
    fn new(endpoint: SocketAddrV4) -> Self {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("worker.rs");
        let program = temp.path().join("worker-fixture");
        let record_path = temp.path().join("record.txt");
        let exited = temp.path().join("exited");
        std::fs::write(
            &source,
            r#"use std::collections::BTreeMap;
use std::io::Read;

fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    let mut lines = vec![format!("pid={}", std::process::id())];
    for argument in &arguments[4..] {
        lines.push(format!("arg={argument}"));
    }
    for (name, value) in std::env::vars().collect::<BTreeMap<_, _>>() {
        lines.push(format!("env={name}={value}"));
    }
    std::fs::write(&arguments[1], lines.join("\n")).unwrap();
    println!("BOUND addr={}", arguments[3]);
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
    std::fs::write(&arguments[2], "eof").unwrap();
}
"#,
        )
        .unwrap();
        let status = std::process::Command::new("rustc")
            .args(["--edition=2024", "-o"])
            .arg(&program)
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        Self {
            _temp: temp,
            program,
            record_path,
            exited,
            endpoint,
        }
    }

    fn spec(
        &self,
        name: &str,
        credentials: WorkerCredentials,
        operator_args: &[&str],
    ) -> ChildSpec {
        let mut args = vec![
            self.record_path.display().to_string(),
            self.exited.display().to_string(),
            self.endpoint.to_string(),
        ];
        args.extend(operator_args.iter().map(ToString::to_string));
        ChildSpec::from_worker(
            &WorkerConfig {
                name: name.to_owned(),
                program: self.program.clone(),
                args,
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            },
            credentials,
        )
    }

    fn record(&self) -> ChildRecord {
        let document = std::fs::read_to_string(&self.record_path).unwrap();
        let mut pid = None;
        let mut argv = Vec::new();
        let mut env = BTreeMap::new();
        for line in document.lines() {
            if let Some(value) = line.strip_prefix("pid=") {
                pid = Some(value.parse().unwrap());
            } else if let Some(value) = line.strip_prefix("arg=") {
                argv.push(value.to_owned());
            } else if let Some(value) = line.strip_prefix("env=") {
                let (name, value) = value.split_once('=').unwrap();
                env.insert(name.to_owned(), value.to_owned());
            }
        }
        ChildRecord {
            argv,
            env,
            pid: pid.unwrap(),
        }
    }
}

impl ChildFixture {
    fn new() -> Self {
        let temp = TempDir::new().unwrap();
        Self {
            script: temp.path().join("worker.py"),
            record_path: temp.path().join("record.json"),
            exited: temp.path().join("exited"),
            _temp: temp,
        }
    }

    fn write(&self, line: &str, delay_ms: u64, newline: bool, ignore_stdin: bool) {
        let line = serde_json::to_string(line).unwrap();
        let wait = if ignore_stdin {
            "\nwhile True:\n    time.sleep(60)\n"
        } else {
            "\nsys.stdin.buffer.read()\nexited.write_text(\"eof\", encoding=\"utf-8\")\n"
        };
        let script = format!(
            r#"#!/usr/bin/python3
import json
import os
import pathlib
import sys
import time

record = pathlib.Path(sys.argv[1])
exited = pathlib.Path(sys.argv[2])
record.write_text(json.dumps({{"argv": sys.argv[3:], "env": dict(os.environ), "pid": os.getpid()}}), encoding="utf-8")
time.sleep({delay_ms} / 1000)
sys.stdout.write({line})
if {}:
    sys.stdout.write("\n")
sys.stdout.flush()
{wait}"#,
            if newline { "True" } else { "False" },
        );
        std::fs::write(&self.script, script).unwrap();
        make_executable(&self.script);
        let _ = std::fs::remove_file(&self.record_path);
        let _ = std::fs::remove_file(&self.exited);
    }

    fn spec(
        &self,
        name: &str,
        credentials: WorkerCredentials,
        operator_args: &[&str],
    ) -> ChildSpec {
        let mut args = vec![
            self.script.display().to_string(),
            self.record_path.display().to_string(),
            self.exited.display().to_string(),
        ];
        args.extend(operator_args.iter().map(ToString::to_string));
        ChildSpec::from_worker(
            &WorkerConfig {
                name: name.to_owned(),
                program: PathBuf::from("/usr/bin/python3"),
                args,
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                max_parallel: 1,
            },
            credentials,
        )
    }

    fn record(&self) -> ChildRecord {
        serde_json::from_slice(&std::fs::read(&self.record_path).unwrap()).unwrap()
    }
}

#[derive(Deserialize)]
struct ChildRecord {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    pid: u32,
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn assert_reaped(pid: u32) {
    assert!(!Path::new(&format!("/proc/{pid}")).exists(), "pid {pid}");
}
