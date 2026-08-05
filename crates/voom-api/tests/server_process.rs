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
