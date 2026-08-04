use std::{
    ffi::OsStr,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use voom_worker_protocol::VIDEOTOOLBOX_PROBE_TIMEOUT;

use super::FfmpegPreflightError;

pub(super) const PROBE_TIMEOUT: Duration = VIDEOTOOLBOX_PROBE_TIMEOUT;

pub(super) fn resolve_binary(binary: &OsStr) -> PathBuf {
    let path = PathBuf::from(binary);
    if path.components().count() > 1 {
        return path;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return path;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(&path);
        if is_executable_file(&candidate) {
            return candidate;
        }
    }
    path
}

pub(super) fn require_executable_file(
    label: &str,
    path: &Path,
) -> Result<(), FfmpegPreflightError> {
    if !is_executable_file(path) {
        return Err(FfmpegPreflightError::Failed(format!(
            "{label} binary is missing or not executable: {}",
            path.display()
        )));
    }
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    is_executable_metadata(&metadata)
}

#[cfg(unix)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_metadata(_metadata: &std::fs::Metadata) -> bool {
    true
}

pub(super) fn first_output_line(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FfmpegPreflightError> {
    command_text(command_name, output)?
        .lines()
        .next()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| FfmpegPreflightError::Failed(format!("{command_name} produced no output")))
}

pub(super) fn command_text(
    command_name: &str,
    output: std::io::Result<std::process::Output>,
) -> Result<String, FfmpegPreflightError> {
    let output = output.map_err(|err| {
        FfmpegPreflightError::Failed(format!("{command_name} failed to start: {err}"))
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}{stderr}");
    if output.status.success() {
        Ok(text)
    } else {
        Err(FfmpegPreflightError::Failed(format!(
            "{command_name} exited with status {}: {}",
            output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            text.trim()
        )))
    }
}

pub(super) fn command_output(command: &mut Command) -> io::Result<Output> {
    command_output_within(command, PROBE_TIMEOUT)
}

pub(super) fn wait_child_output(
    child: Child,
    deadline: Duration,
    label: &str,
) -> Result<Output, FfmpegPreflightError> {
    wait_child_output_io(child, deadline, label).map_err(|error| {
        FfmpegPreflightError::Failed(format!("{label} failed while waiting: {error}"))
    })
}

/// Waits for `child` under `deadline`, draining its piped output the whole time.
///
/// The drain threads are not an optimization: a child that writes more than the OS
/// pipe capacity blocks in `write()` until someone reads, so polling `try_wait()`
/// against an undrained pipe can never see it exit.
fn wait_child_output_io(mut child: Child, deadline: Duration, label: &str) -> io::Result<Output> {
    let stdout = drain_in_background(child.stdout.take());
    let stderr = drain_in_background(child.stderr.take());
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                status,
                stdout: join_drained(stdout)?,
                stderr: join_drained(stderr)?,
            });
        }
        if started.elapsed() >= deadline {
            kill_and_reap(&mut child);
            // Deliberately detached, not joined. Killing the child does not close a
            // pipe that a surviving grandchild still holds, so joining here would
            // block for exactly as long as the deadline exists to prevent. The
            // threads exit on their own once the last writer does.
            drop(stdout);
            drop(stderr);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{label} exceeded {} seconds", deadline.as_secs()),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn kill_and_reap_all(children: &mut Vec<Child>) {
    for child in &mut *children {
        kill_and_reap(child);
    }
    children.clear();
}

fn is_text_file_busy(err: &io::Error) -> bool {
    err.raw_os_error() == Some(26)
}

pub(super) fn parse_token(text: &str, token: &str) -> Option<String> {
    text.lines()
        .find(|line| line.split_whitespace().any(|candidate| candidate == token))
        .map(|_| token.to_owned())
}

pub(super) struct ProbeDir {
    path: PathBuf,
}

static PROBE_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl ProbeDir {
    pub(super) fn new(label: &str) -> Result<Self, FfmpegPreflightError> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| {
                FfmpegPreflightError::Failed(format!(
                    "system clock before Unix epoch during {label}: {error}"
                ))
            })?
            .as_nanos();
        let sequence = PROBE_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "voom-{label}-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).map_err(|error| {
            FfmpegPreflightError::Failed(format!(
                "create {label} directory {}: {error}",
                path.display()
            ))
        })?;
        set_private_directory_permissions(&path, label)?;
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path, label: &str) -> Result<(), FfmpegPreflightError> {
    use std::os::unix::fs::PermissionsExt;

    let permissions = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, permissions).map_err(|error| {
        FfmpegPreflightError::Failed(format!(
            "secure {label} directory {}: {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(
    _path: &Path,
    _label: &str,
) -> Result<(), FfmpegPreflightError> {
    Ok(())
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// [`command_output`] under an explicit deadline, so ADR 0052 §7's per-probe clock
/// is a value a test can shorten rather than a constant it has to wait out.
pub(super) fn command_output_within(
    command: &mut Command,
    deadline: Duration,
) -> io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut attempts_remaining = 3;
    loop {
        attempts_remaining -= 1;
        match command
            .spawn()
            .and_then(|child| wait_child_output_io(child, deadline, "dependency probe"))
        {
            Err(err) if is_text_file_busy(&err) && attempts_remaining > 0 => {
                thread::sleep(Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

type DrainHandle = Option<thread::JoinHandle<io::Result<Vec<u8>>>>;

fn drain_in_background<R>(stream: Option<R>) -> DrainHandle
where
    R: Read + Send + 'static,
{
    stream.map(|mut stream| {
        thread::spawn(move || {
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).map(|_| buffer)
        })
    })
}

fn join_drained(handle: DrainHandle) -> io::Result<Vec<u8>> {
    match handle {
        None => Ok(Vec::new()),
        Some(handle) => handle
            .join()
            .map_err(|_| io::Error::other("output drain thread panicked"))?,
    }
}

#[cfg(test)]
#[path = "process_test.rs"]
mod tests;
