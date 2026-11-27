# #483 implementation plan

Expected implementation size: 190–300 changed lines (M).
File map: worker.rs replaces approximately 35 lines with approximately 70;
worker_test.rs adds approximately 170 lines. Design denominator remains 250 from
WORK:SCOPE q483-0bf14dc7. BASE_BRANCH main; branch fix/483-crash-safe-fixture-lock.
Guardrails: focused cargo test, just fmt-check, just lint, just ci, mandatory hooks.
Deferrals: hidden sibling restoration after death is excluded, owner #501.

## Task 1 — bounded acquisition seam and regression proof

Modify only `crates/voom-test-support/src/worker.rs` and `worker_test.rs`.
Interfaces: preserve `TargetDebugLock::acquire(name: &str) -> io::Result<Self>`;
add private `acquire_at(path: &Path, timeout: Duration) -> io::Result<Self>`.
Public `hide_stale_fake_ffprobe_sibling` and `install_fake_ffprobe_sibling`
remain the direct callers and require no migration.

Verification inventory (each Mode: focused-test):
- C1 legacy orphan and process-death recovery: sibling tests must fail against
  create_dir locking with TimedOut, then succeed with file locking.
- C2 live holder, simultaneous waiters and post-Drop inode stability: sibling
  tests must detect a second owner or poisoned lock if exclusivity/release breaks.
- C3 timeout message and I/O errors: sibling tests assert ErrorKind, path,
  stale explanation, stop-all-processes condition and safe relative command.
- C4 process-level boundary: subprocess helper in sibling unit suite exercises
  independent opens; fixtures use unique temporary paths with bounded handshakes.
Red/green command for every entry: `cargo test -p voom-test-support worker::tests`.
Expected red: orphan recovery and diagnostic assertions fail; expected green: all pass.

First extract the existing acquisition loop into acquire_at with no behavior change;
parameterize the duration and preserve create_dir/Drop semantics. Write sibling tests
before replacing ownership. Spawn the current unit-test executable with an exact helper
test filter, child-specific environment and a temporary root; do not modify parent env.
The helper waits for the kernel lock, signals acquired via an exclusive marker,
and holds until a release file appears. Each surviving waiter first observes a
zero-timeout TimedOut attempt while the initial holder is alive, then signals ready
and retries. Only surviving waiters create the overlap marker, so killing the initial
holder cannot poison the marker. A parent-owned Child wrapper kills/reaps on Drop.
Parent polling has a deadline and checks premature child exit. Kill/reap the first
holder with two waiters already started, then release the winning waiter and observe
second acquisition. A shared create_new marker inside the critical section detects
any overlap independently from the lock implementation. Also test normal Drop by
reacquiring with an already-open waiter handle, without unlinking the owner file.

## Task 2 — replace directory ownership with a kernel-held file

Consume the exact private acquisition seam from Task 1. Change only worker.rs.
Verification inventory: Mode: focused-test; C1–C3 tests from Task 1; same command.
Use this complete replacement, preserving sibling guard cleanup ordering:

```rust
#[derive(Debug)]
struct TargetDebugLock {
    _file: fs::File,
}

impl TargetDebugLock {
    fn acquire(name: &str) -> io::Result<Self> {
        let path = target_debug_dir().join(format!(".{name}.lock"));
        Self::acquire_at(&path, Duration::from_mins(2))
    }

    fn acquire_at(path: &Path, timeout: Duration) -> io::Result<Self> {
        fs::create_dir_all(path).map_err(|err| {
            io::Error::new(err.kind(), format!("create lock directory {}: {err}", path.display()))
        })?;
        let file = fs::File::options().read(true).write(true).create(true).truncate(false)
            .open(path.join("owner")).map_err(|err| {
                io::Error::new(err.kind(), format!("open lock {}: {err}", path.display()))
            })?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(fs::TryLockError::WouldBlock) => {
                    if started.elapsed() >= timeout {
                        return Err(io::Error::new(ErrorKind::TimedOut,
                            format!("timed out waiting for {}; lock state may be stale; after stopping all test processes, from inside that lock directory, run: rm -f -- owner", path.display())));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(fs::TryLockError::Error(err)) => return Err(io::Error::new(
                    err.kind(), format!("acquire lock {}: {err}", path.display()))),
            }
        }
    }
}
```

Remove TargetDebugLock's directory-removing Drop implementation. Add a comment explaining
persistent identity and death cleanup; no owner metadata or new dependency. Run the focused
tests, repeat process tests, just fmt and just lint. Expected all green, zero warnings.
Rollback: stop all old/new test processes, revert implementation/test commits, then
from each affected profile run `rm -f -- .voom-ffprobe-sibling.lock/owner` followed by
`rmdir -- .voom-ffprobe-sibling.lock` before restarting legacy binaries. Never delete lock files while tests run. Run assembled and final just ci,
retain mandatory pre-commit checks, then branch review/security/simplification/publication.

## Task 1 sibling test code

Append this complete test code after the existing sibling test:

