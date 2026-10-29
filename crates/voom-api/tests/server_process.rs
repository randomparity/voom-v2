#![expect(
    clippy::panic_in_result_fn,
    reason = "process tests use assertions after fallible setup"
)]
#![cfg_attr(
    unix,
    expect(
        clippy::unwrap_used,
        reason = "the process contract requires checked kill(1) delivery"
    )
)]

use std::error::Error;
#[cfg(unix)]
use std::io::{BufRead, BufReader, Read};
use std::process::Command;
#[cfg(unix)]
use std::process::{Child, Stdio};
#[cfg(unix)]
use std::sync::mpsc;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::{Duration, Instant};

use tempfile::TempDir;
use voom_store::test_support::sqlite_url_for;
#[cfg(unix)]
use voom_test_support::TempDatabase;

type TestResult = Result<(), Box<dyn Error>>;

fn command(database_url: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_voom-api"));
    command.env("VOOM_DATABASE_URL", database_url);
    command
}

#[test]
fn non_loopback_cleartext_fails_closed_without_secret_output() -> TestResult {
    let output = command("sqlite:///sentinel-secret.db")
        .args(["--bind", "0.0.0.0:7443", "--allow-cleartext-loopback"])
        .output()?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("cleartext requires a loopback --bind"));
    assert!(!stderr.contains("sentinel-secret"));
    Ok(())
}

#[test]
fn missing_database_fails_without_creating_path_or_parent() -> TestResult {
    let directory = TempDir::new()?;
    let parent = directory.path().join("missing-parent");
    let database = parent.join("missing.db");
    let database_url = sqlite_url_for(&database);

    let output = command(&database_url)
        .args(["--bind", "127.0.0.1:0", "--allow-cleartext-loopback"])
        .output()?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!database.exists());
    assert!(!parent.exists());
    assert!(!String::from_utf8(output.stderr)?.contains(&database_url));
    Ok(())
}

#[cfg(unix)]
#[test]
fn sigterm_drains_cleanly_without_running_migrations() -> TestResult {
    let runtime = tokio::runtime::Runtime::new()?;
    let database = TempDatabase::new()?;
    let database_url = sqlite_url_for(database.path());
    runtime.block_on(voom_store::init(&database_url))?;
    let pool = runtime.block_on(voom_store::connect(&database_url))?;
    let migration_count_before = runtime.block_on(migration_count(&pool))?;

    let mut child = command(&database_url)
        .args(["--bind", "127.0.0.1:0", "--allow-cleartext-loopback"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().ok_or("child stderr was not piped")?;
    let mut stdout = child.stdout.take().ok_or("child stdout was not piped")?;
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || -> std::io::Result<Vec<String>> {
        let mut lines = Vec::new();
        for line in BufReader::new(stderr).lines() {
            let line = line?;
            sender.send(line.clone()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "process-log receiver closed before stderr",
                )
            })?;
            lines.push(line);
        }
        Ok(lines)
    });

    if let Err(error) = wait_for_event(&receiver, "listening") {
        send_sigterm(&child);
        child.wait()?;
        reader
            .join()
            .map_err(|_| "stderr reader thread panicked")??;
        return Err(error);
    }
    send_sigterm(&child);
    let status = child.wait()?;
    let mut stdout_bytes = Vec::new();
    stdout.read_to_end(&mut stdout_bytes)?;
    let lines = reader
        .join()
        .map_err(|_| "stderr reader thread panicked")??;

    assert!(status.success());
    assert!(stdout_bytes.is_empty());
    for line in &lines {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\"event\":\"shutdown_complete\""))
    );
    assert_eq!(
        runtime.block_on(migration_count(&pool))?,
        migration_count_before
    );
    Ok(())
}

