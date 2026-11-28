#![expect(
    clippy::unwrap_used,
    reason = "fixture setup and explicit lock success/error assertions"
)]

use super::*;

#[test]
fn prebuilt_worker_binary_errors_when_the_expected_binary_is_absent() {
    let result = prebuilt_worker_binary("definitely-missing-worker");
    assert!(
        result.is_err(),
        "missing worker unexpectedly resolved to {result:?}"
    );
    let message = result.err().map_or_else(String::new, |err| err.to_string());

    assert!(
        message.contains("prebuilt worker binary missing at"),
        "{message}"
    );
}

#[test]
fn abandoned_legacy_lock_directory_is_usable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FFPROBE_SIBLING_LOCK_FILE);
    let legacy = root.path().join(".voom-ffprobe-sibling.lock");
    fs::create_dir(&legacy).unwrap();
    let guard =
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::from_secs(15)).unwrap();
    drop(guard);
    TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::from_secs(15)).unwrap();
}

#[test]
fn live_holder_excludes_contenders_and_timeout_is_actionable() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("fixture ' lock");
    fs::create_dir(&profile).unwrap();
    let path = profile.join(FFPROBE_SIBLING_LOCK_FILE);
    let _guard =
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::from_secs(15)).unwrap();
    let err = TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    let message = err.to_string();
    assert!(
        message.contains(&format!("{:?}", path.as_os_str())),
        "{message}"
    );
    assert!(message.contains("may be stale"), "{message}");
    assert!(
        message.contains("after stopping all test processes"),
        "{message}"
    );
    assert!(
        message
            .contains("from its profile directory, run: rm -f -- .voom-ffprobe-sibling.lock-owner"),
        "{message}"
    );
}