```rust

#[test]
fn abandoned_legacy_lock_directory_is_usable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.lock");
    fs::create_dir(&path).unwrap();
    let guard = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
    drop(guard);
    TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
}

#[test]
fn live_holder_excludes_contenders_and_timeout_is_actionable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture ' lock");
    let _guard = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
    let err = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    let message = err.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(message.contains("may be stale"), "{message}");
    assert!(message.contains("after stopping all test processes"), "{message}");
    assert!(message.contains("from inside that lock directory, run: rm -f -- owner"), "{message}");
}

#[test]
fn lock_io_error_preserves_kind_and_names_path() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.lock");
    fs::write(&path, b"not a directory").unwrap();
    let err = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err();
    assert_ne!(err.kind(), ErrorKind::TimedOut);
    assert!(err.to_string().contains(&path.display().to_string()));
}

struct LockChild(Child);

impl Drop for LockChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_lock_child(root: &Path, label: &str) -> LockChild {
    LockChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "worker::tests::lock_process_helper", "--nocapture"])
            .env("VOOM_LOCK_TEST_ROOT", root)
            .env("VOOM_LOCK_TEST_LABEL", label)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}

fn wait_for_path(path: &Path, children: &mut [&mut LockChild]) {
    let started = Instant::now();
    while !path.exists() {
        for child in children.iter_mut() {
            assert!(child.0.try_wait().unwrap().is_none(), "child exited before {}", path.display());
        }
        assert!(started.elapsed() < Duration::from_secs(15), "waiting for {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn lock_process_helper() {
    let Some(root) = std::env::var_os("VOOM_LOCK_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let label = std::env::var("VOOM_LOCK_TEST_LABEL").unwrap();
    if label != "holder" {
        assert_eq!(TargetDebugLock::acquire_at(&root.join("fixture.lock"), Duration::ZERO).unwrap_err().kind(), ErrorKind::TimedOut);
    }
    fs::write(root.join(format!("{label}.started")), b"").unwrap();
    let _guard = TargetDebugLock::acquire_at(&root.join("fixture.lock"), Duration::from_secs(15)).unwrap();
    // This independent exclusive marker makes simultaneous owners fail immediately.
    let critical = root.join("critical");
    let _marker = (label != "holder").then(|| fs::File::create_new(&critical).unwrap());
    fs::write(root.join(format!("{label}.acquired")), b"").unwrap();
    let started = Instant::now();
    while !root.join(format!("{label}.release")).exists() {
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    if label != "holder" { fs::remove_file(critical).unwrap(); }
}

#[test]
fn dead_holder_releases_lock_and_concurrent_waiters_serialize() {
    let root = tempfile::tempdir().unwrap();
    let mut holder = spawn_lock_child(root.path(), "holder");
    wait_for_path(&root.path().join("holder.acquired"), &mut [&mut holder]);
    let mut a = spawn_lock_child(root.path(), "a");
    let mut b = spawn_lock_child(root.path(), "b");
    wait_for_path(&root.path().join("a.started"), &mut [&mut a, &mut b]);
    wait_for_path(&root.path().join("b.started"), &mut [&mut a, &mut b]);
    let path = root.path().join("fixture.lock");
    assert_eq!(TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err().kind(), ErrorKind::TimedOut);
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let started = Instant::now();
    while !root.path().join("a.acquired").exists() && !root.path().join("b.acquired").exists() {
        assert!(a.0.try_wait().unwrap().is_none());
        assert!(b.0.try_wait().unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    let (first, second) = if root.path().join("a.acquired").exists() { ("a", "b") } else { ("b", "a") };
    assert!(!root.path().join(format!("{second}.acquired")).exists());
    assert_eq!(TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err().kind(), ErrorKind::TimedOut);
    fs::write(root.path().join(format!("{first}.release")), b"").unwrap();
    // Only the losing child is required to remain alive while it acquires.
    let second_child = if second == "a" { &mut a } else { &mut b };
    wait_for_path(&root.path().join(format!("{second}.acquired")), &mut [second_child]);
    assert_eq!(TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err().kind(), ErrorKind::TimedOut);
    fs::write(root.path().join(format!("{second}.release")), b"").unwrap();
    assert!(a.0.wait().unwrap().success());
    assert!(b.0.wait().unwrap().success());
    TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
}

#[test]
fn dropping_a_guard_keeps_existing_waiters_on_the_shared_lock_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.lock");
    let guard = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
    let waiter = fs::File::options().read(true).write(true).open(path.join("owner")).unwrap();
    assert!(matches!(waiter.try_lock(), Err(fs::TryLockError::WouldBlock)));
    drop(guard);
    waiter.try_lock().unwrap();
    assert_eq!(TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err().kind(), ErrorKind::TimedOut);
}

#[cfg(unix)]
#[test]
fn cleanup_command_works_inside_a_non_utf8_lock_path() {
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(std::ffi::OsString::from_vec(b"fixture\xff".to_vec()));
    let other = root.path().join("fixture\u{fffd}");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("owner"), b"untouched").unwrap();
    let guard = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap();
    let message = TargetDebugLock::acquire_at(&path, Duration::ZERO).unwrap_err().to_string();
    let command = message.split_once("run: ").unwrap().1;
    drop(guard);
    assert!(Command::new("sh").args(["-c", command]).current_dir(&path).status().unwrap().success());
    assert!(!path.join("owner").exists());
    assert_eq!(fs::read(other.join("owner")).unwrap(), b"untouched");
}
```
