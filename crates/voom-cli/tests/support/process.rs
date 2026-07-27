#![allow(
    dead_code,
    reason = "real-process helpers are shared by selected CLI integration tests"
)]

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use voom_test_support::worker::{target_debug_binary, workspace_root};

const PREBUILT_WORKERS_ENV: &str = "VOOM_TEST_PREBUILT_WORKERS";

pub struct BoundedOutput {
    pub command: String,
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

pub fn run_bounded(command: &mut Command, timeout: Duration) -> io::Result<BoundedOutput> {
    let command_text = format!("{command:?}");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("bounded child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("bounded child stderr was not piped"))?;
    let stdout_reader = drain(stdout);
    let stderr_reader = drain(stderr);
    let (status, timed_out) = wait_until_deadline(&mut child, timeout)?;
    Ok(BoundedOutput {
        command: command_text,
        status,
        stdout: join_drain(stdout_reader, "stdout")?,
        stderr: join_drain(stderr_reader, "stderr")?,
        timed_out,
    })
}

pub fn build_worker_package(package: &str, timeout: Duration) -> io::Result<()> {
    let binary = target_debug_binary(package);
    if std::env::var_os(PREBUILT_WORKERS_ENV).is_some() {
        return binary.is_file().then_some(()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{PREBUILT_WORKERS_ENV} is set but {} is missing",
                    binary.display()
                ),
            )
        });
    }
    let profile_dir = binary
        .parent()
        .ok_or_else(|| io::Error::other("worker binary has no profile directory"))?;
    let target_root = profile_dir
        .parent()
        .ok_or_else(|| io::Error::other("worker profile has no target root"))?;
    let output = run_bounded(
        Command::new("cargo")
            .args(["build", "-p", package, "--all-features"])
            .arg("--target-dir")
            .arg(target_root)
            .current_dir(workspace_root()),
        timeout,
    )?;
    if !output.timed_out && output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(output.diagnostics("worker prebuild")))
}

impl BoundedOutput {
    pub fn diagnostics(&self, what: &str) -> String {
        format!(
            "{what} failed: command={} status={} timed_out={}\nstdout:\n{}\nstderr:\n{}",
            self.command,
            self.status,
            self.timed_out,
            String::from_utf8_lossy(&self.stdout),
            String::from_utf8_lossy(&self.stderr)
        )
    }
}

fn drain(mut reader: impl Read + Send + 'static) -> JoinHandle<io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn wait_until_deadline(child: &mut Child, timeout: Duration) -> io::Result<(ExitStatus, bool)> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            child.kill()?;
            return child.wait().map(|status| (status, true));
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn join_drain(
    reader: JoinHandle<io::Result<Vec<u8>>>,
    stream: &'static str,
) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other(format!("bounded child {stream} drain panicked")))?
}