#[cfg(unix)]
fn wait_for_event(receiver: &mpsc::Receiver<String>, event: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("timed out waiting for structured process event")?;
        let line = receiver.recv_timeout(remaining)?;
        let _: serde_json::Value = serde_json::from_str(&line)?;
        if line.contains(&format!("\"event\":\"{event}\"")) {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn send_sigterm(child: &Child) {
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(
        status.success(),
        "kill -TERM must report successful delivery"
    );
}

#[cfg(unix)]
async fn migration_count(pool: &sqlx::SqlitePool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
}

#[cfg(unix)]
mod recovery_process {
    #![expect(
        clippy::unwrap_used,
        reason = "fixtures use assertions and checked setup"
    )]
    use super::*;
    use secrecy::SecretString;
    use serde_json::json;
    use voom_control_plane::ControlPlane;
    use voom_control_plane::execution::{RemoteActivateInput, RemoteWorkerDeclaration};
    use voom_control_plane::scan::{RemoteScanStartInput, ScanRunOutcome};
    use voom_control_plane::workers::RegisterNodeInput;
    use voom_core::{
        ArtifactAccessMode, LibraryId, NodeId, NodeIncarnationId, NodeKind, OperationKind,
        ProviderLocator, ScanSessionId, ScanSessionStatus, StorageProviderKind, StorageRootId,
    };
    use voom_store::repo::library::libraries::{LibraryMediaKind, NewLibrary};
    use voom_store::repo::library::library_roots::{
        HiddenFilePolicy, LibraryScanMode, NewLibraryRoot, SymlinkPolicy,
    };
    const INCARNATION: &str = "0123456789abcdef0123456789abcdef";
    struct RecoveryFixture {
        database: TempDatabase,
        cp: ControlPlane,
        pool: sqlx::SqlitePool,
        node_id: NodeId,
        incarnation_id: NodeIncarnationId,
        token: SecretString,
    }
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn recovery_process_expires_scan_lease_without_executor() -> TestResult {
        let runtime = tokio::runtime::Runtime::new()?;
        let fixture = runtime.block_on(recovery_fixture());
        let run = runtime.block_on(async {
            let root = create_root(&fixture, "automatic").await;
            let ScanRunOutcome::Requested(run) = fixture.cp.request_scan_run(root, 86_400).await.unwrap() else { return Err("root blocked"); };
            start(&fixture, run.scan_session_id, "automatic").await;
            sqlx::query("UPDATE tickets SET state = 'leased', attempt = max_attempts WHERE id = ?")
                .bind(i64::try_from(run.ticket_id.0).unwrap()).execute(&fixture.pool).await.unwrap();
            sqlx::query("INSERT INTO leases (ticket_id, worker_id, state, acquired_at, expires_at, last_heartbeat_at, ttl_seconds) VALUES (?, (SELECT MIN(id) FROM workers), 'held', '1970-01-01T00:00:00Z', '1970-01-01T00:00:01Z', '1970-01-01T00:00:00Z', 1)")
                .bind(i64::try_from(run.ticket_id.0).unwrap()).execute(&fixture.pool).await.unwrap();
            Ok(run)
        })?;
        let url = sqlite_url_for(fixture.database.path());
        let mut process = Process(
            command(&url)
                .args(["--bind", "127.0.0.1:0", "--allow-cleartext-loopback"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let stderr = process.0.stderr.take().ok_or("missing stderr")?;
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let result = (|| -> TestResult {
            wait_for_event(&receiver, "listening")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let status = runtime
                    .block_on(fixture.cp.scan_session(run.scan_session_id))?
                    .status;
                if status == ScanSessionStatus::Failed {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!("automatic recovery left scan {status:?}").into());
                }
                thread::sleep(Duration::from_millis(20));
            }
            let (ticket, lease): (String, String) = runtime.block_on(sqlx::query_as("SELECT t.state, l.state FROM tickets t JOIN leases l ON l.ticket_id = t.id WHERE t.id = ?").bind(i64::try_from(run.ticket_id.0)?).fetch_one(&fixture.pool))?;
            assert_eq!((ticket.as_str(), lease.as_str()), ("failed", "expired"));
            let owner: String = runtime.block_on(
                sqlx::query_scalar("SELECT status FROM nodes WHERE id = ?")
                    .bind(i64::try_from(fixture.node_id.0)?)
                    .fetch_one(&fixture.pool),
            )?;
            assert_eq!(
                owner, "active",
                "healthy owner must not take the stale-node shortcut"
            );
            Ok(())
        })();
        send_sigterm(&process.0);
        let status = process.0.wait()?;
        reader.join().map_err(|_| "stderr reader panicked")?;
        result?;
        assert!(status.success());
        Ok(())
    }
    async fn recovery_fixture() -> RecoveryFixture {
        let database = TempDatabase::new().unwrap();
        let url = voom_store::test_support::sqlite_url_for(database.path());
        voom_store::init(&url).await.unwrap();
        let pool = voom_store::connect(&url).await.unwrap();
        let cp = ControlPlane::open(&url).await.unwrap();
        let registered = cp
            .register_node(RegisterNodeInput {
                name: "durable-scan-charter-owner".to_owned(),
                kind: NodeKind::Remote,
                heartbeat_ttl_seconds: 600,
                metadata: json!({}),
            })
            .await
            .unwrap();
        let incarnation_id = INCARNATION.parse().unwrap();
        cp.remote_activate(RemoteActivateInput {
            storage_roots: Vec::new(),
            node_id: registered.node.id,
            token: registered.token.clone(),
            idempotency_key: "durable-scan-charter-activate".to_owned(),
            request_hash: "durable-scan-charter-activate-body".to_owned(),
            incarnation_id,
            workers: vec![RemoteWorkerDeclaration {
                logical_name: "scan".to_owned(),
                operations: vec![OperationKind::ProbeFile],
                artifact_access: vec![ArtifactAccessMode::SharedMount],
                accelerator: None,
                max_parallel: 1,
            }],
        })
        .await
        .unwrap();
        RecoveryFixture {
            database,
            cp,
            pool,
            node_id: registered.node.id,
            incarnation_id,
            token: registered.token,
        }
    }

    async fn create_root(fixture: &RecoveryFixture, suffix: &str) -> StorageRootId {
        let library = fixture
            .cp
            .create_library(NewLibrary {
                slug: format!("charter-{suffix}"),
                display_name: format!("Charter {suffix}"),
                media_kind: LibraryMediaKind::Movie,
                description: None,
                enabled: true,
            })
            .await
            .unwrap();
        let root = fixture
            .cp
            .create_library_root(new_root(library.id, fixture.node_id, suffix))
            .await
            .unwrap();
        fixture
            .cp
            .activate_library_root(root.id, format!("charter-{suffix}"))
            .await
            .unwrap();
        root.id
    }

    fn new_root(library_id: LibraryId, owner: NodeId, suffix: &str) -> NewLibraryRoot {
        NewLibraryRoot {
            library_id,
            owner_node_id: owner,
            provider_kind: StorageProviderKind::LocalFilesystem,
            provider_locator: ProviderLocator::new(format!("/charter/{suffix}")).unwrap(),
            display_locator: format!("/charter/{suffix}"),
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            extension_allowlist: Vec::new(),
            scan_mode: LibraryScanMode::ManualRecursive,
            symlink_policy: SymlinkPolicy::Reject,
            hidden_file_policy: HiddenFilePolicy::Ignore,
            max_depth: None,
            stability_seconds: 0,
            debounce_seconds: 0,
            default_output_root_id: None,
            default_staging_root_id: None,
            default_backup_root_id: None,
            enabled: true,
        }
    }

    async fn start(fixture: &RecoveryFixture, id: ScanSessionId, key: &str) {
        fixture
            .cp
            .start_scan_session(RemoteScanStartInput {
                node_id: fixture.node_id,
                scan_session_id: id,
                incarnation_id: fixture.incarnation_id,
                token: fixture.token.clone(),
                idempotency_key: format!("start-{key}"),
                request_hash: format!("start-{key}-body"),
            })
            .await
            .unwrap();
    }
}
