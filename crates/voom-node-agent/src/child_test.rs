use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tempfile::TempDir;
use voom_core::{ArtifactAccessMode, OperationKind, WorkerId};
use voom_worker_protocol::{
    HttpServer, LocalWorkerBound, NvidiaVideoAcceleratorDescriptor, OperationHandler,
    ProtocolError, ServerHandle, ServerRunning, VaapiVideoAcceleratorDescriptor,
    VideoAcceleratorDescriptor, VideoToolboxVideoAcceleratorDescriptor, WorkerCredentials,
};

use crate::config::WorkerDependencyPaths;

use super::*;

#[test]
fn readiness_parser_accepts_only_nonzero_ipv4_loopback() {
    let valid = parse_bound_line("BOUND addr=127.0.0.1:4321").unwrap();
    assert_eq!(
        valid.addr,
        SocketAddr::V4("127.0.0.1:4321".parse().unwrap())
    );
    assert_eq!(valid.accelerator, None);

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

#[test]
fn readiness_parser_accepts_strict_structured_accelerator_metadata() {
    let bound = LocalWorkerBound {
        addr: SocketAddr::V4("127.0.0.1:4321".parse().unwrap()),
        accelerator: Some(vaapi_descriptor()),
    };
    let line = format!("BOUND {}", serde_json::to_string(&bound).unwrap());
    assert_eq!(parse_bound_line(&line).unwrap(), bound);

    let mut value = serde_json::to_value(bound).unwrap();
    value["accelerator"]["render_node"] = serde_json::json!("/dev/dri/renderD128");
    let line = format!("BOUND {}", serde_json::to_string(&value).unwrap());
    assert!(parse_bound_line(&line).is_err());
}

#[test]
fn production_startup_deadlines_cover_each_accelerator_probe_graph() {
    let deadlines = StartupDeadline::BackendSpecific;
    assert_eq!(deadlines.timeout(None), STARTUP_TIMEOUT);
    assert_eq!(
        deadlines.timeout(Some(&VideoAcceleratorDescriptor::Nvidia(
            nvidia_descriptor_data()
        ))),
        NVIDIA_STARTUP_TIMEOUT
    );
    assert_eq!(
        deadlines.timeout(Some(&vaapi_descriptor())),
        VAAPI_PREFLIGHT_BUDGET
    );
    assert_eq!(
        deadlines.timeout(Some(&VideoAcceleratorDescriptor::VideoToolbox(
            videotoolbox_descriptor_data()
        ))),
        VIDEOTOOLBOX_PREFLIGHT_BUDGET
    );
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
async fn probe_child_receives_only_its_explicit_ffprobe_dependency() {
    let credentials = credentials(9, 4, "probe-secret");
    let server = identity_server(credentials.clone()).await;
    let fixture = NativeChildFixture::new(server.bound());
    let ffprobe = fixture.write_dependency("ffprobe");
    let spec = fixture.spec_for(
        "probe",
        credentials,
        &[],
        vec![OperationKind::ProbeFile],
        WorkerDependencyPaths {
            ffprobe_bin: Some(ffprobe.clone()),
            ..WorkerDependencyPaths::default()
        },
    );
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(200));

    let children = supervisor.start_all(vec![spec]).await.unwrap();
    let record = fixture.record();
    assert_eq!(
        record.env.get("VOOM_FFPROBE_BIN").map(String::as_str),
        ffprobe.to_str()
    );
    assert!(!record.env.contains_key("VOOM_FFMPEG_BIN"));
    assert!(!record.env.contains_key("VOOM_NVIDIA_SMI_BIN"));
    assert!(!record.env.contains_key("PATH"));
    assert_eq!(record.env.len(), 5, "unexpected child environment size");

    supervisor.shutdown_all(children).await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn accelerator_child_reaches_ready_with_explicit_dependencies_and_cleared_environment() {
    let credentials = credentials(17, 5, "accelerator-secret");
    let server = identity_server(credentials.clone()).await;
    let fixture = ChildFixture::new();
    let descriptor = VideoAcceleratorDescriptor::Nvidia(nvidia_descriptor_data());
    let bound = LocalWorkerBound {
        addr: SocketAddr::V4(server.bound()),
        accelerator: Some(descriptor.clone()),
    };
    fixture.write(
        &format!("BOUND {}", serde_json::to_string(&bound).unwrap()),
        0,
        true,
        false,
    );
    let dependencies = WorkerDependencyPaths {
        ffmpeg_bin: Some(fixture.write_dependency("ffmpeg")),
        ffprobe_bin: Some(fixture.write_dependency("ffprobe")),
        nvidia_smi_bin: Some(fixture.write_dependency("nvidia-smi")),
    };
    let supervisor =
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(200));
    let children = supervisor
        .start_all(vec![fixture.spec_with_accelerator_and_dependencies(
            "ffmpeg",
            credentials.clone(),
            &[],
            Some(descriptor.clone()),
            dependencies.clone(),
        )])
        .await
        .unwrap();
    let record = fixture.record();
    for (name, expected) in [
        (
            "VOOM_NVIDIA_DEVICE",
            "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ),
        ("VOOM_NVIDIA_MAX_SESSIONS", "2"),
        (
            "CUDA_VISIBLE_DEVICES",
            "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ),
    ] {
        assert_eq!(record.env.get(name).map(String::as_str), Some(expected));
    }
    for (name, expected) in [
        ("VOOM_FFMPEG_BIN", dependencies.ffmpeg_bin.as_ref().unwrap()),
        (
            "VOOM_FFPROBE_BIN",
            dependencies.ffprobe_bin.as_ref().unwrap(),
        ),
        (
            "VOOM_NVIDIA_SMI_BIN",
            dependencies.nvidia_smi_bin.as_ref().unwrap(),
        ),
    ] {
        assert_eq!(
            record.env.get(name).map(String::as_str),
            expected.to_str(),
            "{name}"
        );
    }
    assert!(!record.env.contains_key("PATH"));
    assert_dependency_paths_redacted(&children[0], &dependencies);
    supervisor.shutdown_all(children).await.unwrap();

    let mut mismatched = nvidia_descriptor_data();
    mismatched.driver_version = "different driver".to_owned();
    let bound = LocalWorkerBound {
        addr: SocketAddr::V4(server.bound()),
        accelerator: Some(VideoAcceleratorDescriptor::Nvidia(mismatched)),
    };
    fixture.write(
        &format!("BOUND {}", serde_json::to_string(&bound).unwrap()),
        0,
        true,
        false,
    );
    let error = supervisor
        .start_all(vec![fixture.spec_with_accelerator_and_dependencies(
            "ffmpeg",
            credentials,
            &[],
            Some(descriptor),
            dependencies,
        )])
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("did not match activation declaration"),
        "{error}"
    );
    assert_reaped(fixture.record().pid);
    server.stop().await;
}

#[tokio::test]
async fn readiness_is_length_newline_time_and_address_bounded() {
    // The startup budget must clear interpreter startup: the child records its pid as its
    // first action, and a budget tight enough to kill it mid-startup leaves no record for
    // the reap assertion to read. The slow case then has to exceed the larger budget.
    let cases = [
        ("x".repeat(4097), 0, true),
        ("BOUND addr=127.0.0.1:4321".to_owned(), 0, false),
        ("BOUND addr=127.0.0.1:4321".to_owned(), 3_000, true),
        ("malformed".to_owned(), 0, true),
        ("BOUND addr=0.0.0.0:4321".to_owned(), 0, true),
        ("BOUND addr=[::1]:4321".to_owned(), 0, true),
        ("BOUND addr=127.0.0.1:0".to_owned(), 0, true),
    ];
    for (line, delay_ms, newline) in cases {
        let fixture = ChildFixture::new();
        fixture.write(&line, delay_ms, newline, false);
        let supervisor =
            ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(40));
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
        ChildSupervisor::with_timeouts(Duration::from_secs(1), Duration::from_millis(50));
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
    temp: TempDir,
    script: PathBuf,
    record_path: PathBuf,
    exited: PathBuf,
}

struct NativeChildFixture {
    temp: TempDir,
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
            temp,
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
        self.spec_for(
            name,
            credentials,
            operator_args,
            vec![OperationKind::HashFile],
            WorkerDependencyPaths::default(),
        )
    }

    fn spec_for(
        &self,
        name: &str,
        credentials: WorkerCredentials,
        operator_args: &[&str],
        operations: Vec<OperationKind>,
        dependencies: WorkerDependencyPaths,
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
                operations,
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                dependencies,
                accelerator: None,
                max_parallel: 1,
            },
            credentials,
        )
    }

    fn write_dependency(&self, name: &str) -> PathBuf {
        let path = self.temp.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&path);
        path
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
            temp,
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
        self.spec_with_accelerator_and_dependencies(
            name,
            credentials,
            operator_args,
            None,
            WorkerDependencyPaths::default(),
        )
    }

    fn spec_with_accelerator_and_dependencies(
        &self,
        name: &str,
        credentials: WorkerCredentials,
        operator_args: &[&str],
        accelerator: Option<VideoAcceleratorDescriptor>,
        dependencies: WorkerDependencyPaths,
    ) -> ChildSpec {
        let mut args = vec![
            self.script.display().to_string(),
            self.record_path.display().to_string(),
            self.exited.display().to_string(),
        ];
        args.extend(operator_args.iter().map(ToString::to_string));
        let operations = if accelerator.is_some() {
            vec![OperationKind::TranscodeVideo]
        } else {
            vec![OperationKind::HashFile]
        };
        ChildSpec::from_worker(
            &WorkerConfig {
                name: name.to_owned(),
                program: PathBuf::from("/usr/bin/python3"),
                args,
                operations,
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                dependencies,
                accelerator,
                max_parallel: 1,
            },
            credentials,
        )
    }

    fn write_dependency(&self, name: &str) -> PathBuf {
        let path = self.temp.path().join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&path);
        path
    }

    /// Read the record the child writes on start, waiting briefly for it to appear.
    ///
    /// The child is spawned concurrently, so on a loaded machine the record can lag the
    /// assertion that reads it. Waiting keeps the failure message about the child rather
    /// than a bare `NotFound`.
    fn record(&self) -> ChildRecord {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(bytes) = std::fs::read(&self.record_path)
                && let Ok(record) = serde_json::from_slice(&bytes)
            {
                return record;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child never wrote {}",
                self.record_path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
fn assert_dependency_paths_redacted(child: &RunningChild, dependencies: &WorkerDependencyPaths) {
    let child_debug = format!("{child:?}");
    for path in [
        dependencies.ffmpeg_bin.as_ref().unwrap(),
        dependencies.ffprobe_bin.as_ref().unwrap(),
        dependencies.nvidia_smi_bin.as_ref().unwrap(),
    ] {
        assert!(!child_debug.contains(&path.display().to_string()));
    }
}

#[derive(Deserialize)]
struct ChildRecord {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    pid: u32,
}

fn vaapi_descriptor() -> VideoAcceleratorDescriptor {
    VideoAcceleratorDescriptor::Vaapi(vaapi_descriptor_data())
}

fn vaapi_descriptor_data() -> VaapiVideoAcceleratorDescriptor {
    VaapiVideoAcceleratorDescriptor {
        pci_address: "0000:f4:00.0".to_owned(),
        device_name: "Radeon Pro".to_owned(),
        driver_version: "Mesa 26.1".to_owned(),
        encoders: vec!["hevc_vaapi".to_owned()],
        decoders: vec!["hevc".to_owned()],
        max_sessions: 2,
    }
}

fn nvidia_descriptor_data() -> NvidiaVideoAcceleratorDescriptor {
    NvidiaVideoAcceleratorDescriptor {
        hardware_token: "nvidia:GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_uuid: "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        device_name: "RTX A6000".to_owned(),
        driver_version: "595.80".to_owned(),
        encoders: vec!["hevc_nvenc".to_owned()],
        decoders: vec!["hevc_cuvid".to_owned()],
        max_sessions: 2,
    }
}

fn videotoolbox_descriptor_data() -> VideoToolboxVideoAcceleratorDescriptor {
    VideoToolboxVideoAcceleratorDescriptor {
        hardware_token: "videotoolbox:abc123".to_owned(),
        resource_id: "abc123".to_owned(),
        model_identifier: "Mac17,6".to_owned(),
        chip_name: "Apple M5 Max".to_owned(),
        macos_version: "26.0".to_owned(),
        macos_build: "25A123".to_owned(),
        encoders: vec!["hevc_videotoolbox".to_owned()],
        decoders: Vec::new(),
        max_sessions: 2,
    }
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
