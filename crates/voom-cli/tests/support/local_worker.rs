#![allow(
    dead_code,
    reason = "local-worker support is shared by real-process CLI tests"
)]

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

pub struct LocalWorker {
    kind: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<io::Result<String>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    worker_id: u64,
}

impl LocalWorker {
    pub fn spawn(database_url: &str, kind: &'static str) -> io::Result<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_voom"))
            .env("VOOM_DATABASE_URL", database_url)
            .args(["worker", "run-local", "--kind", kind])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("run-local stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("run-local stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("run-local stderr was not piped"))?;
        let stderr = drain_stderr(stderr);
        let stdout_rx = read_stdout_lines(stdout);
        Ok(Self {
            kind,
            child,
            stdin: Some(stdin),
            stdout_rx,
            stderr,
            worker_id: 0,
        })
    }

    pub fn worker_id(&self) -> u64 {
        self.worker_id
    }

    pub fn wait_for_ready(&mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(self.error(format!("exited before ready with {status}")));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill_and_reap();
                return Err(self.error("timed out waiting for readiness"));
            }
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(250)))
            {
                Ok(Ok(line)) if self.accept_ready_line(&line)? => return Ok(()),
                Ok(Ok(_)) | Err(RecvTimeoutError::Timeout) => {}
                Ok(Err(err)) => return Err(self.error(format!("stdout read failed: {err}"))),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.error("stdout closed before readiness"));
                }
            }
        }
    }

    pub fn shutdown(&mut self, timeout: Duration) -> io::Result<Value> {
        drop(self.stdin.take());
        let deadline = Instant::now() + timeout;
        let mut last_envelope = None;
        let status = loop {
            collect_lines(&self.stdout_rx, &mut last_envelope)?;
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill_and_reap();
                return Err(self.error("timed out waiting for shutdown"));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(10)));
        };
        collect_lines(&self.stdout_rx, &mut last_envelope)?;
        if !status.success() {
            return Err(self.error(format!("exited nonzero during shutdown: {status}")));
        }
        last_envelope.ok_or_else(|| self.error("printed no retirement envelope"))
    }

    fn accept_ready_line(&mut self, line: &str) -> io::Result<bool> {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Ok(false);
        };
        if value["status"] != "ready" {
            return Ok(false);
        }
        if value["kind"] != self.kind {
            return Err(self.error(format!("ready kind mismatch: {value}")));
        }
        self.worker_id = value["worker_id"]
            .as_u64()
            .ok_or_else(|| self.error(format!("ready line missing worker_id: {value}")))?;
        Ok(true)
    }

    fn error(&self, message: impl AsRef<str>) -> io::Error {
        io::Error::other(format!(
            "run-local {} {};\nstderr:\n{}",
            self.kind,
            message.as_ref(),
            self.stderr_snapshot()
        ))
    }

    fn stderr_snapshot(&self) -> String {
        let bytes = self
            .stderr
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |buf| buf.clone());
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn kill_and_reap(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for LocalWorker {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.kill_and_reap();
    }
}

fn read_stdout_lines(stdout: impl Read + Send + 'static) -> Receiver<io::Result<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn drain_stderr(mut stderr: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let drain = Arc::clone(&buffer);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        match drain.lock() {
            Ok(mut buffer) => buffer.extend(bytes),
            Err(poisoned) => poisoned.into_inner().extend(bytes),
        }
    });
    buffer
}

fn collect_lines(
    receiver: &Receiver<io::Result<String>>,
    last_envelope: &mut Option<Value>,
) -> io::Result<()> {
    loop {
        match receiver.try_recv() {
            Ok(Ok(line)) => {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    *last_envelope = Some(value);
                }
            }
            Ok(Err(err)) => return Err(err),
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}