#[test]
fn lock_io_error_preserves_kind_and_names_path() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("fixture\n.lock");
    fs::write(&profile, b"not a directory").unwrap();
    let path = profile.join(FFPROBE_SIBLING_LOCK_FILE);
    let expected_kind = fs::File::open(&path).unwrap_err().kind();
    let err = TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO).unwrap_err();
    assert_eq!(err.kind(), expected_kind);
    assert!(err.to_string().contains("open lock"));
    assert!(err.to_string().contains(&format!("{:?}", path.as_os_str())));
    assert!(!err.to_string().contains('\n'));
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
            .args([
                "--ignored",
                "--exact",
                "worker::tests::lock_process_helper",
                "--nocapture",
            ])
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
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before {}",
                path.display()
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "subprocess entry point, invoked explicitly by lock tests"]
fn lock_process_helper() {
    let Some(root) = std::env::var_os("VOOM_LOCK_TEST_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let label = std::env::var("VOOM_LOCK_TEST_LABEL").unwrap();
    assert!(
        matches!(label.as_str(), "holder" | "a" | "b"),
        "invalid lock helper role"
    );
    if label != "holder" {
        assert_eq!(
            TargetDebugLock::acquire_at(&root, Duration::ZERO)
                .unwrap_err()
                .kind(),
            ErrorKind::TimedOut
        );
    }
    fs::write(root.join(format!("{label}.started")), b"").unwrap();
    let _guard = TargetDebugLock::acquire_at(&root, Duration::from_secs(15)).unwrap();
    // This independent exclusive marker makes simultaneous owners fail immediately.
    let critical = root.join("critical");
    let _marker = (label != "holder").then(|| fs::File::create_new(&critical).unwrap());
    fs::write(root.join(format!("{label}.acquired")), b"").unwrap();
    let started = Instant::now();
    while !root.join(format!("{label}.release")).exists() {
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    if label != "holder" {
        fs::remove_file(critical).unwrap();
    }
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
    let path = root.path().join(FFPROBE_SIBLING_LOCK_FILE);
    assert_eq!(
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    let started = Instant::now();
    while !root.path().join("a.acquired").exists() && !root.path().join("b.acquired").exists() {
        assert!(a.0.try_wait().unwrap().is_none());
        assert!(b.0.try_wait().unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    let (first, second) = if root.path().join("a.acquired").exists() {
        ("a", "b")
    } else {
        ("b", "a")
    };
    assert!(!root.path().join(format!("{second}.acquired")).exists());
    assert_eq!(
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    fs::write(root.path().join(format!("{first}.release")), b"").unwrap();
    // Only the losing child is required to remain alive while it acquires.
    let second_child = if second == "a" { &mut a } else { &mut b };
    wait_for_path(
        &root.path().join(format!("{second}.acquired")),
        &mut [second_child],
    );
    assert_eq!(
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    fs::write(root.path().join(format!("{second}.release")), b"").unwrap();
    assert!(a.0.wait().unwrap().success());
    assert!(b.0.wait().unwrap().success());
    TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::from_secs(15)).unwrap();
}

#[test]
fn dropping_a_guard_keeps_existing_waiters_on_the_shared_lock_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join(FFPROBE_SIBLING_LOCK_FILE);
    let guard =
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::from_secs(15)).unwrap();
    let waiter = fs::File::options()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(matches!(
        waiter.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    drop(guard);
    // A concurrent fork may retain the former owner's descriptor until exec.
    let started = Instant::now();
    while let Err(err) = waiter.try_lock() {
        assert!(matches!(err, fs::TryLockError::WouldBlock), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        TargetDebugLock::acquire_at(path.parent().unwrap(), Duration::ZERO)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
}

#[cfg(target_os = "linux")]
#[test]
fn cleanup_command_works_inside_a_non_utf8_lock_path() {
    use std::os::unix::ffi::OsStringExt;

    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join(std::ffi::OsString::from_vec(b"fixture run: \xff".to_vec()));
    let other = root.path().join("fixture run: \u{fffd}");
    assert_cleanup_in_lock_directory(&path, &other);
}

#[cfg(unix)]
#[test]
fn cleanup_command_works_with_legal_shell_sensitive_paths() {
    let root = tempfile::tempdir().unwrap();
    assert_cleanup_in_lock_directory(
        &root.path().join("fixture ' \" run: \n$()"),
        &root.path().join("untouched"),
    );
}

#[cfg(unix)]
fn assert_cleanup_in_lock_directory(profile: &Path, other_profile: &Path) {
    fs::create_dir(profile).unwrap();
    fs::create_dir(other_profile).unwrap();
    let path = profile.join(FFPROBE_SIBLING_LOCK_FILE);
    let other = other_profile.join(FFPROBE_SIBLING_LOCK_FILE);
    fs::write(&other, b"untouched").unwrap();
    let guard = TargetDebugLock::acquire_at(profile, Duration::from_secs(15)).unwrap();
    let message = TargetDebugLock::acquire_at(profile, Duration::ZERO)
        .unwrap_err()
        .to_string();
    assert!(message.contains(&format!("{:?}", path.as_os_str())));
    let _other_guard = TargetDebugLock::acquire_at(other_profile, Duration::from_secs(15)).unwrap();
    let other_message = TargetDebugLock::acquire_at(other_profile, Duration::ZERO)
        .unwrap_err()
        .to_string();
    assert_ne!(
        message, other_message,
        "cleanup must identify the original path without losing bytes"
    );
    assert!(
        message.ends_with(
            "from its profile directory, run: rm -f -- .voom-ffprobe-sibling.lock-owner"
        )
    );
    drop(guard);
    assert!(
        Command::new("rm")
            .args(["-f", "--", FFPROBE_SIBLING_LOCK_FILE])
            .current_dir(profile)
            .status()
            .unwrap()
            .success()
    );
    assert!(!path.exists());
    assert_eq!(fs::read(other).unwrap(), b"untouched");
}

#[cfg(unix)]
#[test]
fn owner_file_does_not_grant_other_accounts_lock_access() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("holder.release"), b"").unwrap();
    // Set the child's umask without changing the parallel test runner's umask.
    let status = Command::new("sh")
        .args(["-c", "umask 022; exec \"$@\"", "voom-lock-permissions"])
        .arg(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "worker::tests::lock_process_helper"])
        .env("VOOM_LOCK_TEST_ROOT", root.path())
        .env("VOOM_LOCK_TEST_LABEL", "holder")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let mode = fs::metadata(root.path().join(FFPROBE_SIBLING_LOCK_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o077,
        0,
        "other accounts must not be able to hold this lock"
    );
}

#[test]
fn ambient_helper_settings_cannot_activate_the_subprocess_entry() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("holder.release"), b"").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "worker::tests::lock_process_helper"])
        .env("VOOM_LOCK_TEST_ROOT", root.path())
        .env("VOOM_LOCK_TEST_LABEL", "holder")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!root.path().join("holder.started").exists());
    let invalid = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "worker::tests::lock_process_helper",
            "--nocapture",
        ])
        .env("VOOM_LOCK_TEST_ROOT", root.path())
        .env("VOOM_LOCK_TEST_LABEL", "../escape")
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stdout).contains("invalid lock helper role")
            || String::from_utf8_lossy(&invalid.stderr).contains("invalid lock helper role")
    );
}

#[cfg(unix)]
#[test]
fn writable_legacy_lock_directory_cannot_create_an_external_file() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join(".voom-ffprobe-sibling.lock");
    fs::create_dir(&legacy).unwrap();
    fs::set_permissions(&legacy, fs::Permissions::from_mode(0o777)).unwrap();
    let external = root.path().join("unexpected");
    symlink(&external, legacy.join("owner")).unwrap();
    let path = root.path().join(FFPROBE_SIBLING_LOCK_FILE);
    let guard = TargetDebugLock::acquire_at(root.path(), Duration::from_secs(15)).unwrap();
    assert!(
        !external.exists(),
        "legacy owner symlinks must not create external files"
    );
    assert!(
        path.is_file(),
        "the shared selector must create the flat sibling"
    );
    assert!(
        fs::symlink_metadata(legacy.join("owner"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::metadata(&legacy).unwrap().permissions().mode() & 0o777,
        0o777
    );
    assert_eq!(
        TargetDebugLock::acquire_at(root.path(), Duration::ZERO)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    drop(guard);
    // Wait for any pre-exec inherited descriptor rather than assuming immediate release.
    TargetDebugLock::acquire_at(root.path(), Duration::from_secs(15)).unwrap();
    assert!(!external.exists());
}
